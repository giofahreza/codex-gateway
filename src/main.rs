use axum::{
    body::Body,
    extract::{Form, OriginalUri, Path, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
    routing::any,
    Router,
};
use axum::http::HeaderValue;
use bytes::Bytes;
use futures_util::StreamExt;
use base64::Engine;
use rand::{distr::Alphanumeric, Rng};
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tracing::{error, info};
use uuid::Uuid;
use url::Url;
use chrono::{DateTime, Utc};

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

#[derive(Clone)]
struct UpstreamToken {
    token: String,
    account_id: Option<String>,
    label: String,
    file_name: Option<String>,
    enabled: bool,
    expired_at: Option<String>,
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

#[derive(Clone)]
struct QuotaCacheEntry {
    fetched_at: std::time::Instant,
    summary: QuotaSummary,
    error: Option<String>,
}

#[derive(Default, Clone, Serialize)]
struct QuotaSummary {
    label: String,
    account_id: String,
    plan_type: String,
    code_generation: QuotaRateSummary,
    code_review: QuotaRateSummary,
}

#[derive(Default, Clone, Serialize)]
struct QuotaRateSummary {
    five_hour: Option<QuotaWindowSummary>,
    weekly: Option<QuotaWindowSummary>,
}

#[derive(Default, Clone, Serialize)]
struct QuotaWindowSummary {
    used_percent: Option<f64>,
    reset_label: String,
}

#[derive(Clone)]
struct PendingOAuth {
    code_verifier: String,
    created_at: std::time::Instant,
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
    let tokens = load_tokens(&cfg, &disabled);
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
        .route("/quota.json", any(quota_json))
        .route("/credentials/delete", any(delete_credential))
        .route("/credentials/toggle", any(toggle_credential))
        .route("/login/codex/start", any(login_start))
        .route("/login/codex/submit", any(login_submit))
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

async fn quota_json(State(state): State<AppState>) -> impl IntoResponse {
    let accounts = get_quota_summaries(&state).await;
    axum::Json(serde_json::json!({ "accounts": accounts }))
}

async fn login_start(State(state): State<AppState>) -> impl IntoResponse {
    let (url, state_token, code_verifier) = match build_codex_auth_url() {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("failed to create auth url: {}", err),
            )
                .into_response();
        }
    };
    {
        let mut pending = state.oauth_pending.lock().unwrap();
        pending.insert(
            state_token.clone(),
            PendingOAuth {
                code_verifier,
                created_at: std::time::Instant::now(),
            },
        );
    }
    axum::Json(serde_json::json!({ "url": url, "state": state_token })).into_response()
}

#[derive(Deserialize)]
struct CallbackForm {
    redirect_url: String,
}

#[derive(Deserialize)]
struct DeleteForm {
    file_name: String,
}

#[derive(Deserialize)]
struct ToggleForm {
    file_name: String,
    enabled: String,
}

async fn login_submit(
    State(state): State<AppState>,
    Form(form): Form<CallbackForm>,
) -> impl IntoResponse {
    let redirect_url = form.redirect_url.trim();
    if redirect_url.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "redirect_url is required"
        }))
        .into_response();
    }
    let (code, state_token) = match parse_oauth_callback(redirect_url) {
        Ok(v) => v,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response()
        }
    };
    let code_verifier = {
        let mut pending = state.oauth_pending.lock().unwrap();
        match pending.remove(&state_token) {
            Some(p) => p.code_verifier,
            None => {
                return axum::Json(serde_json::json!({
                    "ok": false,
                    "message": "invalid or expired state"
                }))
                .into_response()
            }
        }
    };

    match exchange_code_for_tokens(&state.client, &code, &code_verifier).await {
        Ok(token_resp) => match save_codex_auth(&state, &token_resp) {
            Ok(saved_path) => axum::Json(serde_json::json!({
                "ok": true,
                "message": format!("saved credentials to {}", saved_path)
            }))
            .into_response(),
            Err(err) => axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response(),
        },
        Err(err) => axum::Json(serde_json::json!({
            "ok": false,
            "message": err
        }))
        .into_response(),
    }
}

async fn delete_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> impl IntoResponse {
    if !check_api_key(&headers, &state.cfg.proxy_api_key) {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "unauthorized"
        }))
        .into_response();
    }
    let file_name = form.file_name.trim();
    if file_name.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "file_name is required"
        }))
        .into_response();
    }
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    match std::fs::remove_file(&path) {
        Ok(_) => {
            // reload tokens + stats
            let disabled = state.disabled.lock().unwrap().clone();
            let tokens = load_tokens(&state.cfg, &disabled);
            {
                let mut tlock = state.tokens.lock().unwrap();
                *tlock = tokens.clone();
            }
            {
                let mut stats = state.stats.lock().unwrap();
                stats.per_account = tokens
                    .iter()
                    .map(|t| AccountUsage {
                        label: t.label.clone(),
                        account_id: t.account_id.clone().unwrap_or_default(),
                        requests: 0,
                        errors: 0,
                    })
                    .collect();
                stats.total_requests = 0;
                stats.total_errors = 0;
            }
            {
                let mut cache = state.quota_cache.lock().unwrap();
                *cache = vec![None; tokens.len()];
            }
            axum::Json(serde_json::json!({
                "ok": true,
                "message": format!("deleted {}", file_name)
            }))
            .into_response()
        }
        Err(err) => axum::Json(serde_json::json!({
            "ok": false,
            "message": format!("delete failed: {}", err)
        }))
        .into_response(),
    }
}

async fn toggle_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ToggleForm>,
) -> impl IntoResponse {
    if !check_api_key(&headers, &state.cfg.proxy_api_key) {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "unauthorized"
        }))
        .into_response();
    }
    let file_name = form.file_name.trim();
    if file_name.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "file_name is required"
        }))
        .into_response();
    }
    let enable = form.enabled.trim().eq_ignore_ascii_case("true");

    {
        let mut disabled = state.disabled.lock().unwrap();
        if enable {
            disabled.remove(file_name);
        } else {
            disabled.insert(file_name.to_string());
        }
    }

    if let Err(err) = persist_disabled_list(state.cfg.as_ref(), &state.disabled) {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": format!("failed to persist: {}", err)
        }))
        .into_response();
    }

    // reload tokens + stats
    let disabled = state.disabled.lock().unwrap().clone();
    let tokens = load_tokens(&state.cfg, &disabled);
    {
        let mut tlock = state.tokens.lock().unwrap();
        *tlock = tokens.clone();
    }
    {
        let mut stats = state.stats.lock().unwrap();
        stats.per_account = tokens
            .iter()
            .map(|t| AccountUsage {
                label: t.label.clone(),
                account_id: t.account_id.clone().unwrap_or_default(),
                requests: 0,
                errors: 0,
            })
            .collect();
        stats.total_requests = 0;
        stats.total_errors = 0;
    }
    {
        let mut cache = state.quota_cache.lock().unwrap();
        *cache = vec![None; tokens.len()];
    }

    axum::Json(serde_json::json!({
        "ok": true,
        "message": format!("{} {}", if enable { "enabled" } else { "disabled" }, file_name)
    }))
    .into_response()
}

async fn proxy(
    State(state): State<AppState>,
    Path(path): Path<String>,
    method: Method,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    body: Body,
) -> impl IntoResponse {
    // Simple API key guard
    if !check_api_key(&headers, &state.cfg.proxy_api_key) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    // Read full body (small/simple proxy)
    let body_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid body").into_response(),
    };

    let (mapped_path, mapped_query) = map_v1_path_and_query(&path, &uri);
    let client_wants_stream =
        mapped_path == "responses" && method == Method::POST && wants_stream(&headers, &body_bytes);
    let upstream = build_upstream_url(&state.cfg.upstream_base, &mapped_path, mapped_query.as_deref());
    let session_id = Uuid::new_v4().to_string();

    let picked = pick_token(&state);
    if picked.is_none() {
        return (StatusCode::SERVICE_UNAVAILABLE, "no upstream tokens configured")
            .into_response();
    }
    let (token_idx, token) = picked.unwrap();
    record_request(&state, token_idx);
    let mut body_bytes = maybe_apply_prompt_cache_key(&headers, body_bytes, &session_id);
    if mapped_path == "responses" && method == Method::POST {
        body_bytes = ensure_store_and_stream(&headers, body_bytes);
    }
    let mut req = state.client.request(method.clone(), upstream).body(body_bytes);

    // Copy headers except hop-by-hop and auth; set upstream auth
    for (k, v) in headers.iter() {
        if is_hop_header(k.as_str())
            || k.as_str().eq_ignore_ascii_case("authorization")
            || k.as_str().eq_ignore_ascii_case("host")
            || k.as_str().eq_ignore_ascii_case("content-length")
        {
            continue;
        }
        req = req.header(k, v);
    }
    req = req.header("Authorization", format!("Bearer {}", token.token));
    req = apply_default_codex_headers(
        req,
        &headers,
        token.account_id.as_deref(),
        &session_id,
    );

    let resp = match req.send().await {
        Ok(r) => r,
        Err(err) => {
            error!("upstream error: {}", err);
            record_error(&state, token_idx);
            return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
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
                return (
                    StatusCode::BAD_GATEWAY,
                    "upstream error (failed to read body)",
                )
                    .into_response();
            }
        };
        return (status, out_headers, body_bytes).into_response();
    }

    if mapped_path == "responses" && method == Method::POST && !client_wants_stream {
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

fn build_upstream_url(base: &str, path: &str, query: Option<&str>) -> String {
    let mut base = base.trim_end_matches('/').to_string();
    if !path.is_empty() {
        base.push('/');
        base.push_str(path.trim_start_matches('/'));
    }
    if let Some(q) = query {
        base.push('?');
        base.push_str(q);
    }
    base
}

fn map_v1_path_and_query(path: &str, uri: &Uri) -> (String, Option<String>) {
    let mut new_path = path.trim_start_matches('/').to_string();
    if let Some(stripped) = new_path.strip_prefix("v1/") {
        new_path = stripped.to_string();
    }

    let mut query = uri.query().unwrap_or("").to_string();
    if new_path == "models" {
        let has_client_version = query
            .split('&')
            .any(|kv| kv.starts_with("client_version="));
        if !has_client_version {
            if query.is_empty() {
                query = "client_version=0.0.0".to_string();
            } else {
                query.push('&');
                query.push_str("client_version=0.0.0");
            }
        }
    }

    let query = if query.is_empty() { None } else { Some(query) };
    (new_path, query)
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

fn apply_default_codex_headers(
    mut req: reqwest::RequestBuilder,
    incoming: &HeaderMap,
    account_id: Option<&str>,
    session_id: &str,
) -> reqwest::RequestBuilder {
    // Mirror codex-cli defaults if missing.
    if !incoming.contains_key("content-type") {
        req = req.header("Content-Type", "application/json");
    }
    if !incoming.contains_key("accept") {
        req = req.header("Accept", "text/event-stream");
    }
    if !incoming.contains_key("connection") {
        req = req.header("Connection", "Keep-Alive");
    }
    if !incoming.contains_key("openai-beta") {
        req = req.header("Openai-Beta", "responses=experimental");
    }
    if !incoming.contains_key("version") {
        req = req.header("Version", "0.21.0");
    }
    if !incoming.contains_key("session_id") {
        req = req.header("Session_id", session_id);
    }
    if !incoming.contains_key("conversation_id") {
        req = req.header("Conversation_id", session_id);
    }
    if !incoming.contains_key("user-agent") {
        req = req.header(
            "User-Agent",
            "codex_cli_rs/0.50.0 (Mac OS 26.0.1; arm64) Apple_Terminal/464",
        );
    }
    if !incoming.contains_key("origin") {
        req = req.header("Origin", "https://chatgpt.com");
    }
    if !incoming.contains_key("referer") {
        req = req.header("Referer", "https://chatgpt.com/");
    }
    if !incoming.contains_key("accept-language") {
        req = req.header("Accept-Language", "en-US,en;q=0.9");
    }
    if !incoming.contains_key("accept-encoding") {
        req = req.header("Accept-Encoding", "identity");
    }
    if !incoming.contains_key("originator") {
        req = req.header("Originator", "codex_cli_rs");
    }
    if !incoming.contains_key("chatgpt-account-id") {
        if let Some(id) = account_id {
            if !id.trim().is_empty() {
                req = req.header("Chatgpt-Account-Id", id);
            }
        }
    }
    req
}

fn maybe_apply_prompt_cache_key(
    headers: &HeaderMap,
    body: Bytes,
    session_id: &str,
) -> Bytes {
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        if !ct.to_ascii_lowercase().contains("application/json") {
            return body;
        }
    } else {
        return body;
    }

    let mut value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return body,
    };
    if let serde_json::Value::Object(map) = &mut value {
        if !map.contains_key("prompt_cache_key") {
            map.insert(
                "prompt_cache_key".to_string(),
                serde_json::Value::String(session_id.to_string()),
            );
        }
        if !map.contains_key("stream") {
            map.insert("stream".to_string(), serde_json::Value::Bool(true));
        }
        if let Ok(out) = serde_json::to_vec(&value) {
            return Bytes::from(out);
        }
    }
    body
}

fn ensure_store_and_stream(headers: &HeaderMap, body: Bytes) -> Bytes {
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        if !ct.to_ascii_lowercase().contains("application/json") {
            return body;
        }
    } else {
        return body;
    }

    let mut value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return body,
    };
    if let serde_json::Value::Object(map) = &mut value {
        map.insert("store".to_string(), serde_json::Value::Bool(false));
        map.insert("stream".to_string(), serde_json::Value::Bool(true));
        if let Ok(out) = serde_json::to_vec(&value) {
            return Bytes::from(out);
        }
    }
    body
}

fn wants_stream(headers: &HeaderMap, body: &Bytes) -> bool {
    if let Some(accept) = headers.get("accept").and_then(|v| v.to_str().ok()) {
        if accept.to_ascii_lowercase().contains("text/event-stream") {
            return true;
        }
    }
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        if !ct.to_ascii_lowercase().contains("application/json") {
            return false;
        }
    } else {
        return false;
    }
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return false,
    };
    match value.get("stream") {
        Some(serde_json::Value::Bool(b)) => *b,
        _ => false,
    }
}

fn sse_to_response_json(body: &Bytes) -> Bytes {
    let text = String::from_utf8_lossy(body);
    let mut response_obj: Option<serde_json::Value> = None;
    let mut output_text = String::new();

    for line in text.lines() {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            break;
        }
        let v: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(t) = v.get("type").and_then(|v| v.as_str()) {
            if t == "response.output_text.delta" {
                if let Some(delta) = v.get("delta").and_then(|v| v.as_str()) {
                    output_text.push_str(delta);
                }
            } else if t == "response.completed" {
                if let Some(r) = v.get("response") {
                    response_obj = Some(r.clone());
                }
            } else if t == "response.created" && response_obj.is_none() {
                if let Some(r) = v.get("response") {
                    response_obj = Some(r.clone());
                }
            }
        }
    }

    let mut resp = response_obj.unwrap_or_else(|| {
        serde_json::json!({
            "object": "response",
            "output": []
        })
    });
    if let serde_json::Value::Object(map) = &mut resp {
        map.insert(
            "output_text".to_string(),
            serde_json::Value::String(output_text.clone()),
        );
        let output_is_empty = map
            .get("output")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if output_is_empty {
            map.insert(
                "output".to_string(),
                serde_json::json!([{
                    "type": "message",
                    "id": "msg_compat",
                    "role": "assistant",
                    "content": [{"type":"output_text","text": output_text}]
                }]),
            );
        }
    }
    Bytes::from(serde_json::to_vec(&resp).unwrap_or_default())
}

fn load_config() -> Config {
    // expects config.json in working dir
    let data = std::fs::read_to_string("config.json").expect("config.json missing");
    serde_json::from_str(&data).expect("invalid config.json")
}

fn persist_disabled_list(cfg: &Config, disabled: &Arc<Mutex<HashSet<String>>>) -> Result<(), String> {
    let mut v: serde_json::Value = {
        let data = std::fs::read_to_string("config.json").map_err(|e| e.to_string())?;
        serde_json::from_str(&data).map_err(|e| e.to_string())?
    };
    let list: Vec<String> = disabled.lock().unwrap().iter().cloned().collect();
    if let serde_json::Value::Object(map) = &mut v {
        if list.is_empty() {
            map.remove("disabled_files");
        } else {
            map.insert("disabled_files".to_string(), serde_json::json!(list));
        }
    }
    std::fs::write("config.json", serde_json::to_vec_pretty(&v).unwrap())
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_tokens(cfg: &Config, disabled: &HashSet<String>) -> Vec<UpstreamToken> {
    let mut tokens: Vec<UpstreamToken> = cfg
        .tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.trim().to_string()))
        .filter(|(_, t)| !t.is_empty())
        .map(|(i, t)| UpstreamToken {
            token: t,
            account_id: None,
            label: format!("manual-{}", i + 1),
            file_name: None,
            enabled: true,
            expired_at: None,
        })
        .collect();

    if let Some(dir) = cfg.auth_dir.as_ref() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            files.sort_by_key(|e| e.path());
            for entry in files {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let data = match std::fs::read_to_string(&path) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let value: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(t) = value.get("type").and_then(|v| v.as_str()) {
                    if !t.eq_ignore_ascii_case("codex") {
                        continue;
                    }
                }
                let account_id = value
                    .get("account_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let label = value
                    .get("email")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("label").and_then(|v| v.as_str()))
                    .or_else(|| value.get("account_id").and_then(|v| v.as_str()))
                    .or_else(|| path.file_name().and_then(|s| s.to_str()))
                    .unwrap_or("codex-account")
                    .to_string();
                // Prefer subscription end date from ID token.
                // Fallback to access token expiration only when subscription data is unavailable.
                let expired_at = value
                    .get("id_token")
                    .and_then(|v| v.as_str())
                    .and_then(parse_jwt_subscription_until)
                    .or_else(|| {
                        value
                            .get("expired")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                if let Some(tok) = value
                    .get("access_token")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("api_key").and_then(|v| v.as_str()))
                {
                    let tok = tok.trim();
                    if !tok.is_empty() {
                        let file_name = path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string());
                        let enabled = file_name
                            .as_ref()
                            .map(|f| !disabled.contains(f))
                            .unwrap_or(true);
                        tokens.push(UpstreamToken {
                            token: tok.to_string(),
                            account_id,
                            label,
                            file_name,
                            enabled,
                            expired_at,
                        });
                    }
                }
            }
        }
    }

    // de-dup while preserving order
    let mut seen = HashSet::new();
    tokens.retain(|t| seen.insert(t.token.clone()));
    tokens
}

fn build_codex_auth_url() -> Result<(String, String, String), String> {
    let code_verifier: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let digest = hasher.finalize();
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    let state_token: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let mut url = Url::parse("https://auth.openai.com/oauth/authorize")
        .map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", "http://localhost:1455/auth/callback")
        .append_pair("scope", "openid email profile offline_access")
        .append_pair("state", &state_token)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "login")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true");

    Ok((url.to_string(), state_token, code_verifier))
}

fn parse_oauth_callback(redirect_url: &str) -> Result<(String, String), String> {
    let url = Url::parse(redirect_url).map_err(|_| "invalid redirect_url".to_string())?;
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let code = params.get("code").cloned().unwrap_or_default();
    let state = params.get("state").cloned().unwrap_or_default();
    if code.is_empty() || state.is_empty() {
        return Err("missing code or state in redirect_url".to_string());
    }
    Ok((code, state))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_in: i64,
    token_type: Option<String>,
}

async fn exchange_code_for_tokens(
    client: &reqwest::Client,
    code: &str,
    code_verifier: &str,
) -> Result<TokenResponse, String> {
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"),
        ("code", code),
        ("redirect_uri", "http://localhost:1455/auth/callback"),
        ("code_verifier", code_verifier),
    ];
    let resp = client
        .post("https://auth.openai.com/oauth/token")
        .form(&params)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token exchange failed: {}", body));
    }
    resp.json::<TokenResponse>().await.map_err(|e| e.to_string())
}

fn save_codex_auth(state: &AppState, token_resp: &TokenResponse) -> Result<String, String> {
    let id_token = &token_resp.id_token;
    let account_id = parse_jwt_account_id(id_token).unwrap_or_default();
    let email = parse_jwt_email(id_token).unwrap_or_else(|| "unknown".to_string());
    let now = Utc::now();
    let expires_at = now + chrono::Duration::seconds(token_resp.expires_in);

    let file_name = format!("codex-{}.json", sanitize_label(&email));
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    std::fs::create_dir_all(&auth_dir).map_err(|e| e.to_string())?;
    let out = serde_json::json!({
        "id_token": token_resp.id_token,
        "access_token": token_resp.access_token,
        "refresh_token": token_resp.refresh_token,
        "account_id": account_id,
        "last_refresh": now.to_rfc3339(),
        "email": email,
        "type": "codex",
        "expired": expires_at.to_rfc3339()
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&out).unwrap())
        .map_err(|e| e.to_string())?;

    // reload tokens + stats
    let disabled = state.disabled.lock().unwrap().clone();
    let tokens = load_tokens(&state.cfg, &disabled);
    {
        let mut tlock = state.tokens.lock().unwrap();
        *tlock = tokens.clone();
    }
    {
        let mut stats = state.stats.lock().unwrap();
        stats.per_account = tokens
            .iter()
            .map(|t| AccountUsage {
                label: t.label.clone(),
                account_id: t.account_id.clone().unwrap_or_default(),
                requests: 0,
                errors: 0,
            })
            .collect();
        stats.total_requests = 0;
        stats.total_errors = 0;
    }
    {
        let mut cache = state.quota_cache.lock().unwrap();
        *cache = vec![None; tokens.len()];
    }
    Ok(path.to_string_lossy().to_string())
}

fn sanitize_label(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

fn parse_jwt_email(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("email")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            v.get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
}

fn parse_jwt_account_id(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
}

fn parse_jwt_subscription_until(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let raw = v
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_subscription_active_until"))?;
    if let Some(s) = raw.as_str() {
        return Some(s.to_string());
    }
    if let Some(ts) = raw.as_f64() {
        if ts > 0.0 {
            let dt = if ts > 1e12 {
                DateTime::<Utc>::from(std::time::UNIX_EPOCH + Duration::from_millis(ts as u64))
            } else {
                DateTime::<Utc>::from(std::time::UNIX_EPOCH + Duration::from_secs(ts as u64))
            };
            return Some(dt.to_rfc3339());
        }
    }
    None
}

async fn get_quota_summaries(state: &AppState) -> Vec<serde_json::Value> {
    let tokens = state.tokens.lock().unwrap().clone();
    {
        let mut cache = state.quota_cache.lock().unwrap();
        if cache.len() != tokens.len() {
            *cache = vec![None; tokens.len()];
        }
    }
    let now = std::time::Instant::now();
    let mut results = Vec::with_capacity(tokens.len());
    for (idx, token) in tokens.iter().enumerate() {
        let cached = {
            let cache = state.quota_cache.lock().unwrap();
            cache.get(idx).cloned().flatten()
        };
        let entry = if let Some(c) = cached {
            if now.duration_since(c.fetched_at).as_secs() < 60 {
                c
            } else {
                let fetched = fetch_codex_quota(state, token).await;
                let mut cache = state.quota_cache.lock().unwrap();
                if cache.len() <= idx {
                    cache.resize(idx + 1, None);
                }
                cache[idx] = Some(fetched.clone());
                fetched
            }
        } else {
            let fetched = fetch_codex_quota(state, token).await;
            let mut cache = state.quota_cache.lock().unwrap();
            if cache.len() <= idx {
                cache.resize(idx + 1, None);
            }
            cache[idx] = Some(fetched.clone());
            fetched
        };
        if let Some(err) = entry.error {
            results.push(serde_json::json!({
                "label": token.label,
                "account_id": token.account_id.clone().unwrap_or_default(),
                "file_name": token.file_name.clone().unwrap_or_default(),
                "error": err
            }));
        } else {
            results.push(serde_json::json!({
                "label": entry.summary.label,
                "account_id": entry.summary.account_id,
                "file_name": token.file_name.clone().unwrap_or_default(),
                "plan_type": entry.summary.plan_type,
                "code_generation": entry.summary.code_generation,
                "code_review": entry.summary.code_review
            }));
        }
    }
    results
}

async fn fetch_codex_quota(state: &AppState, token: &UpstreamToken) -> QuotaCacheEntry {
    let mut req = state
        .client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {}", token.token))
        .header("Content-Type", "application/json")
        .header("User-Agent", "codex_cli_rs/0.76.0 (Debian 13.0.0; x86_64) WindowsTerminal");
    if let Some(account_id) = token.account_id.as_ref() {
        if !account_id.trim().is_empty() {
            req = req.header("Chatgpt-Account-Id", account_id);
        }
    }

    let resp = match req.timeout(Duration::from_secs(30)).send().await {
        Ok(r) => r,
        Err(err) => {
            return QuotaCacheEntry {
                fetched_at: std::time::Instant::now(),
                summary: QuotaSummary::default(),
                error: Some(err.to_string()),
            }
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return QuotaCacheEntry {
            fetched_at: std::time::Instant::now(),
            summary: QuotaSummary::default(),
            error: Some(format!("status {}: {}", status.as_u16(), body)),
        };
    }
    let body = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return QuotaCacheEntry {
                fetched_at: std::time::Instant::now(),
                summary: QuotaSummary::default(),
                error: Some("failed to parse quota response".to_string()),
            }
        }
    };

    let plan_type = v
        .get("plan_type")
        .or_else(|| v.get("planType"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let rate_limit = v.get("rate_limit").or_else(|| v.get("rateLimit"));
    let code_review = v
        .get("code_review_rate_limit")
        .or_else(|| v.get("codeReviewRateLimit"));

    let summary = QuotaSummary {
        label: token.label.clone(),
        account_id: token.account_id.clone().unwrap_or_default(),
        plan_type,
        code_generation: extract_quota_rate(rate_limit),
        code_review: extract_quota_rate(code_review),
    };
    QuotaCacheEntry {
        fetched_at: std::time::Instant::now(),
        summary,
        error: None,
    }
}

fn extract_quota_rate(rate: Option<&serde_json::Value>) -> QuotaRateSummary {
    if rate.is_none() {
        return QuotaRateSummary::default();
    }
    let rate = rate.unwrap();
    let primary = rate.get("primary_window").or_else(|| rate.get("primaryWindow"));
    let secondary = rate
        .get("secondary_window")
        .or_else(|| rate.get("secondaryWindow"));
    let mut five_hour = None;
    let mut weekly = None;
    for w in [primary, secondary] {
        if let Some(win) = w {
            let secs = get_window_seconds(win);
            if secs == 18000 && five_hour.is_none() {
                five_hour = Some(build_window_summary(win));
            } else if secs == 604800 && weekly.is_none() {
                weekly = Some(build_window_summary(win));
            } else if five_hour.is_none() {
                five_hour = Some(build_window_summary(win));
            } else if weekly.is_none() {
                weekly = Some(build_window_summary(win));
            }
        }
    }
    QuotaRateSummary { five_hour, weekly }
}

fn get_window_seconds(win: &serde_json::Value) -> i64 {
    let raw = win
        .get("limit_window_seconds")
        .or_else(|| win.get("limitWindowSeconds"));
    raw.and_then(|v| v.as_f64())
        .map(|v| v as i64)
        .unwrap_or(0)
}

fn build_window_summary(win: &serde_json::Value) -> QuotaWindowSummary {
    let used = win.get("used_percent").or_else(|| win.get("usedPercent"));
    let used_percent = used.and_then(|v| v.as_f64());
    let reset_label = get_reset_label(win);
    QuotaWindowSummary {
        used_percent,
        reset_label,
    }
}

fn get_reset_label(win: &serde_json::Value) -> String {
    if let Some(v) = win.get("reset_at").or_else(|| win.get("resetAt")) {
        if let Some(ts) = v.as_f64() {
            let ms = if ts > 1e12 { ts } else { ts * 1000.0 };
            let dt = DateTime::<Utc>::from(std::time::UNIX_EPOCH + Duration::from_millis(ms as u64));
            return format_reset_time(dt);
        }
    }
    if let Some(v) = win
        .get("reset_after_seconds")
        .or_else(|| win.get("resetAfterSeconds"))
    {
        if let Some(secs) = v.as_f64() {
            let mins = (secs / 60.0).floor() as i64;
            if mins < 60 {
                return format!("in {}m", mins);
            }
            let hours = mins / 60;
            let rem = mins % 60;
            if hours < 24 {
                return format!("in {}h {}m", hours, rem);
            }
            let days = hours / 24;
            return format!("in {}d {}h", days, hours % 24);
        }
    }
    String::new()
}

fn format_reset_time(dt: DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = dt - now;
    let mins = diff.num_minutes();
    if mins <= 0 {
        return "now".to_string();
    }
    if mins < 60 {
        return format!("in {}m", mins);
    }
    let hours = mins / 60;
    let rem = mins % 60;
    if hours < 24 {
        return format!("in {}h {}m", hours, rem);
    }
    let days = hours / 24;
    format!("in {}d {}h", days, hours % 24)
}
