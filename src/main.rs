use axum::{
    body::Body,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::any,
    Router,
};
use axum::http::HeaderValue;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tracing::{error, info};
use uuid::Uuid;
mod source;
mod target;
use source::{route_request, ResponseMode, TargetModel};
use source::v1::response::{
    model_retrieve_to_openai_json, models_list_to_openai_json, openai_error_body,
    sse_to_response_json, upstream_error_to_openai,
};
use target::codex::auth::PendingOAuth;
use target::codex::quota::QuotaCacheEntry;
use target::codex::tokens::UpstreamToken;

#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
    rr: Arc<Mutex<usize>>,
    client: reqwest::Client,
    tokens: Arc<Mutex<Vec<UpstreamToken>>>,
    stats: Arc<Mutex<UsageStats>>,
    quota_cache: Arc<Mutex<Vec<Option<QuotaCacheEntry>>>>,
    oauth_pending: Arc<Mutex<HashMap<String, PendingOAuth>>>,
    disabled: Arc<Mutex<HashSet<String>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceApi {
    V1,
    Codex,
    Claude,
}

#[derive(Debug, Deserialize)]
struct Config {
    // Proxy listen port
    listen: String,
    // Upstream base url, e.g. https://chatgpt.com/backend-api/codex
    upstream_base: String,
    // One shared API key used by your Codex CLI (proxy client)
    proxy_api_key: String,
    // List of codex account access tokens (or API keys) to rotate
    tokens: Vec<String>,
    // Optional directory containing Codex credential json files
    auth_dir: Option<String>,
    // Optional list of disabled credential filenames
    disabled_files: Option<Vec<String>>,
}


#[derive(Default, Clone, Serialize)]
struct UsageStats {
    per_account: Vec<AccountUsage>,
    total_requests: u64,
    total_errors: u64,
}

#[derive(Default, Clone, Serialize)]
struct AccountUsage {
    label: String,
    account_id: String,
    requests: u64,
    errors: u64,
}


#[tokio::main]
async fn main() {
    let cfg = load_config();
    let disabled = cfg
        .disabled_files
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let tokens = target::codex::tokens::load_tokens(&cfg, &disabled);
    let stats = UsageStats {
        per_account: tokens
            .iter()
            .map(|t| AccountUsage {
                label: t.label.clone(),
                account_id: t.account_id.clone().unwrap_or_default(),
                requests: 0,
                errors: 0,
            })
            .collect(),
        total_requests: 0,
        total_errors: 0,
    };
    let quota_cache = vec![None; tokens.len()];
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let client = reqwest::Client::builder()
        .http1_only()
        .tcp_keepalive(Duration::from_secs(60))
        .pool_idle_timeout(None)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .unwrap();

    let state = AppState {
        cfg: Arc::new(cfg),
        rr: Arc::new(Mutex::new(0)),
        client,
        tokens: Arc::new(Mutex::new(tokens)),
        stats: Arc::new(Mutex::new(stats)),
        quota_cache: Arc::new(Mutex::new(quota_cache)),
        oauth_pending: Arc::new(Mutex::new(HashMap::new())),
        disabled: Arc::new(Mutex::new(disabled)),
    };

    let app = Router::new()
        .route("/health", any(health))
        .route("/", any(dashboard))
        .route("/dashboard", any(dashboard))
        .route("/dashboard.json", any(dashboard_json))
        .route("/quota.json", any(target::codex::admin::quota_json))
        .route("/credentials/delete", any(target::codex::admin::delete_credential))
        .route("/credentials/toggle", any(target::codex::admin::toggle_credential))
        .route("/login/codex/start", any(target::codex::admin::login_start))
        .route("/login/codex/submit", any(target::codex::admin::login_submit))
        .route("/*path", any(proxy))
        .with_state(state.clone());

    let addr: SocketAddr = state
        .cfg
        .listen
        .parse()
        .expect("invalid listen address");
    info!("listening on {}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await.unwrap(), app)
        .await
        .unwrap();
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn dashboard(State(_state): State<AppState>) -> impl IntoResponse {
    let html = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Codex Gateway Dashboard</title>
    <style>
      :root { color-scheme: light; }
      body { font-family: Arial, sans-serif; margin: 24px; font-size: 16px; }
      h1 { margin: 0 0 12px 0; }
      table { border-collapse: collapse; width: 100%; }
      th, td { border: 1px solid #ddd; padding: 8px; text-align: left; }
      th { background: #f2f2f2; }
      .muted { color: #666; font-size: 12px; }
      .stacked { line-height: 1.35; white-space: nowrap; }
      .weekly-line { font-size: calc(1em - 4px); }
      input, button { font-size: 14px; }
      .tap-tip {
        position: absolute;
        background: #111827;
        color: #fff;
        border-radius: 6px;
        padding: 6px 8px;
        font-size: 12px;
        line-height: 1.25;
        white-space: nowrap;
        z-index: 9999;
        box-shadow: 0 4px 12px rgba(0,0,0,0.25);
      }
      @media (max-width: 768px) {
        body { margin: 12px; font-size: 17px; }
        h1 { font-size: 22px; }
        th, td { padding: 10px; font-size: 15px; }
        .muted { font-size: 13px; }
        input, button { font-size: 16px; }
      }
    </style>
  </head>
  <body>
    <h1 style="display:flex;align-items:center;justify-content:space-between;gap:12px;">
      <span>Codex Gateway Usage</span>
      <button id="addAccountBtn">Add account</button>
    </h1>
    <div id="totals" class="muted"></div>
    <div style="overflow-x:auto;">
      <table>
      <thead>
        <tr>
          <th>Account</th>
          <th>Code Gen</th>
          <th>Code Review</th>
          <th>Expired At</th>
        </tr>
      </thead>
      <tbody id="rows"></tbody>
      </table>
    </div>
    <script>
      let lastQuota = new Map();
      let activeTipEl = null;
      let activeTipTimer = null;
      function showTapTip(el, ev) {
        if (ev) {
          ev.preventDefault();
          ev.stopPropagation();
        }
        const text = el.getAttribute('data-tip') || el.getAttribute('title') || '';
        if (!text) return;
        if (activeTipEl) {
          activeTipEl.remove();
          activeTipEl = null;
        }
        const tip = document.createElement('div');
        tip.className = 'tap-tip';
        tip.textContent = text;
        document.body.appendChild(tip);
        const rect = el.getBoundingClientRect();
        const margin = 8;
        let left = rect.left + (rect.width / 2) - (tip.offsetWidth / 2);
        left = Math.max(margin, Math.min(left, window.innerWidth - tip.offsetWidth - margin));
        let top = rect.bottom + 8;
        if (top + tip.offsetHeight > window.innerHeight - margin) {
          top = rect.top - tip.offsetHeight - 8;
        }
        tip.style.left = (left + window.scrollX) + 'px';
        tip.style.top = (top + window.scrollY) + 'px';
        activeTipEl = tip;
        if (activeTipTimer) clearTimeout(activeTipTimer);
        activeTipTimer = setTimeout(() => {
          if (activeTipEl) {
            activeTipEl.remove();
            activeTipEl = null;
          }
        }, 2500);
      }
      document.addEventListener('click', () => {
        if (activeTipEl) {
          activeTipEl.remove();
          activeTipEl = null;
        }
      });
      async function refresh() {
        const res = await fetch('/dashboard.json');
        const data = await res.json();
        document.getElementById('totals').textContent =
          'Total requests: ' + data.total_requests + ' | Total errors: ' + data.total_errors;
        const rows = data.accounts.map(a => {
          const toggleLabel = a.enabled ? 'Disable' : 'Enable';
          const dot = a.enabled ? '#2ecc71' : '#e74c3c';
          const key = a.file_name || a.label;
          const toggleControl = a.file_name
            ? `<button title="${toggleLabel}" onclick="toggleCred('${a.file_name}', ${a.enabled ? 'false' : 'true'})" style="display:inline-block;width:10px;height:10px;border-radius:50%;background:${dot};border:none;padding:0;cursor:pointer;"></button>`
            : `<span style="display:inline-block;width:10px;height:10px;border-radius:50%;background:${dot};"></span>`;
          const deleteControl = a.file_name
            ? `<button title="Delete" onclick="deleteCred('${a.file_name}')" style="border:none;background:transparent;cursor:pointer;padding:0 0 0 4px;line-height:1;">&#128465;</button>`
            : '';
          const expiredAt = a.expired_at || '-';
          const label = `<span style="display:flex;align-items:center;gap:6px;width:100%;">${toggleControl}${deleteControl}<span data-tip="Account ID: ${a.account_id || ''} | Expired at: ${expiredAt}" title="Account ID: ${a.account_id || ''} | Expired at: ${expiredAt}" onclick="showTapTip(this, event)" style="cursor:help;">${a.label}</span><span style="margin-left:auto;color:#666;font-size:12px;">(${a.requests}/${a.errors})</span></span>`;
          const q = lastQuota.get(key);
          const qcg5 = q?.code_generation?.five_hour;
          const qcgw = q?.code_generation?.weekly;
          const qcr5 = q?.code_review?.five_hour;
          const qcrw = q?.code_review?.weekly;
          const fmt = (x) => x && x.used_percent !== null && x.used_percent !== undefined
            ? (x.used_percent.toFixed(1) + '% ' + (x.reset_label ? '(' + x.reset_label + ')' : ''))
            : '…';
          return '<tr>' +
            '<td>' + label + '</td>' +
            '<td class="stacked">5h: <span data-q="cg5" data-key="' + key + '">' + fmt(qcg5) + '</span><br><span class="weekly-line">Weekly: <span data-q="cgw" data-key="' + key + '">' + fmt(qcgw) + '</span></span></td>' +
            '<td class="stacked">5h: <span data-q="cr5" data-key="' + key + '">' + fmt(qcr5) + '</span><br><span class="weekly-line">Weekly: <span data-q="crw" data-key="' + key + '">' + fmt(qcrw) + '</span></span></td>' +
            '<td>' + expiredAt + '</td>' +
          '</tr>';
        }).join('');
        document.getElementById('rows').innerHTML = rows;
      }
      async function refreshQuota() {
        const res = await fetch('/quota.json');
        const quota = await res.json();
        const quotaMap = new Map();
        (quota.accounts || []).forEach(q => {
          const key = q.file_name || q.label;
          quotaMap.set(key, q);
        });
        lastQuota = quotaMap;
        const fmt = (x) => x && x.used_percent !== null && x.used_percent !== undefined
          ? (x.used_percent.toFixed(1) + '% ' + (x.reset_label ? '(' + x.reset_label + ')' : ''))
          : '0%';
        document.querySelectorAll('[data-q]').forEach(td => {
          const key = td.getAttribute('data-key');
          const kind = td.getAttribute('data-q');
          const row = quotaMap.get(key);
          if (!row || row.error) {
            td.textContent = '0%';
            return;
          }
          const cg5 = row.code_generation?.five_hour || {};
          const cgw = row.code_generation?.weekly || {};
          const cr5 = row.code_review?.five_hour || {};
          const crw = row.code_review?.weekly || {};
          if (kind === 'cg5') td.textContent = fmt(cg5);
          if (kind === 'cgw') td.textContent = fmt(cgw);
          if (kind === 'cr5') td.textContent = fmt(cr5);
          if (kind === 'crw') td.textContent = fmt(crw);
        });
      }
      refresh();
      refreshQuota();
      setInterval(refresh, 5000);
      setInterval(refreshQuota, 60000);
    </script>
    <div id="addModal" style="display:none;position:fixed;inset:0;background:rgba(0,0,0,0.45);">
      <div style="background:#fff;max-width:720px;margin:8% auto;padding:16px;border-radius:8px;max-height:80vh;overflow:auto;">
        <h2 style="margin-top:0;">Add Codex Account</h2>
        <p>Click start, open the URL in a new tab, complete login, then paste the callback URL below.</p>
        <button onclick="startLogin()">Start Login</button>
        <div id="status" class="muted" style="margin-top:8px;"></div>
        <pre id="authUrl" style="display:none;white-space:pre-wrap;word-break:break-all;overflow-wrap:anywhere;"></pre>
        <form id="loginForm" style="margin-top:16px;">
          <label>Callback URL</label>
          <input name="redirect_url" placeholder="http://localhost:1455/auth/callback?code=...&state=...">
          <button type="submit" style="margin-top:8px;">Submit</button>
          <button type="button" id="closeModalBtn" style="margin-top:8px;margin-left:8px;">Close</button>
        </form>
      </div>
    </div>
    <script>
      async function startLogin() {
        const res = await fetch('/login/codex/start');
        const data = await res.json();
        if (data.url) {
          window.open(data.url, '_blank');
          document.getElementById('status').textContent = 'Opened login URL in new tab. If blocked, copy from below.';
          const pre = document.getElementById('authUrl');
          pre.textContent = data.url;
          pre.style.display = 'block';
        } else {
          document.getElementById('status').textContent = 'Failed to start login';
        }
      }
      document.getElementById('addAccountBtn').addEventListener('click', () => {
        document.getElementById('addModal').style.display = 'block';
      });
      document.getElementById('closeModalBtn').addEventListener('click', () => {
        document.getElementById('addModal').style.display = 'none';
      });
      document.getElementById('addModal').addEventListener('click', (e) => {
        if (e.target.id === 'addModal') {
          document.getElementById('addModal').style.display = 'none';
        }
      });
      document.getElementById('loginForm').addEventListener('submit', async (e) => {
        e.preventDefault();
        const form = e.target;
        const input = form.querySelector('input[name="redirect_url"]');
        const redirectUrl = input.value.trim();
        if (!redirectUrl) {
          document.getElementById('status').textContent = 'Callback URL is required.';
          return;
        }
        const res = await fetch('/login/codex/submit', {
          method: 'POST',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({ redirect_url: redirectUrl })
        });
        const data = await res.json();
        document.getElementById('status').textContent = data.message || 'Login completed.';
        if (data.ok) {
          refresh();
        }
      });
      async function deleteCred(fileName) {
        const key = prompt('Proxy API key for delete:');
        if (!key) return;
        const res = await fetch('/credentials/delete', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/x-www-form-urlencoded',
            'Authorization': 'Bearer ' + key
          },
          body: new URLSearchParams({ file_name: fileName })
        });
        const data = await res.json();
        alert(data.message || 'done');
        refresh();
        refreshQuota();
      }
      async function toggleCred(fileName, enabled) {
        const key = prompt('Proxy API key for toggle:');
        if (!key) return;
        const res = await fetch('/credentials/toggle', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/x-www-form-urlencoded',
            'Authorization': 'Bearer ' + key
          },
          body: new URLSearchParams({ file_name: fileName, enabled: enabled ? 'true' : 'false' })
        });
        const data = await res.json();
        alert(data.message || 'done');
        refresh();
        refreshQuota();
      }
    </script>
  </body>
</html>
"#;
    (
        StatusCode::OK,
        [
            ("Content-Type", "text/html"),
            ("Cache-Control", "no-store"),
            ("Pragma", "no-cache"),
        ],
        html,
    )
        .into_response()
}

async fn dashboard_json(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = {
        let stats = state.stats.lock().unwrap();
        stats.clone()
    };
    let accounts: Vec<serde_json::Value> = snapshot
        .per_account
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            let file_name = {
                let tokens = state.tokens.lock().unwrap();
                tokens.get(i).and_then(|t| t.file_name.clone())
            };
            let enabled = {
                let tokens = state.tokens.lock().unwrap();
                tokens.get(i).map(|t| t.enabled).unwrap_or(false)
            };
            let expired_at = {
                let tokens = state.tokens.lock().unwrap();
                tokens.get(i).and_then(|t| t.expired_at.clone())
            };
            serde_json::json!({
                "label": a.label,
                "account_id": a.account_id,
                "requests": a.requests,
                "errors": a.errors,
                "file_name": file_name,
                "enabled": enabled,
                "expired_at": expired_at
            })
        })
        .collect();
    axum::Json(serde_json::json!({
        "total_requests": snapshot.total_requests,
        "total_errors": snapshot.total_errors,
        "accounts": accounts
    }))
}

async fn proxy(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    body: Body,
) -> impl IntoResponse {
    let raw_path = uri.path().to_string();
    let source_api = detect_source_api(&raw_path);

    // Simple API key guard
    if !check_api_key(&headers, &state.cfg.proxy_api_key) {
        return if matches!(source_api, SourceApi::V1) {
            (
                StatusCode::UNAUTHORIZED,
                [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                openai_error_body(
                    "Missing bearer authentication in header",
                    "invalid_request_error",
                    None,
                ),
            )
                .into_response()
        } else {
            (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
        };
    }

    // Read full body (small/simple proxy)
    let body_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => {
            return if matches!(source_api, SourceApi::V1) {
                (
                    StatusCode::BAD_REQUEST,
                    [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                    openai_error_body("Invalid request body", "invalid_request_error", None),
                )
                    .into_response()
            } else {
                (StatusCode::BAD_REQUEST, "invalid body").into_response()
            };
        }
    };

    let routed = match route_request(&raw_path, &uri, &method, &headers, body_bytes) {
        Ok(r) => r,
        Err(e) => {
            return if matches!(source_api, SourceApi::V1) {
                (
                    e.status,
                    [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                    openai_error_body(e.message, "invalid_request_error", None),
                )
                    .into_response()
            } else {
                (e.status, e.message).into_response()
            };
        }
    };
    let upstream = match routed.target {
        TargetModel::Codex => target::codex::gateway::build_upstream_url(
            &state.cfg.upstream_base,
            &routed.upstream_path,
            routed.upstream_query.as_deref(),
        ),
    };
    let session_id = Uuid::new_v4().to_string();

    let picked = pick_token(&state);
    if picked.is_none() {
        return if matches!(source_api, SourceApi::V1) {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                openai_error_body("No upstream credentials configured", "server_error", None),
            )
                .into_response()
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, "no upstream tokens configured").into_response()
        };
    }
    let (token_idx, token) = picked.unwrap();
    record_request(&state, token_idx);
    let body_bytes = match routed.target {
        TargetModel::Codex => target::codex::gateway::build_request_body(
            &method,
            &routed.upstream_path,
            &headers,
            routed.upstream_body,
            &session_id,
        ),
    };
    let mut req = state.client.request(method.clone(), upstream).body(body_bytes);

    // Copy headers except hop-by-hop/auth and proxy-edge client headers; set upstream auth
    for (k, v) in headers.iter() {
        if should_drop_incoming_header(k.as_str()) {
            continue;
        }
        req = req.header(k, v);
    }
    req = req.header("Authorization", format!("Bearer {}", token.token));
    req = match routed.target {
        TargetModel::Codex => target::codex::gateway::apply_default_headers(
            req,
            &headers,
            token.account_id.as_deref(),
            &session_id,
        ),
    };

    let resp = match req.send().await {
        Ok(r) => r,
        Err(err) => {
            error!("upstream error: {}", err);
            record_error(&state, token_idx);
            return if matches!(source_api, SourceApi::V1) {
                (
                    StatusCode::BAD_GATEWAY,
                    [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                    openai_error_body("Upstream error", "server_error", None),
                )
                    .into_response()
            } else {
                (StatusCode::BAD_GATEWAY, "upstream error").into_response()
            };
        }
    };

    let status = resp.status();
    let mut out_headers = HeaderMap::new();
    for (k, v) in resp.headers().iter() {
        let name = k.as_str().to_ascii_lowercase();
        if is_hop_header(&name) || name == "content-encoding" || name == "content-length" {
            continue;
        }
        out_headers.insert(k, v.clone());
    }

    if status.as_u16() >= 400 {
        record_error(&state, token_idx);
        let body_bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(err) => {
                error!("upstream error body read failed: {}", err);
                return if matches!(source_api, SourceApi::V1) {
                    (
                        StatusCode::BAD_GATEWAY,
                        [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                        openai_error_body("Upstream error", "server_error", None),
                    )
                        .into_response()
                } else {
                    (
                        StatusCode::BAD_GATEWAY,
                        "upstream error (failed to read body)",
                    )
                        .into_response()
                };
            }
        };
        return if matches!(source_api, SourceApi::V1) {
            let mut headers = out_headers;
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (status, headers, upstream_error_to_openai(status, &body_bytes)).into_response()
        } else {
            (status, out_headers, body_bytes).into_response()
        };
    }

    if matches!(routed.response_mode, ResponseMode::SseToJson) {
        let body_bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(err) => {
                error!("upstream body read failed: {}", err);
                return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
            }
        };
        let json_body = sse_to_response_json(&body_bytes);
        let mut headers = out_headers;
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        return (status, headers, json_body).into_response();
    }

    if matches!(source_api, SourceApi::V1) && method == Method::GET {
        if is_v1_models_list_path(&raw_path) || v1_model_retrieve_id(&raw_path).is_some() {
            let body_bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(err) => {
                    error!("upstream body read failed: {}", err);
                    return (
                        StatusCode::BAD_GATEWAY,
                        [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                        openai_error_body("Upstream error", "server_error", None),
                    )
                        .into_response();
                }
            };
            let converted = if let Some(model_id) = v1_model_retrieve_id(&raw_path) {
                model_retrieve_to_openai_json(&body_bytes, &model_id).map_err(|e| {
                    if e.contains("does not exist") {
                        (StatusCode::NOT_FOUND, e)
                    } else {
                        (StatusCode::BAD_GATEWAY, e)
                    }
                })
            } else {
                models_list_to_openai_json(&body_bytes).map_err(|e| (StatusCode::BAD_GATEWAY, e))
            };
            return match converted {
                Ok(json_body) => {
                    let mut headers = out_headers;
                    headers.insert(
                        axum::http::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    (status, headers, json_body).into_response()
                }
                Err((mapped_status, mapped_message)) => (
                    mapped_status,
                    [(axum::http::header::CONTENT_TYPE.as_str(), "application/json")],
                    openai_error_body(
                        &mapped_message,
                        if mapped_status == StatusCode::NOT_FOUND {
                            "invalid_request_error"
                        } else {
                            "server_error"
                        },
                        if mapped_status == StatusCode::NOT_FOUND {
                            Some("model_not_found")
                        } else {
                            None
                        },
                    ),
                )
                    .into_response(),
            };
        }
    }

    // Stream response body back
    let stats_state = state.clone();
    let stream_idx = token_idx;
    let stream = resp.bytes_stream().map(move |chunk| {
        if let Err(ref err) = chunk {
            error!("stream chunk error: {}", err);
            record_error(&stats_state, stream_idx);
        }
        chunk.map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "stream"))
    });
    let body = Body::from_stream(stream);
    if matches!(source_api, SourceApi::V1)
        && !out_headers.contains_key(axum::http::header::CONTENT_TYPE)
    {
        out_headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
    }
    (status, out_headers, body).into_response()
}

fn check_api_key(headers: &HeaderMap, expected: &str) -> bool {
    let auth = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if let Some(token) = auth.strip_prefix("Bearer ") {
        return token == expected;
    }
    false
}

fn pick_token(state: &AppState) -> Option<(usize, UpstreamToken)> {
    let mut idx = state.rr.lock().unwrap();
    let tokens = state.tokens.lock().unwrap();
    if tokens.is_empty() {
        return None;
    }
    let len = tokens.len();
    for _ in 0..len {
        let picked_idx = *idx % len;
        *idx = (*idx + 1) % len;
        if tokens[picked_idx].enabled {
            let token = tokens[picked_idx].clone();
            return Some((picked_idx, token));
        }
    }
    None
}

fn record_request(state: &AppState, idx: usize) {
    let mut stats = state.stats.lock().unwrap();
    stats.total_requests += 1;
    if let Some(a) = stats.per_account.get_mut(idx) {
        a.requests += 1;
    }
}

fn record_error(state: &AppState, idx: usize) {
    let mut stats = state.stats.lock().unwrap();
    stats.total_errors += 1;
    if let Some(a) = stats.per_account.get_mut(idx) {
        a.errors += 1;
    }
}


fn is_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn should_drop_incoming_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if is_hop_header(&lower)
        || lower == "authorization"
        || lower == "host"
        || lower == "content-length"
        || lower == "x-forwarded-for"
        || lower == "x-forwarded-host"
        || lower == "x-forwarded-proto"
        || lower == "x-real-ip"
        || lower == "true-client-ip"
    {
        return true;
    }

    // Never leak edge-provider headers upstream (for example Cloudflare).
    lower.starts_with("cf-")
}

fn load_config() -> Config {
    // expects config.json in working dir
    let data = std::fs::read_to_string("config.json").expect("config.json missing");
    serde_json::from_str(&data).expect("invalid config.json")
}

fn detect_source_api(raw_path: &str) -> SourceApi {
    let trimmed = raw_path.trim_start_matches('/');
    if trimmed == "codex" || trimmed.starts_with("codex/") {
        SourceApi::Codex
    } else if trimmed == "claude" || trimmed.starts_with("claude/") {
        SourceApi::Claude
    } else {
        SourceApi::V1
    }
}

fn is_v1_models_list_path(raw_path: &str) -> bool {
    normalize_v1_path(raw_path) == "models"
}

fn v1_model_retrieve_id(raw_path: &str) -> Option<String> {
    let norm = normalize_v1_path(raw_path);
    let id = norm.strip_prefix("models/")?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(id.to_string())
}

fn normalize_v1_path(raw_path: &str) -> String {
    let trimmed = raw_path.trim_start_matches('/');
    if trimmed == "v1" {
        String::new()
    } else if let Some(rest) = trimmed.strip_prefix("v1/") {
        rest.to_string()
    } else {
        trimmed.to_string()
    }
}
