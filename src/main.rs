use axum::http::HeaderValue;
use axum::{
    body::Body,
    extract::{Form, OriginalUri, Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use bytes::{Bytes, BytesMut};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tracing::{error, info};
use uuid::Uuid;
mod admin_auth;
mod source;
mod stats_store;
mod target;
mod usage_store;
use source::v1::response::{
    model_retrieve_to_openai_json, models_list_to_openai_json, openai_error_body,
    sse_to_response_json, upstream_error_to_openai,
};
use source::{route_request, ResponseMode, TargetModel};
use stats_store::{Provider, StatsStore};
use target::codex::auth::PendingOAuth;
use target::codex::quota::QuotaCacheEntry;
use target::codex::tokens::UpstreamToken;

#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
    rr: Arc<Mutex<usize>>,
    agw_rr: Arc<Mutex<usize>>,
    gemini_rr: Arc<Mutex<usize>>,
    qwen_rr: Arc<Mutex<usize>>,
    deepseek_rr: Arc<Mutex<usize>>,
    minimax_rr: Arc<Mutex<usize>>,
    grok_rr: Arc<Mutex<usize>>,
    client: reqwest::Client,
    tokens: Arc<Mutex<Vec<UpstreamToken>>>,
    agw_accounts: Arc<Mutex<Vec<target::antigravity::accounts::AntigravityAccount>>>,
    gemini_accounts: Arc<Mutex<Vec<target::gemini::accounts::GeminiAccount>>>,
    qwen_accounts: Arc<Mutex<Vec<target::qwen::accounts::QwenAccount>>>,
    deepseek_accounts: Arc<Mutex<Vec<target::deepseek::accounts::DeepSeekAccount>>>,
    minimax_accounts: Arc<Mutex<Vec<target::minimax::accounts::MiniMaxAccount>>>,
    grok_accounts: Arc<Mutex<Vec<target::grok::accounts::GrokAccount>>>,
    stats: Arc<Mutex<UsageStats>>,
    persisted_stats: Arc<Mutex<StatsStore>>,
    quota_cache: Arc<Mutex<Vec<Option<QuotaCacheEntry>>>>,
    agw_quota_cache: Arc<Mutex<HashMap<String, target::antigravity::quota::QuotaCacheEntry>>>,
    gemini_quota_cache: Arc<Mutex<HashMap<String, target::gemini::quota::QuotaCacheEntry>>>,
    qwen_quota_cache: Arc<Mutex<HashMap<String, target::qwen::quota::QuotaCacheEntry>>>,
    minimax_quota_cache: Arc<Mutex<HashMap<String, target::minimax::quota::QuotaCacheEntry>>>,
    deepseek_quota_cache: Arc<Mutex<HashMap<String, target::deepseek::quota::QuotaCacheEntry>>>,
    oauth_pending: Arc<Mutex<HashMap<String, PendingOAuth>>>,
    agw_oauth_pending: Arc<Mutex<HashMap<String, target::antigravity::auth::PendingOAuth>>>,
    gemini_oauth_pending: Arc<Mutex<HashSet<String>>>,
    qwen_oauth_pending: Arc<Mutex<HashMap<String, target::qwen::auth::PendingOAuth>>>,
    grok_oauth_pending: Arc<Mutex<HashMap<String, target::grok::auth::PendingOAuth>>>,
    admin_sessions: Arc<Mutex<HashMap<String, admin_auth::AdminSession>>>,
    disabled: Arc<Mutex<HashSet<String>>>,
    usage_history_lock: Arc<Mutex<()>>,
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
    #[serde(default)]
    admin_auth: admin_auth::AdminAuthConfig,
    #[serde(default)]
    oauth: target::oauth::OAuthConfig,
}

#[derive(Default, Clone, Serialize)]
struct UsageStats {
    codex_accounts: Vec<AccountUsage>,
    agw_accounts: Vec<AccountUsage>,
    gemini_accounts: Vec<AccountUsage>,
    qwen_accounts: Vec<AccountUsage>,
    deepseek_accounts: Vec<AccountUsage>,
    minimax_accounts: Vec<AccountUsage>,
    grok_accounts: Vec<AccountUsage>,
    total_requests: u64,
    total_errors: u64,
    total_prompt_total: u64,
    total_prompt_error_total: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_tokens_used: u64,
    total_cache_tokens: u64,
    total_reasoning_tokens: u64,
    first_recorded_at: Option<String>,
    last_recorded_at: Option<String>,
}

#[derive(Default, Clone, Serialize)]
struct AccountUsage {
    #[serde(skip_serializing)]
    key: String,
    label: String,
    account_id: String,
    requests: u64,
    errors: u64,
    prompt_total: u64,
    prompt_error_total: u64,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cache_tokens: u64,
    reasoning_tokens: u64,
    first_seen_at: Option<String>,
    last_seen_at: Option<String>,
    last_success_at: Option<String>,
    last_error_at: Option<String>,
}

#[derive(Default, Clone)]
pub(crate) struct PromptMetrics {
    input_chars: u64,
    prompt_items: u64,
    is_prompt: bool,
}

#[derive(Default, Clone)]
pub(crate) struct UsageMetrics {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cache_tokens: u64,
    reasoning_tokens: u64,
    raw_usage: Option<serde_json::Value>,
}

#[derive(Clone)]
pub(crate) struct UsageContext {
    provider: Provider,
    provider_name: &'static str,
    key: String,
    label: String,
    account_id: String,
    credential_file: Option<String>,
    model: Option<String>,
    request_path: String,
    prompt: PromptMetrics,
}

#[derive(Default)]
struct CounterDelta {
    request_delta: u64,
    error_delta: u64,
    prompt_total_delta: u64,
    prompt_error_total_delta: u64,
    input_tokens_delta: u64,
    output_tokens_delta: u64,
    total_tokens_delta: u64,
    cache_tokens_delta: u64,
    reasoning_tokens_delta: u64,
    observed_at: Option<String>,
    success_at: Option<String>,
    error_at: Option<String>,
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
    let agw_accounts = target::antigravity::accounts::load_accounts(&cfg, &disabled);
    let gemini_accounts = target::gemini::accounts::load_accounts(&cfg, &disabled);
    let qwen_accounts = target::qwen::accounts::load_accounts(&cfg, &disabled);
    let deepseek_accounts = target::deepseek::accounts::load_accounts(&cfg, &disabled);
    let grok_accounts = target::grok::accounts::load_accounts(&cfg, &disabled);
    let minimax_accounts = target::minimax::accounts::load_accounts(&cfg, &disabled);
    let persisted_stats = stats_store::load(&cfg);
    let stats = build_usage_stats(
        &tokens,
        &agw_accounts,
        &gemini_accounts,
        &qwen_accounts,
        &deepseek_accounts,
        &grok_accounts,
        &minimax_accounts,
        &persisted_stats,
    );
    let quota_cache = vec![None; tokens.len()];
    tracing_subscriber::fmt().with_env_filter("info").init();

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
        agw_rr: Arc::new(Mutex::new(0)),
        gemini_rr: Arc::new(Mutex::new(0)),
        qwen_rr: Arc::new(Mutex::new(0)),
        deepseek_rr: Arc::new(Mutex::new(0)),
        grok_rr: Arc::new(Mutex::new(0)),
        minimax_rr: Arc::new(Mutex::new(0)),
        client,
        tokens: Arc::new(Mutex::new(tokens)),
        agw_accounts: Arc::new(Mutex::new(agw_accounts)),
        gemini_accounts: Arc::new(Mutex::new(gemini_accounts)),
        qwen_accounts: Arc::new(Mutex::new(qwen_accounts)),
        deepseek_accounts: Arc::new(Mutex::new(deepseek_accounts)),
        grok_accounts: Arc::new(Mutex::new(grok_accounts)),
        minimax_accounts: Arc::new(Mutex::new(minimax_accounts)),
        stats: Arc::new(Mutex::new(stats)),
        persisted_stats: Arc::new(Mutex::new(persisted_stats)),
        quota_cache: Arc::new(Mutex::new(quota_cache)),
        agw_quota_cache: Arc::new(Mutex::new(HashMap::new())),
        gemini_quota_cache: Arc::new(Mutex::new(HashMap::new())),
        qwen_quota_cache: Arc::new(Mutex::new(HashMap::new())),
        minimax_quota_cache: Arc::new(Mutex::new(HashMap::new())),
        deepseek_quota_cache: Arc::new(Mutex::new(HashMap::new())),
        oauth_pending: Arc::new(Mutex::new(HashMap::new())),
        agw_oauth_pending: Arc::new(Mutex::new(HashMap::new())),
        gemini_oauth_pending: Arc::new(Mutex::new(HashSet::new())),
        qwen_oauth_pending: Arc::new(Mutex::new(HashMap::new())),
        grok_oauth_pending: Arc::new(Mutex::new(HashMap::new())),
        admin_sessions: Arc::new(Mutex::new(HashMap::new())),
        disabled: Arc::new(Mutex::new(disabled)),
        usage_history_lock: Arc::new(Mutex::new(())),
    };
    migrate_qwen_usage_keys(&state);
    migrate_grok_usage_keys(&state);
    sync_usage_stats(&state);

    let app = Router::new()
        .route("/health", any(health))
        .route("/", any(dashboard_root))
        .route("/dashboard", any(dashboard))
        .route("/admin/session", any(admin_session_route))
        .route("/admin/login", any(admin_login_route))
        .route("/admin/logout", any(admin_logout_route))
        .route("/dashboard.json", any(dashboard_json))
        .route("/quota.json", any(quota_json_route))
        .route("/credentials/delete", any(delete_credential_route))
        .route("/credentials/toggle", any(toggle_credential_route))
        .route("/login/codex/start", any(login_start_route))
        .route("/login/codex/submit", any(login_submit_route))
        .route("/agw/accounts.json", any(agw_accounts_route))
        .route("/agw/quota.json", any(agw_quota_json_route))
        .route("/login/antigravity/start", any(agw_login_start_route))
        .route("/login/antigravity/submit", any(agw_login_submit_route))
        .route("/gemini/accounts.json", any(gemini_accounts_route))
        .route("/gemini/quota.json", any(gemini_quota_json_route))
        .route("/minimax/quota.json", any(minimax_quota_json_route))
        .route("/deepseek/quota.json", any(deepseek_quota_json_route))
        .route("/login/gemini/start", any(gemini_login_start_route))
        .route("/login/gemini/submit", any(gemini_login_submit_route))
        .route("/qwen/accounts.json", any(qwen_accounts_route))
        .route("/qwen/quota.json", any(qwen_quota_json_route))
        .route("/login/qwen/start", any(qwen_login_start_route))
        .route("/login/qwen/submit", any(qwen_login_submit_route))
        .route("/login/qwen/status", any(qwen_login_status_route))
        .route("/login/:provider/callback", any(oauth_login_callback_route))
        .route("/deepseek/accounts.json", any(deepseek_accounts_route))
        .route("/login/deepseek/start", any(deepseek_login_start_route))
        .route("/grok/accounts.json", any(grok_accounts_route))
        .route("/login/grok/start", any(grok_login_start_route))
        .route("/login/grok/submit", any(grok_login_submit_route))
        .route("/login/grok/status", any(grok_login_status_route))
        .route("/minimax/accounts.json", any(minimax_accounts_route))
        .route("/login/minimax/start", any(minimax_login_start_route))
        .route("/usage/summary.json", any(usage_summary_route))
        .route("/usage/history.json", any(usage_history_route))
        .route("/usage/context-history.json", any(context_history_route))
        .route("/temp-files/:name", any(temp_file_route))
        .route("/docs", any(source::openapi::swagger_ui_redirect))
        .route("/docs/", any(source::openapi::swagger_ui_root))
        .route("/docs/*rest", any(source::openapi::swagger_ui_asset))
        .route("/api-docs/openapi.json", any(source::openapi::openapi_json))
        .route("/*path", any(proxy))
        .with_state(state.clone());

    let addr: SocketAddr = state.cfg.listen.parse().expect("invalid listen address");
    info!("listening on {}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await.unwrap(), app)
        .await
        .unwrap();
}

/// Returns `ok` when the gateway process is running.
#[utoipa::path(
    get,
    path = "/health",
    responses((status = 200, description = "Gateway health check", body = String))
)]
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Serves the HTML dashboard at the root path.
#[utoipa::path(
    get,
    path = "/",
    responses((status = 200, description = "Dashboard HTML", body = String))
)]
async fn dashboard_root() -> impl IntoResponse {
    dashboard().await
}

/// Serves the HTML dashboard page.
#[utoipa::path(
    get,
    path = "/dashboard",
    responses((status = 200, description = "Dashboard HTML", body = String))
)]
async fn dashboard() -> impl IntoResponse {
    let html = r###"<!doctype html>
<html>
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Codex Gateway Dashboard</title>
    <style>
      :root {
        color-scheme: dark;
        --bg: #09111f;
        --surface: #111b2e;
        --surface-alt: #182338;
        --surface-raised: #0f1729;
        --border: #25314a;
        --text: #ecf2ff;
        --muted: #94a3b8;
        --button-bg: #3b82f6;
        --button-hover: #2563eb;
        --button-text: #eff6ff;
        --secondary-bg: #182338;
        --secondary-hover: #22304a;
        --secondary-text: #dbe7ff;
        --code-bg: rgba(15, 23, 42, 0.85);
        --overlay: rgba(2, 6, 23, 0.72);
        --row-hover: rgba(148, 163, 184, 0.08);
        --tip-bg: #e2e8f0;
        --tip-text: #0f172a;
        --shadow: 0 18px 40px rgba(2, 6, 23, 0.34);
      }
      :root[data-theme="light"] {
        color-scheme: light;
        --bg: #f3f6fb;
        --surface: #ffffff;
        --surface-alt: #eef2f7;
        --surface-raised: #f8fafc;
        --border: #d5ddeb;
        --text: #111827;
        --muted: #5b6474;
        --button-bg: #111827;
        --button-hover: #1f2937;
        --button-text: #f9fafb;
        --secondary-bg: #ffffff;
        --secondary-hover: #eef2f7;
        --secondary-text: #111827;
        --code-bg: #edf2f8;
        --overlay: rgba(15, 23, 42, 0.45);
        --row-hover: rgba(15, 23, 42, 0.05);
        --tip-bg: #111827;
        --tip-text: #f8fafc;
        --shadow: 0 18px 40px rgba(15, 23, 42, 0.12);
      }
      * { box-sizing: border-box; }
      body {
        font-family: Arial, sans-serif;
        margin: 0;
        font-size: 16px;
        min-height: 100vh;
        background: var(--bg);
        color: var(--text);
      }
      .page-shell { padding: 24px; }
      h1 { margin: 0 0 12px 0; }
      h2 { margin: 0; }
      table { border-collapse: collapse; width: 100%; background: var(--surface); }
      th, td { border: 1px solid var(--border); padding: 8px; text-align: left; }
      th { background: var(--surface-alt); }
      code, pre {
        background: var(--code-bg);
        color: var(--text);
      }
      code {
        padding: 2px 6px;
        border-radius: 6px;
      }
      pre {
        padding: 10px 12px;
        border-radius: 10px;
        border: 1px solid var(--border);
      }
      button, input {
        font-size: 14px;
        font-family: inherit;
      }
      button {
        border: 1px solid transparent;
        background: var(--button-bg);
        color: var(--button-text);
        padding: 9px 14px;
        border-radius: 10px;
        cursor: pointer;
      }
      button:hover {
        background: var(--button-hover);
      }
      input {
        width: min(100%, 560px);
        padding: 10px 12px;
        border-radius: 10px;
        border: 1px solid var(--border);
        background: var(--surface);
        color: var(--text);
      }
      input::placeholder { color: var(--muted); }
      label {
        display: block;
        font-weight: 600;
      }
      .page-header,
      .section-header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 12px;
      }
      .section-header { margin-bottom: 8px; }
      .header-actions,
      .modal-actions {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-wrap: wrap;
      }
      .section { margin-top: 28px; }
      .table-wrap {
        overflow-x: auto;
        border: 1px solid var(--border);
        border-radius: 14px;
        box-shadow: var(--shadow);
        background: var(--surface);
      }
      .muted { color: var(--muted); font-size: 12px; }
      .stacked { line-height: 1.35; white-space: nowrap; }
      .weekly-line { font-size: calc(1em - 4px); }
      .row-label {
        display: flex;
        align-items: center;
        gap: 6px;
        width: 100%;
      }
      .count-pill,
      .expander {
        color: var(--muted);
        font-size: 12px;
      }
      .count-pill { margin-left: auto; }
      .expander {
        min-width: 12px;
        text-align: center;
      }
      .help-trigger { cursor: help; }
      .secondary-button {
        background: var(--secondary-bg);
        color: var(--secondary-text);
        border-color: var(--border);
      }
      .secondary-button:hover { background: var(--secondary-hover); }
      .dot-button,
      .dot-indicator {
        display: inline-block;
        width: 10px;
        height: 10px;
        border-radius: 50%;
      }
      .dot-button {
        border: none;
        padding: 0;
        min-width: 10px;
      }
      .dot-button:hover {
        opacity: 0.85;
      }
      .icon-button {
        border: none;
        background: transparent;
        color: var(--muted);
        padding: 0 0 0 4px;
        line-height: 1;
      }
      .icon-button:hover {
        background: transparent;
        color: var(--text);
      }
      .clickable-row { cursor: pointer; }
      .clickable-row:hover { background: var(--row-hover); }
      .detail-row > td { padding: 0; }
      .detail-panel {
        padding: 12px 10px 14px 10px;
        background: var(--surface-raised);
      }
      .detail-table-wrap {
        overflow-x: auto;
        margin-top: 8px;
        width: 100%;
      }
      .detail-table {
        width: 100%;
        min-width: 560px;
      }
      .modal {
        position: fixed;
        inset: 0;
        z-index: 1000;
        background: var(--overlay);
        padding: 24px 12px;
      }
      #adminLoginGate {
        z-index: 1100;
      }
      .modal-card {
        background: var(--surface);
        max-width: 720px;
        margin: 8% auto;
        padding: 16px;
        border-radius: 16px;
        max-height: 80vh;
        overflow: auto;
        border: 1px solid var(--border);
        box-shadow: var(--shadow);
      }
      .auth-url {
        display: none;
        white-space: pre-wrap;
        word-break: break-all;
        overflow-wrap: anywhere;
      }
      .tap-tip {
        position: absolute;
        background: var(--tip-bg);
        color: var(--tip-text);
        border-radius: 6px;
        padding: 6px 8px;
        font-size: 12px;
        line-height: 1.25;
        white-space: nowrap;
        z-index: 9999;
        box-shadow: 0 4px 12px rgba(0,0,0,0.25);
      }
      @media (max-width: 768px) {
        body { font-size: 17px; }
        .page-shell { padding: 12px; }
        h1 { font-size: 22px; }
        th, td { padding: 10px; font-size: 15px; }
        .muted { font-size: 13px; }
        input, button { font-size: 16px; }
        .page-header,
        .section-header {
          align-items: flex-start;
          flex-direction: column;
        }
        .modal {
          padding: 12px;
        }
        .modal-card {
          margin: 0 auto;
          max-height: calc(100vh - 24px);
        }
      }
      .chart-section {
        margin: 20px 0 28px 0;
        background: var(--surface);
        border: 1px solid var(--border);
        border-radius: 14px;
        padding: 16px;
        box-shadow: var(--shadow);
      }
      .chart-header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 12px;
        margin-bottom: 8px;
      }
      .chart-legend {
        display: flex;
        gap: 16px;
        flex-wrap: wrap;
        font-size: 13px;
      }
      .chart-legend-item {
        display: flex;
        align-items: center;
        gap: 4px;
        cursor: pointer;
        user-select: none;
      }
      .chart-legend-dot {
        display: inline-block;
        width: 10px;
        height: 10px;
        border-radius: 2px;
      }
      .chart-wrap { position: relative; height: 300px; }
      .provider-menu-item:hover { background: var(--row-hover); }
      .card {
        background: var(--surface);
        border: 1px solid var(--border);
        border-radius: 14px;
        box-shadow: var(--shadow);
        padding: 20px;
        margin-bottom: 16px;
        transition: border-color 0.2s;
      }
      .card:hover { border-color: var(--muted); }
      .card-header {
        display: flex;
        align-items: center;
        gap: 10px;
        margin-bottom: 12px;
        flex-wrap: wrap;
      }
      .card-email { font-weight: 700; font-size: 15px; }
      .card-actions { margin-left: auto; display: flex; gap: 6px; align-items: center; }
      .stat-pills {
        display: flex;
        gap: 10px;
        flex-wrap: wrap;
        margin-bottom: 14px;
      }
      .stat-pill {
        display: flex;
        align-items: center;
        gap: 5px;
        background: var(--surface-alt);
        border: 1px solid var(--border);
        border-radius: 10px;
        padding: 6px 12px;
        font-size: 13px;
        white-space: nowrap;
      }
      .stat-pill-icon { font-size: 16px; }
      .stat-pill-value { font-weight: 700; color: var(--text); }
      .stat-pill-label { color: var(--muted); }
      .card-chart-wrap {
        position: relative;
        height: 180px;
        margin-bottom: 14px;
        background: var(--surface-raised);
        border-radius: 10px;
        padding: 8px;
        border: 1px solid var(--border);
      }
      .card-chart-placeholder {
        display: flex;
        align-items: center;
        justify-content: center;
        height: 100%;
        color: var(--muted);
        font-size: 13px;
      }
      .card-quota {
        display: flex;
        gap: 16px;
        flex-wrap: wrap;
        font-size: 13px;
        margin-bottom: 10px;
      }
      .quota-bar-wrap {
        flex: 1;
        min-width: 180px;
      }
      .quota-bar-label {
        display: flex;
        justify-content: space-between;
        margin-bottom: 3px;
      }
      .quota-bar-label span:last-child { color: var(--muted); }
      .quota-bar {
        height: 8px;
        background: var(--surface-alt);
        border-radius: 4px;
        overflow: hidden;
      }
      .quota-bar-fill {
        height: 100%;
        border-radius: 4px;
        transition: width 0.4s;
      }
      .quota-bar-fill.low { background: #22c55e; }
      .quota-bar-fill.mid { background: #f59e0b; }
      .quota-bar-fill.high { background: #ef4444; }
      .account-models {
        clear: both;
        margin-top: 10px;
        line-height: 1.45;
      }
      .account-models code {
        display: block;
        box-sizing: border-box;
        margin-top: 4px;
        max-width: 100%;
        white-space: normal;
        overflow-wrap: anywhere;
        word-break: break-word;
      }
      .provider-section {
        margin-top: 28px;
      }
      .provider-badge {
        display: inline-flex;
        align-items: center;
        gap: 6px;
        background: var(--surface-raised);
        border: 1px solid var(--border);
        border-radius: 10px;
        padding: 10px 16px;
        margin-bottom: 12px;
        font-weight: 700;
        font-size: 14px;
      }
      .provider-badge-count {
        background: var(--surface-alt);
        border-radius: 20px;
        padding: 2px 10px;
        font-size: 12px;
        color: var(--muted);
      }
      .empty-state {
        text-align: center;
        padding: 28px;
        color: var(--muted);
        font-style: italic;
      }
      .mini-btn {
        font-size: 12px;
        padding: 4px 8px;
        border-radius: 6px;
        background: var(--secondary-bg);
        color: var(--secondary-text);
        border-color: var(--border);
      }
      .mini-btn:hover { background: var(--secondary-hover); }
      .mini-btn.danger { color: #ef4444; }
      .mini-btn.danger:hover { background: #ef4444; color: #fff; }
      .card-model-legend {
        display: flex;
        gap: 10px;
        flex-wrap: wrap;
        margin-bottom: 6px;
        font-size: 11px;
      }
      .card-model-legend-item {
        display: flex;
        align-items: center;
        gap: 3px;
        cursor: pointer;
      }
      .card-model-legend-dot {
        width: 8px; height: 8px; border-radius: 2px; display: inline-block;
      }
      .admin-login-card {
        max-width: 420px;
        margin: 6vh auto;
      }
      .admin-login-copy {
        margin: 0 0 14px 0;
        line-height: 1.5;
      }
      .admin-login-form {
        display: flex;
        flex-direction: column;
        gap: 12px;
      }
    </style>
    <script>
      (() => {
        try {
          const savedTheme = localStorage.getItem('gpt-gateway-theme');
          document.documentElement.setAttribute('data-theme', savedTheme === 'light' ? 'light' : 'dark');
        } catch (_) {
          document.documentElement.setAttribute('data-theme', 'dark');
        }
      })();
    </script>
    <script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.7/dist/chart.umd.min.js"></script>
  </head>
  <body>
    <div id="adminLoginGate" class="modal" style="display:none;">
      <div class="modal-card admin-login-card">
        <h2 style="margin-top:0;">Admin Login</h2>
        <p class="admin-login-copy">Enter the management API key and the 6-digit OTP from Google Authenticator to manage accounts.</p>
        <form id="adminLoginForm" class="admin-login-form">
          <div>
            <label>API Key</label>
            <input id="adminApiKeyInput" name="api_key" type="password" autocomplete="current-password" placeholder="Enter management API key">
          </div>
          <div>
            <label>Google Authenticator OTP</label>
            <input id="adminOtpInput" name="otp" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9]*" placeholder="123456">
          </div>
          <button type="submit">Log in</button>
          <div id="adminLoginStatus" class="muted"></div>
        </form>
      </div>
    </div>
    <div class="page-shell">
      <h1 class="page-header">
        <span>Codex Gateway Usage</span>
        <span class="header-actions">
          <button type="button" id="themeToggleBtn" class="secondary-button">Theme: Dark</button>
          <button type="button" id="logoutBtn" class="secondary-button" style="display:none;">Log out</button>
          <div style="position:relative;">
            <button id="addProviderBtn">+ Add account</button>
            <div id="providerMenu" style="display:none;position:absolute;right:0;top:100%;z-index:100;background:var(--surface);border:1px solid var(--border);border-radius:12px;box-shadow:var(--shadow);min-width:200px;margin-top:4px;overflow:hidden;">
              <div class="provider-menu-item" data-provider="codex" style="padding:10px 16px;cursor:pointer;border-bottom:1px solid var(--border);">Codex (ChatGPT)</div>
              <div class="provider-menu-item" data-provider="antigravity" style="padding:10px 16px;cursor:pointer;border-bottom:1px solid var(--border);">Antigravity (Google)</div>
              <div class="provider-menu-item" data-provider="gemini" style="padding:10px 16px;cursor:pointer;border-bottom:1px solid var(--border);">Gemini (Google)</div>
              <div class="provider-menu-item" data-provider="qwen" style="padding:10px 16px;cursor:pointer;border-bottom:1px solid var(--border);">Qwen</div>
              <div class="provider-menu-item" data-provider="deepseek" style="padding:10px 16px;cursor:pointer;border-bottom:1px solid var(--border);">DeepSeek</div>
              <div class="provider-menu-item" data-provider="minimax" style="padding:10px 16px;cursor:pointer;border-bottom:1px solid var(--border);">MiniMax</div>
              <div class="provider-menu-item" data-provider="grok" style="padding:10px 16px;cursor:pointer;">Grok (xAI)</div>
            </div>
          </div>
        </span>
      </h1>
      <div id="totals" class="muted"></div>
      <div class="chart-section">
        <div class="chart-header">
          <h2 style="margin:0;">Context Usage (24h)</h2>
          <div class="chart-legend" id="chartLegend"></div>
        </div>
        <div class="chart-wrap"><canvas id="contextChart"></canvas></div>
      </div>
      <div class="provider-section">
        <div class="provider-badge">
          <span>Codex</span>
          <span class="provider-badge-count" id="codexBadgeCount">0 accounts</span>
        </div>
        <div id="codexCards"></div>
      </div>
      <div class="provider-section">
        <div class="provider-badge">
          <span>Antigravity</span>
          <span class="provider-badge-count" id="agwBadgeCount">0 accounts</span>
        </div>
        <div id="agwCards"></div>
      </div>
      <div class="provider-section">
        <div class="provider-badge">
          <span>Gemini</span>
          <span class="provider-badge-count" id="geminiBadgeCount">0 accounts</span>
        </div>
        <div id="geminiCards"></div>
      </div>
      <div class="provider-section">
        <div class="provider-badge">
          <span>Qwen</span>
          <span class="provider-badge-count" id="qwenBadgeCount">0 accounts</span>
        </div>
        <div id="qwenCards"></div>
      </div>
      <div class="provider-section">
        <div class="provider-badge">
          <span>DeepSeek</span>
          <span class="provider-badge-count" id="deepseekBadgeCount">0 accounts</span>
        </div>
        <div id="deepseekCards"></div>
      </div>
      <div class="provider-section">
        <div class="provider-badge">
          <span>MiniMax</span>
          <span class="provider-badge-count" id="minimaxBadgeCount">0 accounts</span>
        </div>
        <div id="minimaxCards"></div>
      </div>
      <div class="provider-section">
        <div class="provider-badge">
          <span>Grok (xAI)</span>
          <span class="provider-badge-count" id="grokBadgeCount">0 accounts</span>
        </div>
        <div id="grokCards"></div>
      </div>
    </div>
    <script>
      let adminAuthEnabled = false;
      let adminAuthenticated = false;
      let adminAuthEpoch = 0;
      let dashboardIntervalsStarted = false;
      let lastQuota = new Map();
      let lastAgwQuota = new Map();
      let lastGeminiQuota = new Map();
      let lastQwenQuota = new Map();
      let openAgwRows = new Set();
      let openGeminiRows = new Set();
      let openQwenRows = new Set();
      let activeTipEl = null;
      let activeTipTimer = null;
      const THEME_KEY = 'gpt-gateway-theme';
      function normalizeTheme(theme) {
        return theme === 'light' ? 'light' : 'dark';
      }
      function readStoredTheme() {
        try {
          return localStorage.getItem(THEME_KEY);
        } catch (_) {
          return null;
        }
      }
      function writeStoredTheme(theme) {
        try {
          localStorage.setItem(THEME_KEY, theme);
        } catch (_) {}
      }
      function closeProviderMenu() {
        const menu = document.getElementById('providerMenu');
        if (menu) {
          menu.style.display = 'none';
        }
      }
      function showAdminLogin(message) {
        adminAuthenticated = false;
        adminAuthEpoch += 1;
        closeProviderMenu();
        document.getElementById('adminLoginGate').style.display = 'block';
        document.getElementById('logoutBtn').style.display = 'none';
        document.getElementById('adminLoginStatus').textContent = message || '';
      }
      function hideAdminLogin() {
        document.getElementById('adminLoginGate').style.display = 'none';
        document.getElementById('adminLoginStatus').textContent = '';
        if (adminAuthEnabled) {
          document.getElementById('logoutBtn').style.display = 'inline-block';
        }
      }
      async function adminFetch(url, options) {
        const requestEpoch = adminAuthEpoch;
        const res = await fetch(url, Object.assign({ credentials: 'same-origin' }, options || {}));
        if (res.status === 401) {
          if (requestEpoch === adminAuthEpoch) {
            showAdminLogin('Session expired. Log in again.');
          }
          return null;
        }
        return res;
      }
      async function bootstrapAdmin() {
        const res = await fetch('/admin/session', { credentials: 'same-origin' });
        const data = await res.json();
        adminAuthEnabled = !!data.enabled;
        adminAuthenticated = !!data.authenticated || !adminAuthEnabled;
        if (!adminAuthEnabled) {
          hideAdminLogin();
          startDashboard();
          return;
        }
        if (!data.configured) {
          showAdminLogin('Admin auth is enabled but not configured. Set admin_auth.totp_secret or ADMIN_AUTH_TOTP_SECRET.');
          return;
        }
        if (adminAuthenticated) {
          hideAdminLogin();
          startDashboard();
          return;
        }
        showAdminLogin('Enter the management API key and your current Google Authenticator code.');
      }
      function setThemeToggleLabel(theme) {
        const btn = document.getElementById('themeToggleBtn');
        if (!btn) return;
        btn.textContent = theme === 'light' ? 'Theme: Light' : 'Theme: Dark';
      }
      function applyTheme(theme) {
        const resolved = normalizeTheme(theme);
        document.documentElement.setAttribute('data-theme', resolved);
        setThemeToggleLabel(resolved);
        return resolved;
      }
      function loadTheme() {
        return applyTheme(readStoredTheme());
      }
      function toggleTheme() {
        const current = normalizeTheme(document.documentElement.getAttribute('data-theme'));
        const next = current === 'dark' ? 'light' : 'dark';
        writeStoredTheme(next);
        applyTheme(next);
      }
      loadTheme();
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
      function toggleAgwModelRow(key) {
        if (openAgwRows.has(key)) {
          openAgwRows.delete(key);
        } else {
          openAgwRows.add(key);
        }
        refreshAgwAccounts();
      }
      function toggleGeminiModelRow(key) {
        if (openGeminiRows.has(key)) {
          openGeminiRows.delete(key);
        } else {
          openGeminiRows.add(key);
        }
        refreshGeminiAccounts();
      }
      function toggleQwenModelRow(key) {
        if (openQwenRows.has(key)) {
          openQwenRows.delete(key);
        } else {
          openQwenRows.add(key);
        }
        refreshQwenAccounts();
      }
      function renderQuotaBars(quota) {
        if (!quota) return '';
        var fmtQ = function(b, fallback) { return b && b.used_percent != null ? b.used_percent.toFixed(1) + '% ' + (b.reset_label || '') : (fallback || '...'); };
        var bars = '';
        // Codex-style quota
        if (quota.code_generation) {
          var cg5 = quota.code_generation.five_hour, cgw = quota.code_generation.weekly;
          var cr5 = quota.code_review?.five_hour, crw = quota.code_review?.weekly;
          bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>Code Gen 5h</span><span>' + fmtQ(cg5) + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (cg5 && cg5.used_percent > 80 ? 'high' : cg5 && cg5.used_percent > 50 ? 'mid' : 'low') + '" style="width:' + (cg5 ? cg5.used_percent || 0 : 0) + '%;"></div></div></div>';
          bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>Code Gen Weekly</span><span>' + fmtQ(cgw) + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (cgw && cgw.used_percent > 80 ? 'high' : cgw && cgw.used_percent > 50 ? 'mid' : 'low') + '" style="width:' + (cgw ? cgw.used_percent || 0 : 0) + '%;"></div></div></div>';
          bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>Code Review 5h</span><span>' + fmtQ(cr5) + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (cr5 && cr5.used_percent > 80 ? 'high' : cr5 && cr5.used_percent > 50 ? 'mid' : 'low') + '" style="width:' + (cr5 ? cr5.used_percent || 0 : 0) + '%;"></div></div></div>';
          bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>Code Review Weekly</span><span>' + fmtQ(crw) + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (crw && crw.used_percent > 80 ? 'high' : crw && crw.used_percent > 50 ? 'mid' : 'low') + '" style="width:' + (crw ? crw.used_percent || 0 : 0) + '%;"></div></div></div>';
        }
        // Provider-style quota (groups + models from AGW/Gemini/Qwen)
        if (quota.models) {
          quota.models.forEach(function(m) {
            var b = m.current || m.quota || m.limit || null;
            if (!b || (b.used_percent == null && b.limit == null && b.remaining == null && !b.limit_text && !b.remaining_text && !b.used_text)) {
              return;
            }
            bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>' + (m.display_name || m.model_id || 'Model') + '</span><span>' + fmtQ(b, 'N/A') + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (b && b.used_percent > 80 ? 'high' : b && b.used_percent > 50 ? 'mid' : 'low') + '" style="width:' + (b ? b.used_percent || 0 : 0) + '%;"></div></div></div>';
          });
        }
        if (quota.groups) {
          quota.groups.forEach(function(g) {
            bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>' + (g.display_name || 'Group') + ' 5h</span><span>' + fmtQ(g.five_hour, 'N/A') + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (g.five_hour && g.five_hour.used_percent > 80 ? 'high' : g.five_hour && g.five_hour.used_percent > 50 ? 'mid' : 'low') + '" style="width:' + (g.five_hour ? g.five_hour.used_percent || 0 : 0) + '%;"></div></div></div>';
            bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>' + (g.display_name || 'Group') + ' Weekly</span><span>' + fmtQ(g.weekly, 'N/A') + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (g.weekly && g.weekly.used_percent > 80 ? 'high' : g.weekly && g.weekly.used_percent > 50 ? 'mid' : 'low') + '" style="width:' + (g.weekly ? g.weekly.used_percent || 0 : 0) + '%;"></div></div></div>';
          });
        }
        // Qwen-style rate limits
        if (quota.limits) {
          quota.limits.forEach(function(l) {
            var label = l.label || l.scope || 'Limit';
            var pct = l.used_percent != null ? l.used_percent : 0;
            var hint = (l.used_text || l.used || '') + '/' + (l.limit_text || l.limit || '') + ' ' + (l.reset_label || '');
            bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>' + label + '</span><span>' + hint + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (pct > 80 ? 'high' : pct > 50 ? 'mid' : 'low') + '" style="width:' + pct + '%;"></div></div></div>';
          });
        }
        // MiniMax top-level current_window / weekly (matches the
        // platform.minimax.io/console/usage layout: two big bars per
        // account: "5h" and "Weekly" with a "resets in" countdown).
        if (quota.current_window) {
          var cw = quota.current_window;
          var cwPct = cw.used_percent != null ? cw.used_percent : 0;
          var cwHint = (cw.used_percent != null ? cw.used_percent.toFixed(1) + '%' : '\u2014')
            + ' used \u00b7 resets in ' + (cw.reset_label || '\u2014');
          bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>5h window</span><span>' + cwHint + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (cwPct > 80 ? 'high' : cwPct > 50 ? 'mid' : 'low') + '" style="width:' + cwPct + '%;"></div></div></div>';
        }
        if (quota.weekly) {
          var wk = quota.weekly;
          var wkPct = wk.used_percent != null ? wk.used_percent : 0;
          var wkHint = (wk.used_percent != null ? wk.used_percent.toFixed(1) + '%' : '\u2014')
            + ' used \u00b7 resets in ' + (wk.reset_label || '\u2014');
          bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>Weekly window</span><span>' + wkHint + '</span></div><div class="quota-bar"><div class="quota-bar-fill ' + (wkPct > 80 ? 'high' : wkPct > 50 ? 'mid' : 'low') + '" style="width:' + wkPct + '%;"></div></div></div>';
        }
        // DeepSeek balances: show total + topped-up in USD. The
        // gateway pulls this from /user/balance.
        if (quota.balances && quota.balances.length) {
          quota.balances.forEach(function(b) {
            if (b.total_balance == null) return;
            var label = 'Balance ' + (b.currency || 'USD');
            var detail = b.total_balance + ' ' + (b.currency || 'USD');
            if (b.topped_up_balance && b.topped_up_balance !== '0.00' && b.topped_up_balance !== '0') {
              detail += ' (topped up ' + b.topped_up_balance + ')';
            } else if (b.granted_balance && b.granted_balance !== '0.00' && b.granted_balance !== '0') {
              detail += ' (granted ' + b.granted_balance + ')';
            }
            bars += '<div class="quota-bar-wrap"><div class="quota-bar-label"><span>' + label + '</span><span>' + detail + '</span></div><div class="quota-bar"><div class="quota-bar-fill low" style="width:100%;"></div></div></div>';
          });
        }
        if (quota.status_msg) {
          bars += '<div class="muted" style="margin-top:4px;">' + quota.status_msg + '</div>';
        }
        return bars ? '<div class="card-quota">' + bars + '</div>' : '';
      }
      function escapeHtml(value) {
        return String(value).replace(/[&<>"']/g, function(ch) {
          return ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[ch];
        });
      }
      function modelLabel(model) {
        if (!model) return '';
        if (typeof model === 'string') return model.trim();
        var id = model.model_id || model.id || model.slug || model.model || model.model_name || model.name || '';
        return String(id).trim();
      }
      function appendModelLabels(out, seen, models) {
        if (!models || !models.length) return;
        models.forEach(function(model) {
          var label = modelLabel(model);
          if (!label || seen.has(label)) return;
          seen.add(label);
          out.push(label);
        });
      }
      function renderAccountModels(a, quota) {
        var labels = [];
        var seen = new Set();
        appendModelLabels(labels, seen, a && a.models);
        appendModelLabels(labels, seen, quota && quota.available_models);
        appendModelLabels(labels, seen, quota && quota.models);
        appendModelLabels(labels, seen, quota && quota.data);
        return labels.length
          ? '<div class="muted account-models">models:<code>' + labels.map(escapeHtml).join(' | ') + '</code></div>'
          : '';
      }
      function buildCard(a, quota) {
        var dot = a.enabled ? '#2ecc71' : '#e74c3c';
        var toggleLabel = a.enabled ? 'Disable' : 'Enable';
        var actions = '';
        if (a.file_name) {
          actions += '<button title="' + toggleLabel + '" onclick="toggleCred(\'' + a.file_name + '\', ' + (a.enabled ? 'false' : 'true') + ')" class="mini-btn" style="background:' + dot + ';color:#fff;">&#9679;</button>';
          actions += '<button title="Delete" onclick="deleteCred(\'' + a.file_name + '\')" class="mini-btn danger">&#128465;</button>';
        } else {
          actions += '<span class="dot-indicator" style="background:' + dot + ';"></span>';
        }
        return '<div class="card">'
          + '<div class="card-header"><span class="card-email">' + a.label + '</span><span class="card-actions">' + actions + '</span></div>'
          + '<div class="stat-pills">'
          + '<span class="stat-pill"><span class="stat-pill-value">' + (a.requests || 0) + '</span><span class="stat-pill-label">req</span></span>'
          + '<span class="stat-pill"><span class="stat-pill-value">' + (a.errors || 0) + '</span><span class="stat-pill-label">err</span></span>'
          + '</div>'
          + renderQuotaBars(quota)
          + renderAccountModels(a, quota)
          + '</div>';
      }
      async function refresh() {
        const res = await adminFetch('/dashboard.json');
        if (!res) return;
        const data = await res.json();
        document.getElementById('totals').textContent =
          'Total requests: ' + data.total_requests + ' | Total errors: ' + data.total_errors;
        var cards = data.accounts.map(function(a) { return buildCard(a, lastQuota.get(a.file_name || a.label)); }).join('');
        document.getElementById('codexCards').innerHTML = cards || '<div class="empty-state">No Codex accounts</div>';
        document.getElementById('codexBadgeCount').textContent = data.accounts.length + ' accounts';
      }
      async function refreshQuota() {
        const res = await adminFetch('/quota.json');
        if (!res) return;
        const quota = await res.json();
        const quotaMap = new Map();
        (quota.accounts || []).forEach(q => {
          const key = q.file_name || q.label;
          quotaMap.set(key, q);
        });
        lastQuota = quotaMap;
        refresh();
      }
      function buildProviderCard(a, quota) {
        var dot = a.enabled ? '#2ecc71' : '#e74c3c';
        var toggleLabel = a.enabled ? 'Disable' : 'Enable';
        var actions = '';
        if (a.file_name) {
          actions += '<button title="' + toggleLabel + '" onclick="toggleCred(\'' + a.file_name + '\', ' + (a.enabled ? 'false' : 'true') + ')" class="mini-btn" style="background:' + dot + ';color:#fff;">&#9679;</button>';
          actions += '<button title="Delete" onclick="deleteCred(\'' + a.file_name + '\')" class="mini-btn danger">&#128465;</button>';
        } else {
          actions += '<span class="dot-indicator" style="background:' + dot + ';"></span>';
        }
        var extra = '';
        if (a.email) extra += '<span class="stat-pill"><span class="stat-pill-label">email</span><span class="stat-pill-value">' + a.email + '</span></span>';
        if (a.project_id) extra += '<span class="stat-pill"><span class="stat-pill-label">project</span><span class="stat-pill-value"><code>' + a.project_id + '</code></span></span>';
        return '<div class="card">'
          + '<div class="card-header"><span class="card-email">' + (a.label || a.email || a.account_id || 'N/A') + '</span><span class="card-actions">' + actions + '</span></div>'
          + '<div class="stat-pills">'
          + '<span class="stat-pill"><span class="stat-pill-value">' + (a.requests || 0) + '</span><span class="stat-pill-label">req</span></span>'
          + '<span class="stat-pill"><span class="stat-pill-value">' + (a.errors || 0) + '</span><span class="stat-pill-label">err</span></span>'
          + extra + '</div>'
          + renderQuotaBars(quota)
          + renderAccountModels(a, quota)
          + '</div>';
      }
      function buildQwenCard(a, quota) {
        var dot = a.enabled ? '#2ecc71' : '#e74c3c';
        var toggleLabel = a.enabled ? 'Disable' : 'Enable';
        var actions = '';
        if (a.file_name) {
          actions += '<button title="' + toggleLabel + '" onclick="toggleCred(\'' + a.file_name + '\', ' + (a.enabled ? 'false' : 'true') + ')" class="mini-btn" style="background:' + dot + ';color:#fff;">&#9679;</button>';
          actions += '<button title="Delete" onclick="deleteCred(\'' + a.file_name + '\')" class="mini-btn danger">&#128465;</button>';
        } else {
          actions += '<span class="dot-indicator" style="background:' + dot + ';"></span>';
        }
        var resource = a.resource_url || 'https://portal.qwen.ai/v1';
        var meta = '';
        meta += '<div class="muted">resource: <code>' + resource + '</code></div>';
        if (a.expired_at) {
          meta += '<div class="muted">saved token expiry: ' + a.expired_at + '</div>';
        }
        if (a.last_success_at) {
          meta += '<div class="muted">last success: ' + a.last_success_at + '</div>';
        }
        if (a.last_error_at) {
          meta += '<div class="muted">last error: ' + a.last_error_at + '</div>';
        }
        var usage = '';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.requests || 0) + '</span><span class="stat-pill-label">req</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.errors || 0) + '</span><span class="stat-pill-label">err</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.prompt_total || 0) + '</span><span class="stat-pill-label">prompt</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.input_tokens || 0) + '</span><span class="stat-pill-label">in tok</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.output_tokens || 0) + '</span><span class="stat-pill-label">out tok</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.total_tokens || 0) + '</span><span class="stat-pill-label">total tok</span></span>';
        if (a.cache_tokens) {
          usage += '<span class="stat-pill"><span class="stat-pill-value">' + a.cache_tokens + '</span><span class="stat-pill-label">cache tok</span></span>';
        }
        if (a.reasoning_tokens) {
          usage += '<span class="stat-pill"><span class="stat-pill-value">' + a.reasoning_tokens + '</span><span class="stat-pill-label">reason tok</span></span>';
        }
        if (a.email) {
          usage += '<span class="stat-pill"><span class="stat-pill-label">email</span><span class="stat-pill-value">' + a.email + '</span></span>';
        }
        return '<div class="card">'
          + '<div class="card-header"><span class="card-email">' + (a.label || a.email || a.account_id || 'N/A') + '</span><span class="card-actions">' + actions + '</span></div>'
          + '<div class="stat-pills">' + usage + '</div>'
          + renderQuotaBars(quota)
          + renderAccountModels(a, quota)
          + meta
          + '</div>';
      }
      function buildGrokCard(a) {
        var dot = a.enabled ? '#2ecc71' : '#e74c3c';
        var toggleLabel = a.enabled ? 'Disable' : 'Enable';
        var actions = '';
        if (a.file_name) {
          actions += '<button title="' + toggleLabel + '" onclick="toggleCred(\'' + a.file_name + '\', ' + (a.enabled ? 'false' : 'true') + ')" class="mini-btn" style="background:' + dot + ';color:#fff;">&#9679;</button>';
          actions += '<button title="Delete" onclick="deleteCred(\'' + a.file_name + '\')" class="mini-btn danger">&#128465;</button>';
        } else {
          actions += '<span class="dot-indicator" style="background:' + dot + ';"></span>';
        }
        var usage = '';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.requests || 0) + '</span><span class="stat-pill-label">req</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.errors || 0) + '</span><span class="stat-pill-label">err</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.prompt_total || 0) + '</span><span class="stat-pill-label">prompt</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.input_tokens || 0) + '</span><span class="stat-pill-label">in tok</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.output_tokens || 0) + '</span><span class="stat-pill-label">out tok</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.total_tokens || 0) + '</span><span class="stat-pill-label">total tok</span></span>';
        if (a.reasoning_tokens) {
          usage += '<span class="stat-pill"><span class="stat-pill-value">' + a.reasoning_tokens + '</span><span class="stat-pill-label">reason tok</span></span>';
        }
        if (a.email) {
          usage += '<span class="stat-pill"><span class="stat-pill-label">email</span><span class="stat-pill-value">' + a.email + '</span></span>';
        }
        if (a.last_effective_model) {
          usage += '<span class="stat-pill"><span class="stat-pill-label">model</span><span class="stat-pill-value"><code>' + a.last_effective_model + '</code></span></span>';
        }
        var meta = '';
        if (a.user_id) {
          meta += '<div class="muted">user id: <code>' + a.user_id + '</code></div>';
        }
        if (a.team_id) {
          meta += '<div class="muted">team id: <code>' + a.team_id + '</code>' + (a.team_blocked ? ' (blocked)' : '') + '</div>';
        }
        if (a.zdr_status) {
          meta += '<div class="muted">zdr: <code>' + a.zdr_status + '</code></div>';
        }
        if (a.expired_at) {
          meta += '<div class="muted">saved token expiry: ' + a.expired_at + '</div>';
        }
        if (a.last_success_at) {
          meta += '<div class="muted">last success: ' + a.last_success_at + '</div>';
        }
        if (a.last_error_at) {
          meta += '<div class="muted">last error: ' + a.last_error_at + '</div>';
        }
        meta += renderAccountModels(a, null);
        return '<div class="card">'
          + '<div class="card-header"><span class="card-email">' + (a.name || a.label || a.email || a.account_id || 'N/A') + '</span><span class="card-actions">' + actions + '</span></div>'
          + '<div class="stat-pills">' + usage + '</div>'
          + renderQuotaBars({ limits: a.rate_limits || [] })
          + meta
          + '</div>';
      }
      async function refreshAgwAccounts() {
        var res = await adminFetch('/agw/accounts.json');
        if (!res) return;
        var data = await res.json();
        var accounts = data.accounts || [];
        var cards = accounts.map(function(a) { return buildProviderCard(a, lastAgwQuota.get(a.file_name || a.label)); }).join('');
        document.getElementById('agwCards').innerHTML = cards || '<div class="empty-state">No Antigravity accounts</div>';
        document.getElementById('agwBadgeCount').textContent = accounts.length + ' accounts';
      }
      async function refreshAgwQuota() {
        const res = await adminFetch('/agw/quota.json');
        if (!res) return;
        const quota = await res.json();
        const quotaMap = new Map();
        (quota.accounts || []).forEach(q => { quotaMap.set(q.file_name || q.label, q); });
        lastAgwQuota = quotaMap;
        refreshAgwAccounts();
      }
      async function refreshGeminiAccounts() {
        var res = await adminFetch('/gemini/accounts.json');
        if (!res) return;
        var data = await res.json();
        var accounts = data.accounts || [];
        var cards = accounts.map(function(a) { return buildProviderCard(a, lastGeminiQuota.get(a.file_name || a.label)); }).join('');
        document.getElementById('geminiCards').innerHTML = cards || '<div class="empty-state">No Gemini accounts</div>';
        document.getElementById('geminiBadgeCount').textContent = accounts.length + ' accounts';
      }
      async function refreshGeminiQuota() {
        const res = await adminFetch('/gemini/quota.json');
        if (!res) return;
        const quota = await res.json();
        const quotaMap = new Map();
        (quota.accounts || []).forEach(q => { quotaMap.set(q.file_name || q.label, q); });
        lastGeminiQuota = quotaMap;
        refreshGeminiAccounts();
      }
      async function refreshQwenAccounts() {
        var res = await adminFetch('/qwen/accounts.json');
        if (!res) return;
        var data = await res.json();
        var accounts = data.accounts || [];
        var cards = accounts.map(function(a) { return buildQwenCard(a, lastQwenQuota.get(a.file_name || a.label)); }).join('');
        document.getElementById('qwenCards').innerHTML = cards || '<div class="empty-state">No Qwen accounts</div>';
        document.getElementById('qwenBadgeCount').textContent = accounts.length + ' accounts';
      }
      async function refreshQwenQuota() {
        const res = await adminFetch('/qwen/quota.json');
        if (!res) return;
        const quota = await res.json();
        const quotaMap = new Map();
        (quota.accounts || []).forEach(q => { quotaMap.set(q.file_name || q.label, q); });
        lastQwenQuota = quotaMap;
        refreshQwenAccounts();
      }
      async function refreshDeepSeekAccounts() {
        var res = await adminFetch('/deepseek/accounts.json');
        if (!res) return;
        var data = await res.json();
        var accounts = data.accounts || [];
        var cards = accounts.map(function(a) { return buildProviderCard(a, lastDeepSeekQuota.get(a.file_name || a.label)); }).join('');
        document.getElementById('deepseekCards').innerHTML = cards || '<div class="empty-state">No DeepSeek accounts</div>';
        document.getElementById('deepseekBadgeCount').textContent = accounts.length + ' accounts';
      }
      async function refreshDeepSeekQuota() {
        const res = await adminFetch('/deepseek/quota.json');
        if (!res) return;
        const quota = await res.json();
        const quotaMap = new Map();
        (quota.accounts || []).forEach(q => { quotaMap.set(q.file_name || q.label, q); });
        lastDeepSeekQuota = quotaMap;
        refreshDeepSeekAccounts();
      }
      function buildMiniMaxCard(a, quota) {
        var dot = a.enabled ? '#2ecc71' : '#e74c3c';
        var toggleLabel = a.enabled ? 'Disable' : 'Enable';
        var actions = '';
        if (a.file_name) {
          actions += '<button title="' + toggleLabel + '" onclick="toggleCred(\'' + a.file_name + '\', ' + (a.enabled ? 'false' : 'true') + ')" class="mini-btn" style="background:' + dot + ';color:#fff;">&#9679;</button>';
          actions += '<button title="Delete" onclick="deleteCred(\'' + a.file_name + '\')" class="mini-btn danger">&#128465;</button>';
        } else {
          actions += '<span class="dot-indicator" style="background:' + dot + ';"></span>';
        }
        var usage = '';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.requests || 0) + '</span><span class="stat-pill-label">req</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.errors || 0) + '</span><span class="stat-pill-label">err</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.prompt_total || 0) + '</span><span class="stat-pill-label">prompt</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.input_tokens || 0) + '</span><span class="stat-pill-label">in tok</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.output_tokens || 0) + '</span><span class="stat-pill-label">out tok</span></span>';
        usage += '<span class="stat-pill"><span class="stat-pill-value">' + (a.total_tokens || 0) + '</span><span class="stat-pill-label">total tok</span></span>';
        var meta = '';
        if (a.base_url) {
          meta += '<div class="muted">base URL: <code>' + a.base_url + '</code></div>';
        }
        if (a.last_success_at) {
          meta += '<div class="muted">last success: ' + a.last_success_at + '</div>';
        }
        if (a.last_error_at) {
          meta += '<div class="muted">last error: ' + a.last_error_at + '</div>';
        }
        return '<div class="card">'
          + '<div class="card-header"><span class="card-email">' + (a.label || a.account_id || 'MiniMax') + '</span><span class="card-actions">' + actions + '</span></div>'
          + '<div class="stat-pills">' + usage + '</div>'
          + renderQuotaBars(quota)
          + renderAccountModels(a, quota)
          + meta
          + '</div>';
      }
      let lastMiniMaxQuota = new Map();
      let lastDeepSeekQuota = new Map();
      async function refreshMiniMaxAccounts() {
        var res = await adminFetch('/minimax/accounts.json');
        if (!res) return;
        var data = await res.json();
        var accounts = data.accounts || [];
        var cards = accounts.map(function(a) { return buildMiniMaxCard(a, lastMiniMaxQuota.get(a.file_name || a.label)); }).join('');
        document.getElementById('minimaxCards').innerHTML = cards || '<div class="empty-state">No MiniMax accounts</div>';
        document.getElementById('minimaxBadgeCount').textContent = accounts.length + ' accounts';
      }
      async function refreshMiniMaxQuota() {
        const res = await adminFetch('/minimax/quota.json');
        if (!res) return;
        const quota = await res.json();
        const quotaMap = new Map();
        (quota.accounts || []).forEach(q => { quotaMap.set(q.file_name || q.label, q); });
        lastMiniMaxQuota = quotaMap;
        refreshMiniMaxAccounts();
      }
      async function refreshGrokAccounts() {
        var res = await adminFetch('/grok/accounts.json');
        if (!res) return;
        var data = await res.json();
        var accounts = data.accounts || [];
        var cards = accounts.map(buildGrokCard).join('');
        document.getElementById('grokCards').innerHTML = cards || '<div class="empty-state">No Grok accounts</div>';
        document.getElementById('grokBadgeCount').textContent = accounts.length + ' accounts';
      }
      let contextChart = null;
      const chartColors = {
        input: '#3b82f6',
        output: '#22c55e',
        cache: '#f59e0b',
        reasoning: '#a855f7'
      };
      function ensureChartDestroyed() {
        if (contextChart) {
          contextChart.destroy();
          contextChart = null;
        }
      }
      async function refreshContextChart() {
        try {
          const res = await adminFetch('/usage/context-history.json?hours=24&bucket_minutes=5');
          if (!res) return;
          const data = await res.json();
          const labels = data.labels || [];
          const buckets = data.buckets || [];

          const inputData = [];
          const outputData = [];
          const cacheData = [];
          const reasoningData = [];
          for (const b of buckets) {
            inputData.push(b.input_tokens || 0);
            outputData.push(b.output_tokens || 0);
            cacheData.push(b.cache_tokens || 0);
            reasoningData.push(b.reasoning_tokens || 0);
          }

          const canvas = document.getElementById('contextChart');
          if (!canvas) return;

          ensureChartDestroyed();
          const ctx = canvas.getContext('2d');
          contextChart = new Chart(ctx, {
            type: 'line',
            data: {
              labels: labels,
              datasets: [
                {
                  label: 'Input',
                  data: inputData,
                  borderColor: chartColors.input,
                  backgroundColor: chartColors.input + '18',
                  borderWidth: 2,
                  pointRadius: 0,
                  pointHitRadius: 6,
                  tension: 0.2,
                  fill: true
                },
                {
                  label: 'Output',
                  data: outputData,
                  borderColor: chartColors.output,
                  backgroundColor: chartColors.output + '18',
                  borderWidth: 2,
                  pointRadius: 0,
                  pointHitRadius: 6,
                  tension: 0.2,
                  fill: true
                },
                {
                  label: 'Cache',
                  data: cacheData,
                  borderColor: chartColors.cache,
                  borderWidth: 1.5,
                  borderDash: [5, 3],
                  pointRadius: 0,
                  pointHitRadius: 6,
                  tension: 0.2,
                  fill: false
                },
                {
                  label: 'Reasoning',
                  data: reasoningData,
                  borderColor: chartColors.reasoning,
                  borderWidth: 1.5,
                  borderDash: [5, 3],
                  pointRadius: 0,
                  pointHitRadius: 6,
                  tension: 0.2,
                  fill: false
                }
              ]
            },
            options: {
              responsive: true,
              maintainAspectRatio: false,
              animation: false,
              interaction: {
                mode: 'index',
                intersect: false
              },
              plugins: {
                legend: { display: false },
                tooltip: {
                  callbacks: {
                    label: function(ctx) {
                      return ctx.dataset.label + ': ' + ctx.parsed.y.toLocaleString() + ' tokens';
                    }
                  }
                }
              },
              scales: {
                x: {
                  display: true,
                  ticks: { maxTicksLimit: 24, color: getComputedStyle(document.documentElement).getPropertyValue('--muted').trim() || '#94a3b8' },
                  grid: { display: false }
                },
                y: {
                  display: true,
                  beginAtZero: true,
                  ticks: {
                    callback: function(v) { return v >= 1000000 ? (v/1000000).toFixed(1)+'M' : v >= 1000 ? (v/1000).toFixed(0)+'K' : v; },
                    color: getComputedStyle(document.documentElement).getPropertyValue('--muted').trim() || '#94a3b8'
                  },
                  grid: { color: (getComputedStyle(document.documentElement).getPropertyValue('--border').trim() || '#25314a') + '40' }
                }
              }
            }
          });

          const legendDiv = document.getElementById('chartLegend');
          if (legendDiv) {
            legendDiv.innerHTML = [
              { key: 'input', label: 'Input' },
              { key: 'output', label: 'Output' },
              { key: 'cache', label: 'Cache' },
              { key: 'reasoning', label: 'Reasoning' }
            ].map(function(item) {
              return '<span class="chart-legend-item" data-key="' + item.key + '">'
                + '<span class="chart-legend-dot" style="background:' + chartColors[item.key] + ';"></span>'
                + item.label
                + '</span>';
            }).join('');
            legendDiv.querySelectorAll('.chart-legend-item').forEach(function(el) {
              el.addEventListener('click', function() {
                const key = el.getAttribute('data-key');
                const meta = contextChart.getDatasetMeta(
                  ['input','output','cache','reasoning'].indexOf(key)
                );
                meta.hidden = !meta.hidden;
                el.style.opacity = meta.hidden ? '0.4' : '1';
                contextChart.update();
              });
            });
          }
        } catch (_) {
          // Chart refresh is best-effort
        }
      }
    </script>
    <div id="addModal" class="modal" style="display:none;">
      <div class="modal-card">
        <h2 style="margin-top:0;">Add Codex Account</h2>
        <p>Click start, open the URL in a new tab, complete login, then paste the callback URL below.</p>
        <button onclick="startLogin()">Start Login</button>
        <div id="status" class="muted" style="margin-top:8px;"></div>
        <pre id="authUrl" class="auth-url"></pre>
        <form id="loginForm" style="margin-top:16px;">
          <label>Callback URL</label>
          <input name="redirect_url" placeholder="http://localhost:1455/auth/callback?code=...&state=...">
          <div class="modal-actions" style="margin-top:8px;">
            <button type="submit">Submit</button>
            <button type="button" id="closeModalBtn" class="secondary-button">Close</button>
          </div>
        </form>
      </div>
    </div>
    <div id="addAgwModal" class="modal" style="display:none;">
      <div class="modal-card">
        <h2 style="margin-top:0;">Add Antigravity Account</h2>
        <p>Click start, log in with Google, then paste the callback URL below.</p>
        <button onclick="startAgwLogin()">Start Login</button>
        <div id="agwStatus" class="muted" style="margin-top:8px;"></div>
        <pre id="agwAuthUrl" class="auth-url"></pre>
        <form id="agwLoginForm" style="margin-top:16px;">
          <label>Callback URL</label>
          <input name="redirect_url" placeholder="http://localhost:51121/oauth-callback?code=...&state=...">
          <div class="modal-actions" style="margin-top:8px;">
            <button type="submit">Submit</button>
            <button type="button" id="closeAgwModalBtn" class="secondary-button">Close</button>
          </div>
        </form>
      </div>
    </div>
    <div id="addGeminiModal" class="modal" style="display:none;">
      <div class="modal-card">
        <h2 style="margin-top:0;">Add Gemini Account</h2>
        <p>Click start, complete Google OAuth, then paste the final callback URL below. If your Google account has multiple Cloud projects, provide one project ID.</p>
        <button onclick="startGeminiLogin()">Start Login</button>
        <div id="geminiStatus" class="muted" style="margin-top:8px;"></div>
        <pre id="geminiAuthUrl" class="auth-url"></pre>
        <form id="geminiLoginForm" style="margin-top:16px;">
          <label>Callback URL</label>
          <input name="redirect_url" placeholder="http://localhost:8085/oauth2callback?code=...&state=...">
          <label style="margin-top:12px;">Project ID</label>
          <input name="project_id" placeholder="optional, but recommended when multiple GCP projects exist">
          <div class="muted" style="margin-top:8px;">Leave Project ID empty to let the gateway use the detected project. If multiple projects exist and no default is exposed, login will ask you to retry with one explicit project ID.</div>
          <div class="modal-actions" style="margin-top:8px;">
            <button type="submit">Submit</button>
            <button type="button" id="closeGeminiModalBtn" class="secondary-button">Close</button>
          </div>
        </form>
      </div>
    </div>
    <div id="addQwenModal" class="modal" style="display:none;">
      <div class="modal-card">
        <h2 style="margin-top:0;">Add Qwen Account</h2>
        <p>Open the local Qwen token helper first. It explains the same browser-token flow used by <code>qwen-api</code> and gives you the extractor snippet for <code>chat.qwen.ai</code>.</p>
        <div class="modal-actions" style="margin-top:8px;">
          <button type="button" onclick="startQwenLogin()">Open Token Helper</button>
        </div>
        <p class="muted" style="margin-top:12px;">Direct fallback: open <code>chat.qwen.ai</code>, copy <code>localStorage.token</code> from the browser console, and paste it here.</p>
        <textarea id="qwenTokenInput" rows="6" placeholder="Paste chat.qwen.ai token here" style="width:100%;box-sizing:border-box;font-family:monospace;"></textarea>
        <button onclick="submitQwenToken()" style="margin-top:12px;">Save Token</button>
        <div id="qwenStatus" class="muted" style="margin-top:8px;"></div>
        <div class="modal-actions" style="margin-top:16px;">
          <button type="button" id="closeQwenModalBtn" class="secondary-button">Close</button>
        </div>
      </div>
    </div>
    <div id="addDeepSeekModal" class="modal" style="display:none;">
      <div class="modal-card">
        <h2 style="margin-top:0;">Add DeepSeek Account</h2>
        <p>Paste a DeepSeek API key. The gateway validates it against <code>/models</code> before saving it.</p>
        <div class="modal-actions" style="margin-top:8px;">
          <button type="button" onclick="window.open('/login/deepseek/start', '_blank', 'noopener')">Open Helper</button>
        </div>
        <label style="margin-top:12px;">API Key</label>
        <textarea id="deepseekKeyInput" rows="6" placeholder="Paste DeepSeek API key here" style="width:100%;box-sizing:border-box;font-family:monospace;"></textarea>
        <label style="margin-top:12px;">Label</label>
        <input id="deepseekLabelInput" placeholder="optional label">
        <label style="margin-top:12px;">Base URL</label>
        <input id="deepseekBaseUrlInput" placeholder="https://api.deepseek.com">
        <button onclick="submitDeepSeekKey()" style="margin-top:12px;">Save Key</button>
        <div id="deepseekStatus" class="muted" style="margin-top:8px;"></div>
        <div class="modal-actions" style="margin-top:16px;">
          <button type="button" id="closeDeepSeekModalBtn" class="secondary-button">Close</button>
        </div>
      </div>
    </div>
    <div id="addMiniMaxModal" class="modal" style="display:none;">
      <div class="modal-card">
        <h2 style="margin-top:0;">Add MiniMax Account</h2>
        <p>Paste a MiniMax API key. The gateway validates it against <code>/v1/models</code> before saving it.</p>
        <div class="modal-actions" style="margin-top:8px;">
          <button type="button" onclick="window.open('/login/minimax/start', '_blank', 'noopener')">Open Helper</button>
        </div>
        <label style="margin-top:12px;">API Key</label>
        <textarea id="minimaxKeyInput" rows="6" placeholder="Paste MiniMax API key here" style="width:100%;box-sizing:border-box;font-family:monospace;"></textarea>
        <label style="margin-top:12px;">Label</label>
        <input id="minimaxLabelInput" placeholder="optional label">
        <label style="margin-top:12px;">Base URL</label>
        <input id="minimaxBaseUrlInput" placeholder="https://api.minimaxi.chat">
        <button onclick="submitMiniMaxKey()" style="margin-top:12px;">Save Key</button>
        <div id="minimaxStatus" class="muted" style="margin-top:8px;"></div>
        <div class="modal-actions" style="margin-top:16px;">
          <button type="button" id="closeMiniMaxModalBtn" class="secondary-button">Close</button>
        </div>
      </div>
    </div>
    <div id="addGrokModal" class="modal" style="display:none;">
      <div class="modal-card">
        <h2 style="margin-top:0;">Add Grok Account</h2>
        <p>Click start, open the URL in a new tab, complete login with your SuperGrok or X Premium+ account, then paste the callback URL, the <code>?code=...&amp;state=...</code> fragment, or just the authorization code if xAI shows a completion page instead of redirecting.</p>
        <button onclick="startGrokLogin()">Start Login</button>
        <div id="grokStatus" class="muted" style="margin-top:8px;"></div>
        <pre id="grokAuthUrl" class="auth-url"></pre>
        <form id="grokLoginForm" style="margin-top:16px;">
          <label>Callback URL or Authorization Code</label>
          <input name="redirect_url" placeholder="http://127.0.0.1:56121/callback?code=...&state=... or paste bare code">
          <input type="hidden" name="state" value="">
          <div class="modal-actions" style="margin-top:8px;">
            <button type="submit">Submit</button>
            <button type="button" id="closeGrokModalBtn" class="secondary-button">Close</button>
          </div>
        </form>
      </div>
    </div>
    <script>
      async function startLogin() {
        const res = await adminFetch('/login/codex/start');
        if (!res) return;
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
      document.getElementById('themeToggleBtn').addEventListener('click', toggleTheme);
      // Provider selector menu
      document.getElementById('addProviderBtn').addEventListener('click', function(e) {
        e.stopPropagation();
        var menu = document.getElementById('providerMenu');
        menu.style.display = menu.style.display === 'none' ? 'block' : 'none';
      });
      document.addEventListener('click', function() {
        closeProviderMenu();
      });
      document.getElementById('providerMenu').addEventListener('click', function(e) {
        e.stopPropagation();
      });
      document.querySelectorAll('.provider-menu-item').forEach(function(item) {
        item.addEventListener('click', function() {
          document.getElementById('providerMenu').style.display = 'none';
          var provider = item.getAttribute('data-provider');
          if (provider === 'codex') document.getElementById('addModal').style.display = 'block';
          else if (provider === 'antigravity') document.getElementById('addAgwModal').style.display = 'block';
          else if (provider === 'gemini') document.getElementById('addGeminiModal').style.display = 'block';
          else if (provider === 'qwen') document.getElementById('addQwenModal').style.display = 'block';
          else if (provider === 'deepseek') document.getElementById('addDeepSeekModal').style.display = 'block';
          else if (provider === 'minimax') document.getElementById('addMiniMaxModal').style.display = 'block';
          else if (provider === 'grok') document.getElementById('addGrokModal').style.display = 'block';
        });
      });
      document.getElementById('closeModalBtn').addEventListener('click', () => {
        document.getElementById('addModal').style.display = 'none';
      });
      document.getElementById('addModal').addEventListener('click', (e) => {
        if (e.target.id === 'addModal') {
          document.getElementById('addModal').style.display = 'none';
        }
      });
      async function startAgwLogin() {
        const res = await adminFetch('/login/antigravity/start');
        if (!res) return;
        const data = await res.json();
        if (data.url) {
          window.open(data.url, '_blank');
          document.getElementById('agwStatus').textContent = 'Opened login URL in new tab. If blocked, copy from below.';
          const pre = document.getElementById('agwAuthUrl');
          pre.textContent = data.url;
          pre.style.display = 'block';
        } else {
          document.getElementById('agwStatus').textContent = data.message || 'Failed to start login';
        }
      }
      document.getElementById('closeAgwModalBtn').addEventListener('click', () => {
        document.getElementById('addAgwModal').style.display = 'none';
      });
      document.getElementById('addAgwModal').addEventListener('click', (e) => {
        if (e.target.id === 'addAgwModal') {
          document.getElementById('addAgwModal').style.display = 'none';
        }
      });
      async function startGeminiLogin() {
        const res = await adminFetch('/login/gemini/start');
        if (!res) return;
        const data = await res.json();
        if (data.url) {
          window.open(data.url, '_blank');
          document.getElementById('geminiStatus').textContent = 'Opened login URL in new tab. If blocked, copy from below.';
          const pre = document.getElementById('geminiAuthUrl');
          pre.textContent = data.url;
          pre.style.display = 'block';
        } else {
          document.getElementById('geminiStatus').textContent = data.message || 'Failed to start Gemini login';
        }
      }
      document.getElementById('closeGeminiModalBtn').addEventListener('click', () => {
        document.getElementById('addGeminiModal').style.display = 'none';
      });
      document.getElementById('addGeminiModal').addEventListener('click', (e) => {
        if (e.target.id === 'addGeminiModal') {
          document.getElementById('addGeminiModal').style.display = 'none';
        }
      });
      function closeQwenModal() {
        document.getElementById('addQwenModal').style.display = 'none';
      }
      function startQwenLogin() {
        const popup = window.open('/login/qwen/start', '_blank', 'noopener');
        if (!popup) {
          window.location.href = '/login/qwen/start';
          return;
        }
        document.getElementById('qwenStatus').textContent = 'Opened the Qwen token helper in a new tab. Follow the browser-token steps there, or use the direct token fallback below.';
      }
      async function submitQwenToken() {
        const token = document.getElementById('qwenTokenInput').value.trim();
        if (!token) {
          document.getElementById('qwenStatus').textContent = 'Paste a Qwen browser token first.';
          return;
        }
        const res = await adminFetch('/login/qwen/start', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json'
          },
          body: JSON.stringify({ token })
        });
        if (!res) return;
        const data = await res.json();
        document.getElementById('qwenStatus').textContent = data.message || 'Failed to save Qwen token';
        if (!data.ok) {
          return;
        }
        document.getElementById('qwenTokenInput').value = '';
        refreshQwenAccounts();
      }
      document.getElementById('closeQwenModalBtn').addEventListener('click', () => {
        closeQwenModal();
      });
      document.getElementById('addQwenModal').addEventListener('click', (e) => {
        if (e.target.id === 'addQwenModal') {
          closeQwenModal();
        }
      });
      function closeDeepSeekModal() {
        document.getElementById('addDeepSeekModal').style.display = 'none';
      }
      async function submitDeepSeekKey() {
        const apiKey = document.getElementById('deepseekKeyInput').value.trim();
        const label = document.getElementById('deepseekLabelInput').value.trim();
        const baseUrl = document.getElementById('deepseekBaseUrlInput').value.trim();
        if (!apiKey) {
          document.getElementById('deepseekStatus').textContent = 'Paste a DeepSeek API key first.';
          return;
        }
        const res = await adminFetch('/login/deepseek/start', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json'
          },
          body: JSON.stringify({
            api_key: apiKey,
            label: label || undefined,
            base_url: baseUrl || undefined
          })
        });
        if (!res) return;
        const data = await res.json();
        document.getElementById('deepseekStatus').textContent = data.message || 'Failed to save DeepSeek key';
        if (!data.ok) {
          return;
        }
        document.getElementById('deepseekKeyInput').value = '';
        document.getElementById('deepseekLabelInput').value = '';
        document.getElementById('deepseekBaseUrlInput').value = '';
        refreshDeepSeekAccounts();
      }
      document.getElementById('closeDeepSeekModalBtn').addEventListener('click', () => {
        closeDeepSeekModal();
      });
      document.getElementById('addDeepSeekModal').addEventListener('click', (e) => {
        if (e.target.id === 'addDeepSeekModal') {
          closeDeepSeekModal();
        }
      });
      function closeMiniMaxModal() {
        document.getElementById('addMiniMaxModal').style.display = 'none';
      }
      async function submitMiniMaxKey() {
        const apiKey = document.getElementById('minimaxKeyInput').value.trim();
        const label = document.getElementById('minimaxLabelInput').value.trim();
        const baseUrl = document.getElementById('minimaxBaseUrlInput').value.trim();
        if (!apiKey) {
          document.getElementById('minimaxStatus').textContent = 'Paste a MiniMax API key first.';
          return;
        }
        const res = await adminFetch('/login/minimax/start', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json'
          },
          body: JSON.stringify({
            api_key: apiKey,
            label: label || undefined,
            base_url: baseUrl || undefined
          })
        });
        if (!res) return;
        const data = await res.json();
        document.getElementById('minimaxStatus').textContent = data.message || 'Failed to save MiniMax key';
        if (!data.ok) {
          return;
        }
        document.getElementById('minimaxKeyInput').value = '';
        document.getElementById('minimaxLabelInput').value = '';
        document.getElementById('minimaxBaseUrlInput').value = '';
        refreshMiniMaxAccounts();
      }
      document.getElementById('closeMiniMaxModalBtn').addEventListener('click', () => {
        closeMiniMaxModal();
      });
      document.getElementById('addMiniMaxModal').addEventListener('click', (e) => {
        if (e.target.id === 'addMiniMaxModal') {
          closeMiniMaxModal();
        }
      });
      // Grok modal
      function closeGrokModal() {
        document.getElementById('addGrokModal').style.display = 'none';
        const form = document.getElementById('grokLoginForm');
        form.querySelector('input[name="state"]').value = '';
        form.querySelector('input[name="redirect_url"]').value = '';
      }
      async function startGrokLogin() {
        document.getElementById('grokStatus').textContent = 'Starting login...';
        const res = await adminFetch('/login/grok/start');
        if (!res) return;
        const data = await res.json();
        if (data.url) {
          document.getElementById('grokLoginForm').querySelector('input[name="state"]').value = data.state || '';
          window.open(data.url, '_blank');
          document.getElementById('grokStatus').textContent = 'Opened login URL. Complete login, then paste the callback URL or the bare authorization code if xAI shows it directly.';
          const pre = document.getElementById('grokAuthUrl');
          pre.textContent = data.url;
          pre.style.display = 'block';
        } else {
          document.getElementById('grokStatus').textContent = data.message || 'Failed to start Grok login';
        }
      }
      document.getElementById('closeGrokModalBtn').addEventListener('click', () => {
        closeGrokModal();
      });
      document.getElementById('addGrokModal').addEventListener('click', (e) => {
        if (e.target.id === 'addGrokModal') {
          closeGrokModal();
        }
      });
      document.getElementById('grokLoginForm').addEventListener('submit', async (e) => {
        e.preventDefault();
        const form = e.target;
        const input = form.querySelector('input[name="redirect_url"]');
        const stateInput = form.querySelector('input[name="state"]');
        const redirectUrl = input.value.trim();
        if (!redirectUrl) {
          document.getElementById('grokStatus').textContent = 'Callback URL or authorization code is required.';
          return;
        }
        const res = await adminFetch('/login/grok/submit', {
          method: 'POST',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({ redirect_url: redirectUrl, state: stateInput.value || '' })
        });
        if (!res) return;
        const data = await res.json();
        document.getElementById('grokStatus').textContent = data.message || 'Login completed.';
        if (data.ok) {
          input.value = '';
          stateInput.value = '';
          refreshGrokAccounts();
          refreshMiniMaxAccounts();
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
        const res = await adminFetch('/login/codex/submit', {
          method: 'POST',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({ redirect_url: redirectUrl })
        });
        if (!res) return;
        const data = await res.json();
        document.getElementById('status').textContent = data.message || 'Login completed.';
        if (data.ok) {
          refresh();
        }
      });
      document.getElementById('agwLoginForm').addEventListener('submit', async (e) => {
        e.preventDefault();
        const form = e.target;
        const input = form.querySelector('input[name="redirect_url"]');
        const redirectUrl = input.value.trim();
        if (!redirectUrl) {
          document.getElementById('agwStatus').textContent = 'Callback URL is required.';
          return;
        }
        const res = await adminFetch('/login/antigravity/submit', {
          method: 'POST',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({ redirect_url: redirectUrl })
        });
        if (!res) return;
        const data = await res.json();
        document.getElementById('agwStatus').textContent = data.message || 'Login completed.';
        if (data.ok) {
          refreshAgwQuota();
          refreshAgwAccounts();
        }
      });
      document.getElementById('geminiLoginForm').addEventListener('submit', async (e) => {
        e.preventDefault();
        const form = e.target;
        const redirectInput = form.querySelector('input[name="redirect_url"]');
        const projectInput = form.querySelector('input[name="project_id"]');
        const redirectUrl = redirectInput.value.trim();
        const projectId = projectInput.value.trim();
        if (!redirectUrl) {
          document.getElementById('geminiStatus').textContent = 'Callback URL is required.';
          return;
        }
        const res = await adminFetch('/login/gemini/submit', {
          method: 'POST',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({ redirect_url: redirectUrl, project_id: projectId })
        });
        if (!res) return;
        const data = await res.json();
        document.getElementById('geminiStatus').textContent = data.message || 'Login completed.';
        if (data.ok) {
          refreshGeminiQuota();
          refreshGeminiAccounts();
        }
      });
      async function deleteCred(fileName) {
        const res = await adminFetch('/credentials/delete', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/x-www-form-urlencoded'
          },
          body: new URLSearchParams({ file_name: fileName })
        });
        if (!res) return;
        const data = await res.json();
        alert(data.message || 'done');
        refresh();
        refreshQuota();
        refreshAgwQuota();
        refreshAgwAccounts();
        refreshGeminiQuota();
        refreshGeminiAccounts();
        refreshQwenQuota();
        refreshQwenAccounts();
        refreshDeepSeekAccounts();
        refreshDeepSeekQuota();
        refreshGrokAccounts();
        refreshMiniMaxAccounts();
        refreshMiniMaxQuota();
      }
      async function toggleCred(fileName, enabled) {
        const res = await adminFetch('/credentials/toggle', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/x-www-form-urlencoded'
          },
          body: new URLSearchParams({ file_name: fileName, enabled: enabled ? 'true' : 'false' })
        });
        if (!res) return;
        const data = await res.json();
        alert(data.message || 'done');
        refresh();
        refreshQuota();
        refreshAgwQuota();
        refreshAgwAccounts();
        refreshGeminiQuota();
        refreshGeminiAccounts();
        refreshQwenQuota();
        refreshQwenAccounts();
        refreshDeepSeekAccounts();
        refreshDeepSeekQuota();
        refreshGrokAccounts();
        refreshMiniMaxAccounts();
        refreshMiniMaxQuota();
      }
      async function startDashboard() {
        refresh();
        refreshContextChart();
        refreshQuota();
        refreshAgwQuota().then(() => refreshAgwAccounts());
        refreshGeminiQuota().then(() => refreshGeminiAccounts());
        refreshQwenQuota().then(() => refreshQwenAccounts());
        refreshDeepSeekAccounts();
        refreshDeepSeekQuota();
        refreshGrokAccounts();
        refreshMiniMaxAccounts();
        refreshMiniMaxQuota();
        if (dashboardIntervalsStarted) {
          return;
        }
        dashboardIntervalsStarted = true;
        setInterval(() => { if (adminAuthenticated) refresh(); }, 5000);
        setInterval(() => { if (adminAuthenticated) refreshQuota(); }, 60000);
        setInterval(() => { if (adminAuthenticated) refreshAgwQuota(); }, 60000);
        setInterval(() => { if (adminAuthenticated) refreshGeminiQuota(); }, 60000);
        setInterval(() => { if (adminAuthenticated) refreshQwenQuota(); }, 60000);
        setInterval(() => { if (adminAuthenticated) refreshDeepSeekQuota(); }, 60000);
        setInterval(() => { if (adminAuthenticated) refreshMiniMaxQuota(); }, 60000);
        setInterval(() => { if (adminAuthenticated) refreshAgwAccounts(); }, 10000);
        setInterval(() => { if (adminAuthenticated) refreshGeminiAccounts(); }, 10000);
        setInterval(() => { if (adminAuthenticated) refreshQwenAccounts(); }, 10000);
        setInterval(() => { if (adminAuthenticated) refreshDeepSeekAccounts(); }, 10000);
        setInterval(() => { if (adminAuthenticated) refreshMiniMaxAccounts(); }, 10000);
        setInterval(() => { if (adminAuthenticated) refreshGrokAccounts(); }, 10000);
        setInterval(() => { if (adminAuthenticated) refreshContextChart(); }, 60000);
      }
      document.getElementById('adminLoginForm').addEventListener('submit', async (e) => {
        e.preventDefault();
        const apiKey = document.getElementById('adminApiKeyInput').value.trim();
        const otp = document.getElementById('adminOtpInput').value.trim();
        if (!apiKey || !otp) {
          document.getElementById('adminLoginStatus').textContent = 'API key and OTP are required.';
          return;
        }
        const res = await fetch('/admin/login', {
          method: 'POST',
          credentials: 'same-origin',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({ api_key: apiKey, otp: otp })
        });
        const data = await res.json();
        if (!res.ok || !data.ok) {
          document.getElementById('adminLoginStatus').textContent = data.message || 'Login failed.';
          return;
        }
        adminAuthEpoch += 1;
        adminAuthenticated = true;
        document.getElementById('adminOtpInput').value = '';
        hideAdminLogin();
        startDashboard();
      });
      document.getElementById('logoutBtn').addEventListener('click', async () => {
        const res = await fetch('/admin/logout', { method: 'POST', credentials: 'same-origin' });
        if (res) {
          try { await res.json(); } catch (_) {}
        }
        showAdminLogin('Logged out.');
      });
      bootstrapAdmin();
    </script>
  </body>
</html>
"###;
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

async fn admin_session_route(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let enabled = admin_auth::is_enabled(&state.cfg.admin_auth);
    let configured = admin_auth::is_configured(&state.cfg.admin_auth, &state.cfg.proxy_api_key);
    let authenticated = if enabled {
        let mut sessions = state.admin_sessions.lock().unwrap();
        admin_auth::validate_session(&headers, &mut sessions)
    } else {
        true
    };

    axum::Json(serde_json::json!({
        "enabled": enabled,
        "configured": configured,
        "authenticated": authenticated
    }))
}

async fn admin_login_route(
    State(state): State<AppState>,
    Form(form): Form<admin_auth::LoginForm>,
) -> impl IntoResponse {
    match admin_auth::verify_login(
        &state.cfg.admin_auth,
        &state.cfg.proxy_api_key,
        &form.api_key,
        &form.otp,
        std::time::SystemTime::now(),
    ) {
        Ok(()) => {
            let ttl_seconds = admin_auth::session_ttl_seconds(&state.cfg.admin_auth);
            let session_id = {
                let mut sessions = state.admin_sessions.lock().unwrap();
                admin_auth::create_session(&mut sessions, ttl_seconds)
            };
            let mut response = axum::Json(serde_json::json!({
                "ok": true,
                "message": "logged in"
            }))
            .into_response();
            admin_auth::append_set_cookie(
                response.headers_mut(),
                &admin_auth::build_session_cookie(&session_id, ttl_seconds),
            );
            response
        }
        Err(err) => (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            })),
        )
            .into_response(),
    }
}

async fn admin_logout_route(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    {
        let mut sessions = state.admin_sessions.lock().unwrap();
        admin_auth::remove_session(&headers, &mut sessions);
    }
    let mut response = axum::Json(serde_json::json!({
        "ok": true,
        "message": "logged out"
    }))
    .into_response();
    admin_auth::append_set_cookie(response.headers_mut(), &admin_auth::clear_session_cookie());
    response
}

/// Returns the dashboard counters and per-account request totals.
#[utoipa::path(
    get,
    path = "/dashboard.json",
    responses((
        status = 200,
        description = "Dashboard JSON snapshot",
        body = crate::source::openapi::DashboardJsonResponse
    ))
)]
async fn dashboard_json(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    let snapshot = {
        let stats = state.stats.lock().unwrap();
        stats.clone()
    };
    let accounts: Vec<serde_json::Value> = snapshot
        .codex_accounts
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
        "total_prompt_total": snapshot.total_prompt_total,
        "total_prompt_error_total": snapshot.total_prompt_error_total,
        "total_input_tokens": snapshot.total_input_tokens,
        "total_output_tokens": snapshot.total_output_tokens,
        "total_tokens_used": snapshot.total_tokens_used,
        "total_cache_tokens": snapshot.total_cache_tokens,
        "total_reasoning_tokens": snapshot.total_reasoning_tokens,
        "first_recorded_at": snapshot.first_recorded_at,
        "last_recorded_at": snapshot.last_recorded_at,
        "accounts": accounts
    }))
    .into_response()
}

/// Returns cached quota usage for each configured Codex credential.
#[utoipa::path(
    get,
    path = "/quota.json",
    responses((
        status = 200,
        description = "Quota summary",
        body = crate::source::openapi::QuotaResponse
    ))
)]
async fn quota_json_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::codex::admin::quota_json(State(state))
        .await
        .into_response()
}

/// Starts the Codex OAuth login flow and returns the upstream authorization URL.
#[utoipa::path(
    get,
    path = "/login/codex/start",
    responses((
        status = 200,
        description = "OAuth login URL and state token",
        body = crate::source::openapi::LoginStartResponse
    ))
)]
async fn login_start_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::codex::admin::login_start(State(state))
        .await
        .into_response()
}

/// Accepts the OAuth callback URL and stores the resulting Codex credentials.
#[utoipa::path(
    post,
    path = "/login/codex/submit",
    request_body(
        content = crate::source::openapi::LoginSubmitRequest,
        content_type = "application/x-www-form-urlencoded",
        description = "OAuth callback URL copied from the browser redirect"
    ),
    responses((
        status = 200,
        description = "Credential save result",
        body = crate::source::openapi::ActionResponse
    ))
)]
async fn login_submit_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<target::codex::admin::CallbackForm>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::codex::admin::login_submit(State(state), Form(form))
        .await
        .into_response()
}

async fn agw_accounts_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::antigravity::admin::accounts_json(State(state))
        .await
        .into_response()
}

async fn agw_quota_json_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::antigravity::admin::quota_json(State(state))
        .await
        .into_response()
}

async fn agw_login_start_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::antigravity::admin::login_start(State(state))
        .await
        .into_response()
}

async fn agw_login_submit_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<target::antigravity::admin::CallbackForm>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::antigravity::admin::login_submit(State(state), Form(form))
        .await
        .into_response()
}

async fn gemini_accounts_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::gemini::admin::accounts_json(State(state))
        .await
        .into_response()
}

async fn gemini_quota_json_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::gemini::admin::quota_json(State(state))
        .await
        .into_response()
}

async fn minimax_quota_json_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::minimax::admin::quota_json(State(state))
        .await
        .into_response()
}

async fn deepseek_quota_json_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::deepseek::admin::quota_json(State(state))
        .await
        .into_response()
}

async fn gemini_login_start_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::gemini::admin::login_start(State(state))
        .await
        .into_response()
}

async fn gemini_login_submit_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<target::gemini::admin::CallbackForm>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::gemini::admin::login_submit(State(state), Form(form))
        .await
        .into_response()
}

async fn qwen_accounts_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::qwen::admin::accounts_json(State(state))
        .await
        .into_response()
}

async fn qwen_quota_json_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::qwen::admin::quota_json(State(state))
        .await
        .into_response()
}

async fn qwen_login_start_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    body: Bytes,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::qwen::admin::login_start(State(state), method, body)
        .await
        .into_response()
}

async fn qwen_login_submit_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<target::qwen::admin::CallbackForm>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::qwen::admin::login_submit(State(state), headers, Form(form))
        .await
        .into_response()
}

async fn qwen_login_status_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<target::qwen::admin::LoginStatusQuery>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::qwen::admin::login_status(State(state), Query(query))
        .await
        .into_response()
}

async fn oauth_login_callback_route(
    Path(provider): Path<String>,
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    if let Some(response) = require_admin_session_text(&state, &headers) {
        return response;
    }
    target::oauth::login_callback_route(state, provider, method, headers, uri)
        .await
        .into_response()
}

async fn deepseek_accounts_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::deepseek::admin::accounts_json(State(state))
        .await
        .into_response()
}

async fn minimax_accounts_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::minimax::admin::accounts_json(State(state))
        .await
        .into_response()
}

async fn minimax_login_start_route(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::minimax::admin::login_start(State(state), method, body)
        .await
        .into_response()
}

async fn deepseek_login_start_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    body: Bytes,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::deepseek::admin::login_start(State(state), method, body)
        .await
        .into_response()
}

async fn grok_accounts_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::grok::admin::accounts_json(State(state))
        .await
        .into_response()
}

async fn grok_login_start_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::grok::admin::login_start(State(state))
        .await
        .into_response()
}

async fn grok_login_submit_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<target::grok::admin::CallbackForm>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::grok::admin::login_submit(State(state), Form(form))
        .await
        .into_response()
}

async fn grok_login_status_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<target::grok::admin::LoginStatusQuery>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::grok::admin::login_status(State(state), Query(query))
        .await
        .into_response()
}

async fn usage_summary_route(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    let persisted = state.persisted_stats.lock().unwrap().clone();
    let mut codex = persisted
        .codex
        .into_iter()
        .map(|(key, usage)| serde_json::json!({ "key": key, "usage": usage }))
        .collect::<Vec<_>>();
    let mut antigravity = persisted
        .antigravity
        .into_iter()
        .map(|(key, usage)| serde_json::json!({ "key": key, "usage": usage }))
        .collect::<Vec<_>>();
    let mut gemini = persisted
        .gemini
        .into_iter()
        .map(|(key, usage)| serde_json::json!({ "key": key, "usage": usage }))
        .collect::<Vec<_>>();
    let mut qwen = persisted
        .qwen
        .into_iter()
        .map(|(key, usage)| serde_json::json!({ "key": key, "usage": usage }))
        .collect::<Vec<_>>();
    let mut deepseek = persisted
        .deepseek
        .into_iter()
        .map(|(key, usage)| serde_json::json!({ "key": key, "usage": usage }))
        .collect::<Vec<_>>();
    let mut grok = persisted
        .grok
        .into_iter()
        .map(|(key, usage)| serde_json::json!({ "key": key, "usage": usage }))
        .collect::<Vec<_>>();
    let mut minimax = persisted
        .minimax
        .into_iter()
        .map(|(key, usage)| serde_json::json!({ "key": key, "usage": usage }))
        .collect::<Vec<_>>();
    codex.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    antigravity.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    gemini.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    qwen.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    deepseek.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    grok.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    minimax.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    axum::Json(serde_json::json!({
        "totals": {
            "requests": persisted.total_requests,
            "errors": persisted.total_errors,
            "prompt_total": persisted.total_prompt_total,
            "prompt_error_total": persisted.total_prompt_error_total,
            "input_tokens": persisted.total_input_tokens,
            "output_tokens": persisted.total_output_tokens,
            "total_tokens": persisted.total_tokens_used,
            "cache_tokens": persisted.total_cache_tokens,
            "reasoning_tokens": persisted.total_reasoning_tokens,
            "first_recorded_at": persisted.first_recorded_at,
            "last_recorded_at": persisted.last_recorded_at
        },
        "providers": {
            "codex": codex,
            "antigravity": antigravity,
            "gemini": gemini,
            "qwen": qwen,
            "deepseek": deepseek,
            "grok": grok,
            "minimax": minimax
        }
    }))
    .into_response()
}

async fn usage_history_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<usage_store::UsageHistoryQuery>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    match usage_store::load(&state.cfg, &query) {
        Ok(events) => axum::Json(serde_json::json!({ "events": events })).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("Content-Type", "application/json")],
            openai_error_body(&err, "server_error", None),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ContextHistoryQuery {
    #[serde(default = "default_context_hours")]
    hours: u64,
    #[serde(default = "default_context_bucket_minutes")]
    bucket_minutes: u64,
    #[serde(default)]
    account_key: Option<String>,
    #[serde(default)]
    per_model: bool,
}

fn default_context_hours() -> u64 {
    24
}
fn default_context_bucket_minutes() -> u64 {
    5
}

async fn context_history_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ContextHistoryQuery>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    let hours = query.hours.max(1).min(720);
    let bucket_minutes = query.bucket_minutes.max(1).min(60);
    let account_filter = query
        .account_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let per_model = query.per_model;

    let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
    let cutoff_str = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let all = match usage_store::load(
        &state.cfg,
        &usage_store::UsageHistoryQuery {
            limit: None,
            provider: None,
            account_key: None,
            model: None,
        },
    ) {
        Ok(entries) => entries,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [("Content-Type", "application/json")],
                openai_error_body(&err, "server_error", None),
            )
                .into_response();
        }
    };

    let filtered: Vec<_> = all
        .iter()
        .filter(|e| e.success && e.recorded_at >= cutoff_str)
        .filter(|e| {
            if let Some(ak) = account_filter {
                e.account_key == ak
            } else {
                true
            }
        })
        .collect();

    if filtered.is_empty() {
        return axum::Json(serde_json::json!({
            "labels": [],
            "models": {}
        }))
        .into_response();
    }

    let bucket_secs = bucket_minutes * 60;
    let start_ts = cutoff.timestamp();
    let end_ts = chrono::Utc::now().timestamp();
    let num_buckets = ((end_ts - start_ts) / bucket_secs as i64).max(1) as usize;
    let num_buckets = num_buckets.min(288);

    let mut labels = Vec::with_capacity(num_buckets);
    for i in 0..num_buckets {
        let bucket_start = start_ts + (i as i64 * bucket_secs as i64);
        let dt = chrono::DateTime::from_timestamp(bucket_start, 0)
            .unwrap_or(chrono::DateTime::UNIX_EPOCH);
        labels.push(dt.format("%H:%M").to_string());
    }

    if per_model {
        // Group by model → buckets
        let mut model_data: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

        for entry in &filtered {
            let model = entry.model.as_deref().unwrap_or("unknown").to_string();
            let ts = match chrono::DateTime::parse_from_rfc3339(&entry.recorded_at) {
                Ok(dt) => dt.timestamp(),
                Err(_) => continue,
            };
            let bucket_idx = ((ts - start_ts) / bucket_secs as i64) as usize;
            if bucket_idx >= num_buckets {
                continue;
            }

            let buckets = model_data.entry(model).or_insert_with(|| {
                vec![serde_json::json!({"input": 0u64, "output": 0u64, "cache": 0u64, "reasoning": 0u64}); num_buckets]
            });

            let b = &mut buckets[bucket_idx];
            b["input"] = serde_json::json!(b["input"].as_u64().unwrap_or(0) + entry.input_tokens);
            b["output"] =
                serde_json::json!(b["output"].as_u64().unwrap_or(0) + entry.output_tokens);
            b["cache"] = serde_json::json!(b["cache"].as_u64().unwrap_or(0) + entry.cache_tokens);
            b["reasoning"] =
                serde_json::json!(b["reasoning"].as_u64().unwrap_or(0) + entry.reasoning_tokens);
        }

        return axum::Json(serde_json::json!({
            "labels": labels,
            "models": model_data
        }))
        .into_response();
    }

    // Default: aggregate all together
    let mut buckets = vec![
        serde_json::json!({
            "input_tokens": 0u64,
            "output_tokens": 0u64,
            "total_tokens": 0u64,
            "cache_tokens": 0u64,
            "reasoning_tokens": 0u64,
            "request_count": 0u64,
        });
        num_buckets
    ];

    for entry in &filtered {
        let ts = match chrono::DateTime::parse_from_rfc3339(&entry.recorded_at) {
            Ok(dt) => dt.timestamp(),
            Err(_) => continue,
        };
        let bucket_idx = ((ts - start_ts) / bucket_secs as i64) as usize;
        if bucket_idx >= num_buckets {
            continue;
        }
        let b = &mut buckets[bucket_idx];
        b["input_tokens"] =
            serde_json::json!(b["input_tokens"].as_u64().unwrap_or(0) + entry.input_tokens);
        b["output_tokens"] =
            serde_json::json!(b["output_tokens"].as_u64().unwrap_or(0) + entry.output_tokens);
        b["total_tokens"] =
            serde_json::json!(b["total_tokens"].as_u64().unwrap_or(0) + entry.total_tokens);
        b["cache_tokens"] =
            serde_json::json!(b["cache_tokens"].as_u64().unwrap_or(0) + entry.cache_tokens);
        b["reasoning_tokens"] =
            serde_json::json!(b["reasoning_tokens"].as_u64().unwrap_or(0) + entry.reasoning_tokens);
        b["request_count"] = serde_json::json!(b["request_count"].as_u64().unwrap_or(0) + 1);
    }

    axum::Json(serde_json::json!({
        "labels": labels,
        "buckets": buckets
    }))
    .into_response()
}

async fn temp_file_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(response) = require_admin_session_text(&state, &headers) {
        return response;
    }
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return (StatusCode::BAD_REQUEST, "invalid file name").into_response();
    }

    let path = std::path::Path::new("/tmp/gpt-gateway-downloads").join(&name);
    let body = match std::fs::read(&path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, "file not found").into_response();
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read temp file",
            )
                .into_response();
        }
    };

    let content_type = if name.ends_with(".png") {
        "image/png"
    } else if name.ends_with(".jpg") || name.ends_with(".jpeg") {
        "image/jpeg"
    } else if name.ends_with(".webp") {
        "image/webp"
    } else if name.ends_with(".gif") {
        "image/gif"
    } else {
        "application/octet-stream"
    };

    (
        StatusCode::OK,
        [
            ("Content-Type", content_type),
            ("Content-Disposition", "attachment"),
            ("Cache-Control", "no-store"),
        ],
        body,
    )
        .into_response()
}

/// Deletes a saved credential file and reloads the in-memory token list.
#[utoipa::path(
    post,
    path = "/credentials/delete",
    request_body(
        content = crate::source::openapi::DeleteCredentialRequest,
        content_type = "application/x-www-form-urlencoded",
        description = "Credential filename from the auth directory"
    ),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "Delete result",
            body = crate::source::openapi::ActionResponse
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = crate::source::openapi::ActionResponse
        )
    )
)]
async fn delete_credential_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<target::codex::admin::DeleteForm>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::codex::admin::delete_credential(State(state), Form(form))
        .await
        .into_response()
}

/// Enables or disables a saved credential file and persists the disabled list.
#[utoipa::path(
    post,
    path = "/credentials/toggle",
    request_body(
        content = crate::source::openapi::ToggleCredentialRequest,
        content_type = "application/x-www-form-urlencoded",
        description = "Credential filename and target enabled state"
    ),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "Toggle result",
            body = crate::source::openapi::ActionResponse
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = crate::source::openapi::ActionResponse
        )
    )
)]
async fn toggle_credential_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<target::codex::admin::ToggleForm>,
) -> Response {
    if let Some(response) = require_admin_session_json(&state, &headers) {
        return response;
    }
    target::codex::admin::toggle_credential(State(state), Form(form))
        .await
        .into_response()
}

pub(crate) fn model_from_request_value(value: &serde_json::Value) -> Option<String> {
    value
        .get("model")
        .and_then(|model| model.as_str())
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(|model| model.to_string())
}

pub(crate) fn prompt_metrics_from_request_value(value: &serde_json::Value) -> PromptMetrics {
    let mut metrics = PromptMetrics::default();
    if let Some(instructions) = value
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        metrics.input_chars += instructions.chars().count() as u64;
        metrics.prompt_items += 1;
        metrics.is_prompt = true;
    }

    if let Some(input) = value.get("input") {
        append_prompt_value(&mut metrics, input);
    }

    if let Some(messages) = value.get("messages") {
        append_prompt_value(&mut metrics, messages);
    }

    if !metrics.is_prompt {
        metrics.is_prompt = metrics.input_chars > 0 || metrics.prompt_items > 0;
    }

    metrics
}

fn append_prompt_value(metrics: &mut PromptMetrics, value: &serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let text = text.trim();
            if !text.is_empty() {
                metrics.input_chars += text.chars().count() as u64;
                metrics.prompt_items += 1;
                metrics.is_prompt = true;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                append_prompt_value(metrics, item);
            }
        }
        serde_json::Value::Object(map) => {
            let mut found_direct_text = false;
            if let Some(text) = map
                .get("text")
                .and_then(|value| value.as_str())
                .or_else(|| map.get("input_text").and_then(|value| value.as_str()))
                .or_else(|| map.get("output_text").and_then(|value| value.as_str()))
                .or_else(|| {
                    map.get("content").and_then(|value| {
                        if value.is_string() {
                            value.as_str()
                        } else {
                            None
                        }
                    })
                })
            {
                let text = text.trim();
                if !text.is_empty() {
                    metrics.input_chars += text.chars().count() as u64;
                    metrics.prompt_items += 1;
                    metrics.is_prompt = true;
                    found_direct_text = true;
                }
            }
            // Only recurse into content array if no direct text was found —
            // avoids double-counting when the same content is expressed both
            // as a top-level text field and as a structured content array.
            if !found_direct_text {
                if let Some(content) = map.get("content").filter(|value| value.is_array()) {
                    append_prompt_value(metrics, content);
                }
            }
            if let Some(input) = map.get("input") {
                append_prompt_value(metrics, input);
            }
            if let Some(messages) = map.get("messages") {
                append_prompt_value(metrics, messages);
            }
        }
        _ => {}
    }
}

pub(crate) fn usage_metrics_from_response_value(value: &serde_json::Value) -> UsageMetrics {
    let usage = value
        .get("usage")
        .cloned()
        .or_else(|| {
            value
                .get("response")
                .and_then(|resp| resp.get("usage"))
                .cloned()
        })
        .unwrap_or(serde_json::Value::Null);

    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| usage.get("prompt_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| usage.get("completion_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(input_tokens + output_tokens);
    let cache_tokens = usage
        .get("input_tokens_details")
        .and_then(|v| v.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| usage.get("cache_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("output_tokens_details")
        .and_then(|v| v.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| usage.get("reasoning_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0);

    UsageMetrics {
        input_tokens,
        output_tokens,
        total_tokens,
        cache_tokens,
        reasoning_tokens,
        raw_usage: if usage.is_null() { None } else { Some(usage) },
    }
}

pub(crate) fn apply_estimated_usage_fallback(
    metrics: &mut UsageMetrics,
    prompt: &PromptMetrics,
    output_text: &str,
) {
    let output_chars = output_text.trim().chars().count() as u64;
    let mut estimated = false;

    if metrics.input_tokens == 0 {
        let input_tokens = estimated_tokens_from_chars(prompt.input_chars);
        if input_tokens > 0 {
            metrics.input_tokens = input_tokens;
            estimated = true;
        }
    }

    if metrics.output_tokens == 0 {
        let output_tokens = estimated_tokens_from_chars(output_chars);
        if output_tokens > 0 {
            metrics.output_tokens = output_tokens;
            estimated = true;
        }
    }

    if metrics.total_tokens == 0 {
        metrics.total_tokens = metrics.input_tokens.saturating_add(metrics.output_tokens);
    }

    if !estimated {
        return;
    }

    let estimated_usage = serde_json::json!({
        "provider": "qwen",
        "input_chars": prompt.input_chars,
        "output_chars": output_chars,
        "input_tokens": metrics.input_tokens,
        "output_tokens": metrics.output_tokens,
        "total_tokens": metrics.total_tokens
    });

    metrics.raw_usage = Some(match metrics.raw_usage.take() {
        Some(serde_json::Value::Object(mut usage)) => {
            usage.insert("estimated_usage".to_string(), estimated_usage);
            serde_json::Value::Object(usage)
        }
        Some(usage) => serde_json::json!({
            "upstream_usage": usage,
            "estimated_usage": estimated_usage
        }),
        None => serde_json::json!({
            "estimated_usage": estimated_usage
        }),
    });
}

fn estimated_tokens_from_chars(chars: u64) -> u64 {
    if chars == 0 {
        0
    } else {
        chars.saturating_add(3) / 4
    }
}

fn usage_metrics_from_sse_response_body(body: &Bytes) -> Option<UsageMetrics> {
    let text = String::from_utf8_lossy(body);
    for line in text.lines() {
        let data = match line.strip_prefix("data: ") {
            Some(data) => data.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            break;
        }
        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("response.completed") {
            if let Some(response) = value.get("response") {
                return Some(usage_metrics_from_response_value(response));
            }
        }
    }
    None
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
                [(
                    axum::http::header::CONTENT_TYPE.as_str(),
                    "application/json",
                )],
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
                    [(
                        axum::http::header::CONTENT_TYPE.as_str(),
                        "application/json",
                    )],
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
                    [(
                        axum::http::header::CONTENT_TYPE.as_str(),
                        "application/json",
                    )],
                    openai_error_body(e.message, "invalid_request_error", None),
                )
                    .into_response()
            } else {
                (e.status, e.message).into_response()
            };
        }
    };
    match routed.target {
        TargetModel::CodexModels => {
            return codex_models_response(state, headers).await;
        }
        TargetModel::UnifiedV1Models => {
            return unified_v1_models_response(state, headers, &routed.upstream_path).await;
        }
        TargetModel::Antigravity => {
            return target::antigravity::api::responses(
                State(state),
                headers,
                routed.upstream_body,
            )
            .await
            .into_response();
        }
        TargetModel::Gemini => {
            return target::gemini::api::responses(State(state), headers, routed.upstream_body)
                .await
                .into_response();
        }
        TargetModel::Qwen => {
            return target::qwen::api::responses(State(state), headers, routed.upstream_body)
                .await
                .into_response();
        }
        TargetModel::DeepSeek => {
            return target::deepseek::api::responses(State(state), headers, routed.upstream_body)
                .await
                .into_response();
        }
        TargetModel::Grok => {
            return target::grok::api::responses(State(state), headers, routed.upstream_body)
                .await
                .into_response();
        }
        TargetModel::MiniMax => {
            return target::minimax::responses_native::responses(State(state), headers, routed.upstream_body)
                .await
                .into_response();
        }
        TargetModel::Codex => {}
    }
    let upstream = match routed.target {
        TargetModel::Codex => target::codex::gateway::build_upstream_url(
            &state.cfg.upstream_base,
            &routed.upstream_path,
            routed.upstream_query.as_deref(),
        ),
        TargetModel::Antigravity
        | TargetModel::Gemini
        | TargetModel::Qwen
        | TargetModel::DeepSeek
        | TargetModel::Grok
        | TargetModel::MiniMax
        | TargetModel::CodexModels
        | TargetModel::UnifiedV1Models => unreachable!("non-codex targets return earlier"),
    };
    let session_id = Uuid::new_v4().to_string();

    let picked = pick_token(&state);
    if picked.is_none() {
        return if matches!(source_api, SourceApi::V1) {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(
                    axum::http::header::CONTENT_TYPE.as_str(),
                    "application/json",
                )],
                openai_error_body("No upstream credentials configured", "server_error", None),
            )
                .into_response()
        } else {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "no upstream tokens configured",
            )
                .into_response()
        };
    }
    let (_token_idx, token) = picked.unwrap();
    let codex_context = {
        let request_value: Option<serde_json::Value> =
            serde_json::from_slice(&routed.upstream_body).ok();
        let prompt = request_value
            .as_ref()
            .map(prompt_metrics_from_request_value)
            .unwrap_or_default();
        let model = request_value.as_ref().and_then(model_from_request_value);
        codex_usage_context(&token, model, routed.upstream_path.clone(), prompt)
    };
    record_codex_request(&state, &codex_context);
    let body_bytes = match routed.target {
        TargetModel::Codex => target::codex::gateway::build_request_body(
            &method,
            &routed.upstream_path,
            &headers,
            routed.upstream_body,
            &session_id,
        ),
        TargetModel::Antigravity
        | TargetModel::Gemini
        | TargetModel::Qwen
        | TargetModel::DeepSeek
        | TargetModel::Grok
        | TargetModel::MiniMax
        | TargetModel::CodexModels
        | TargetModel::UnifiedV1Models => unreachable!("non-codex targets return earlier"),
    };
    let mut req = state
        .client
        .request(method.clone(), upstream)
        .body(body_bytes);

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
        TargetModel::Antigravity
        | TargetModel::Gemini
        | TargetModel::Qwen
        | TargetModel::DeepSeek
        | TargetModel::Grok
        | TargetModel::MiniMax
        | TargetModel::CodexModels
        | TargetModel::UnifiedV1Models => unreachable!("non-codex targets return earlier"),
    };

    let resp = match req.send().await {
        Ok(r) => r,
        Err(err) => {
            error!("upstream error: {}", err);
            record_codex_error(&state, &codex_context, "upstream send failed");
            return if matches!(source_api, SourceApi::V1) {
                (
                    StatusCode::BAD_GATEWAY,
                    [(
                        axum::http::header::CONTENT_TYPE.as_str(),
                        "application/json",
                    )],
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
        record_codex_error(
            &state,
            &codex_context,
            format!("upstream status {}", status),
        );
        let body_bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(err) => {
                error!("upstream error body read failed: {}", err);
                return if matches!(source_api, SourceApi::V1) {
                    (
                        StatusCode::BAD_GATEWAY,
                        [(
                            axum::http::header::CONTENT_TYPE.as_str(),
                            "application/json",
                        )],
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
            (
                status,
                headers,
                upstream_error_to_openai(status, &body_bytes),
            )
                .into_response()
        } else {
            (status, out_headers, body_bytes).into_response()
        };
    }

    if matches!(routed.response_mode, ResponseMode::SseToJson) {
        let body_bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(err) => {
                error!("upstream body read failed: {}", err);
                record_codex_error(&state, &codex_context, "failed to read upstream body");
                return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
            }
        };
        let metrics = usage_metrics_from_sse_response_body(&body_bytes).unwrap_or_default();
        record_usage_success(&state, &codex_context, &metrics);
        let json_body = sse_to_response_json(&body_bytes);
        let mut headers = out_headers;
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        return (status, headers, json_body).into_response();
    }

    if matches!(source_api, SourceApi::Codex) && method == Method::GET {
        if is_codex_models_list_path(&raw_path) {
            let body_bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(err) => {
                    error!("upstream body read failed: {}", err);
                    return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
                }
            };
            let body_bytes = augment_codex_models_json(&body_bytes, &state);
            let mut headers = out_headers;
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            return (status, headers, body_bytes).into_response();
        }
    }

    if matches!(source_api, SourceApi::V1) && method == Method::GET {
        if is_v1_models_list_path(&raw_path) || v1_model_retrieve_id(&raw_path).is_some() {
            let body_bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(err) => {
                    error!("upstream body read failed: {}", err);
                    return (
                        StatusCode::BAD_GATEWAY,
                        [(
                            axum::http::header::CONTENT_TYPE.as_str(),
                            "application/json",
                        )],
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
                    [(
                        axum::http::header::CONTENT_TYPE.as_str(),
                        "application/json",
                    )],
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
    let stream_context = codex_context.clone();
    let content_type = out_headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase());
    let mut usage_tracker = CodexSseUsageTracker::new(stats_state.clone(), stream_context.clone());
    let stream = resp.bytes_stream().map(move |chunk| {
        if let Err(ref err) = chunk {
            error!("stream chunk error: {}", err);
            record_codex_error(&stats_state, &stream_context, "stream chunk error");
        }
        if let Ok(ref bytes) = chunk {
            usage_tracker.push(bytes);
        }
        chunk.map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "stream"))
    });
    let stream = if matches!(source_api, SourceApi::V1)
        && content_type
            .as_deref()
            .map(|value| value.contains("text/event-stream"))
            .unwrap_or(false)
    {
        compat_v1_sse_stream(stream).left_stream()
    } else {
        stream.right_stream()
    };
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

async fn codex_models_response(state: AppState, headers: HeaderMap) -> axum::response::Response {
    let body_bytes = fetch_raw_codex_models_body(&state, &headers)
        .await
        .unwrap_or_else(|| Bytes::from_static(br#"{"models":[]}"#));
    let body_bytes = augment_codex_models_json(&body_bytes, &state);
    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        body_bytes,
    )
        .into_response()
}

async fn fetch_raw_codex_models_body(state: &AppState, headers: &HeaderMap) -> Option<Bytes> {
    let (_token_idx, token) = pick_token(state)?;
    let session_id = Uuid::new_v4().to_string();
    let mut req = state.client.request(
        Method::GET,
        target::codex::gateway::build_upstream_url(
            &state.cfg.upstream_base,
            "models",
            Some("client_version=1.0.0"),
        ),
    );
    for (key, value) in headers.iter() {
        if should_drop_incoming_header(key.as_str()) {
            continue;
        }
        req = req.header(key, value);
    }
    req = req.header("Authorization", format!("Bearer {}", token.token));
    req = target::codex::gateway::apply_default_headers(
        req,
        headers,
        token.account_id.as_deref(),
        &session_id,
    );

    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.bytes().await.ok()
}

async fn unified_v1_models_response(
    state: AppState,
    headers: HeaderMap,
    upstream_path: &str,
) -> axum::response::Response {
    let mut models = collect_unified_v1_models(&state, &headers).await;
    if models.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("Content-Type", "application/json")],
            openai_error_body("No upstream credentials configured", "server_error", None),
        )
            .into_response();
    }

    models.sort_by(|left, right| {
        left.get("id")
            .and_then(|value| value.as_str())
            .cmp(&right.get("id").and_then(|value| value.as_str()))
    });

    if upstream_path == "models" {
        let body = serde_json::to_vec(&serde_json::json!({
            "object": "list",
            "data": models,
            "models": models
        }))
        .unwrap_or_default();
        return (StatusCode::OK, [("Content-Type", "application/json")], body).into_response();
    }

    let Some(model_id) = upstream_path.strip_prefix("models/") else {
        return (
            StatusCode::NOT_FOUND,
            [("Content-Type", "application/json")],
            openai_error_body("v1 endpoint not found", "invalid_request_error", None),
        )
            .into_response();
    };

    let model = models
        .into_iter()
        .find(|entry| entry.get("id").and_then(|value| value.as_str()) == Some(model_id));

    match model {
        Some(model) => (
            StatusCode::OK,
            [("Content-Type", "application/json")],
            serde_json::to_vec(&model).unwrap_or_default(),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [("Content-Type", "application/json")],
            openai_error_body(
                &format!("The model '{}' does not exist", model_id),
                "invalid_request_error",
                Some("model_not_found"),
            ),
        )
            .into_response(),
    }
}

async fn collect_unified_v1_models(
    state: &AppState,
    headers: &HeaderMap,
) -> Vec<serde_json::Value> {
    let mut models = Vec::new();
    let mut seen = HashSet::new();

    append_unique_models(
        &mut models,
        &mut seen,
        fetch_codex_v1_models(state, headers).await,
    );
    append_unique_models(
        &mut models,
        &mut seen,
        fetch_openai_models_from_response(
            target::gemini::api::models(State(state.clone()), headers.clone())
                .await
                .into_response(),
        )
        .await,
    );
    append_unique_models(
        &mut models,
        &mut seen,
        fetch_openai_models_from_response(
            target::antigravity::api::models(State(state.clone()), headers.clone())
                .await
                .into_response(),
        )
        .await,
    );
    append_unique_models(
        &mut models,
        &mut seen,
        fetch_openai_models_from_response(
            target::qwen::api::models(State(state.clone()), headers.clone())
                .await
                .into_response(),
        )
        .await,
    );
    append_unique_models(
        &mut models,
        &mut seen,
        fetch_openai_models_from_response(
            target::deepseek::api::models(State(state.clone()), headers.clone())
                .await
                .into_response(),
        )
        .await,
    );
    append_unique_models(
        &mut models,
        &mut seen,
        fetch_openai_models_from_response(if has_enabled_grok_account(state) {
            target::grok::api::models(State(state.clone()), headers.clone())
                .await
                .into_response()
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, "").into_response()
        })
        .await,
    );
    append_unique_models(
        &mut models,
        &mut seen,
        fetch_openai_models_from_response(if has_enabled_minimax_account(state) {
            target::minimax::api::models(State(state.clone()), headers.clone())
                .await
                .into_response()
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, "").into_response()
        })
        .await,
    );

    models
}

fn has_enabled_grok_account(state: &AppState) -> bool {
    state
        .grok_accounts
        .lock()
        .unwrap()
        .iter()
        .any(|account| account.enabled)
}

fn has_enabled_minimax_account(state: &AppState) -> bool {
    state
        .minimax_accounts
        .lock()
        .unwrap()
        .iter()
        .any(|account| account.enabled)
}

fn append_unique_models(
    output: &mut Vec<serde_json::Value>,
    seen: &mut HashSet<String>,
    incoming: Vec<serde_json::Value>,
) {
    for model in incoming {
        let Some(id) = model
            .get("id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
        else {
            continue;
        };
        if seen.insert(id) {
            output.push(model);
        }
    }
}

async fn fetch_codex_v1_models(state: &AppState, headers: &HeaderMap) -> Vec<serde_json::Value> {
    let Some((_token_idx, token)) = pick_token(state) else {
        return Vec::new();
    };

    let mut req = state.client.request(
        Method::GET,
        target::codex::gateway::build_upstream_url(
            &state.cfg.upstream_base,
            "models",
            Some("client_version=1.0.0"),
        ),
    );
    for (key, value) in headers.iter() {
        if should_drop_incoming_header(key.as_str()) {
            continue;
        }
        req = req.header(key, value);
    }
    req = req.header("Authorization", format!("Bearer {}", token.token));
    req = target::codex::gateway::apply_default_headers(
        req,
        headers,
        token.account_id.as_deref(),
        &Uuid::new_v4().to_string(),
    );

    let resp = match req.send().await {
        Ok(resp) if resp.status().is_success() => resp,
        _ => return Vec::new(),
    };
    let body = match resp.bytes().await {
        Ok(body) => body,
        Err(_) => return Vec::new(),
    };
    let converted = match models_list_to_openai_json(&body) {
        Ok(body) => body,
        Err(_) => return Vec::new(),
    };

    model_entries_from_openai_list_bytes(&converted)
}

async fn fetch_openai_models_from_response(
    response: axum::response::Response,
) -> Vec<serde_json::Value> {
    if !response.status().is_success() {
        return Vec::new();
    }
    let body = match axum::body::to_bytes(response.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(_) => return Vec::new(),
    };
    model_entries_from_openai_list_bytes(&body)
}

fn model_entries_from_openai_list_bytes(body: &[u8]) -> Vec<serde_json::Value> {
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    value
        .get("data")
        .and_then(|value| value.as_array())
        .or_else(|| value.get("models").and_then(|value| value.as_array()))
        .cloned()
        .unwrap_or_default()
}

struct CompatSseState<S> {
    upstream: S,
    buffer: BytesMut,
    output_items: Vec<serde_json::Value>,
    queued: VecDeque<Bytes>,
}

struct CodexSseUsageTracker {
    state: AppState,
    context: UsageContext,
    buffer: BytesMut,
    recorded: bool,
}

impl CodexSseUsageTracker {
    fn new(state: AppState, context: UsageContext) -> Self {
        Self {
            state,
            context,
            buffer: BytesMut::new(),
            recorded: false,
        }
    }

    fn push(&mut self, chunk: &Bytes) {
        if self.recorded || !self.context.prompt.is_prompt {
            return;
        }

        self.buffer.extend_from_slice(chunk);
        while let Some((event_end, delimiter_len)) = find_sse_event_boundary(&self.buffer) {
            let raw_event = self.buffer.split_to(event_end + delimiter_len);
            let event = raw_event[..event_end].to_vec();
            self.handle_event(&event);
            if self.recorded {
                break;
            }
        }
    }

    fn handle_event(&mut self, raw_event: &[u8]) {
        let text = String::from_utf8_lossy(raw_event);
        let mut data_lines = Vec::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(value) = line.strip_prefix("data:") {
                data_lines.push(value.trim_start().to_string());
            }
        }
        if data_lines.is_empty() {
            return;
        }

        let data_text = data_lines.join("\n");
        if data_text == "[DONE]" {
            return;
        }
        let value: serde_json::Value = match serde_json::from_str(&data_text) {
            Ok(value) => value,
            Err(_) => return,
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("response.completed") {
            return;
        }
        let Some(response) = value.get("response") else {
            return;
        };
        let metrics = usage_metrics_from_response_value(response);
        record_usage_success(&self.state, &self.context, &metrics);
        self.recorded = true;
    }
}

fn compat_v1_sse_stream<S>(
    stream: S,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>>
where
    S: futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    stream::try_unfold(
        CompatSseState {
            upstream: stream,
            buffer: BytesMut::new(),
            output_items: Vec::new(),
            queued: VecDeque::new(),
        },
        |mut state| async move {
            loop {
                if let Some(chunk) = state.queued.pop_front() {
                    return Ok(Some((chunk, state)));
                }

                match state.upstream.next().await {
                    Some(Ok(chunk)) => queue_compat_sse_bytes(&mut state, &chunk),
                    Some(Err(err)) => return Err(err),
                    None => {
                        flush_compat_sse_buffer(&mut state);
                        if let Some(chunk) = state.queued.pop_front() {
                            return Ok(Some((chunk, state)));
                        }
                        return Ok(None);
                    }
                }
            }
        },
    )
}

fn queue_compat_sse_bytes<S>(state: &mut CompatSseState<S>, chunk: &Bytes) {
    state.buffer.extend_from_slice(chunk);

    while let Some((event_end, delimiter_len)) = find_sse_event_boundary(&state.buffer) {
        let raw_event = state.buffer.split_to(event_end + delimiter_len);
        let event = raw_event[..event_end].to_vec();
        push_compat_sse_event(state, &event, delimiter_len == 4);
    }
}

fn flush_compat_sse_buffer<S>(state: &mut CompatSseState<S>) {
    if state.buffer.is_empty() {
        return;
    }
    let remaining = state.buffer.split().to_vec();
    push_compat_sse_event(state, &remaining, false);
}

fn push_compat_sse_event<S>(state: &mut CompatSseState<S>, raw_event: &[u8], use_crlf: bool) {
    let event = rewrite_v1_sse_event(raw_event, &mut state.output_items, use_crlf);
    state.queued.push_back(Bytes::from(event));
}

fn find_sse_event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(4)
        .position(|window| {
            window
                == b"

"
        })
        .map(|idx| (idx, 4))
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| {
                    window
                        == b"

"
                })
                .map(|idx| (idx, 2))
        })
}

fn rewrite_v1_sse_event(
    raw_event: &[u8],
    output_items: &mut Vec<serde_json::Value>,
    use_crlf: bool,
) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw_event);
    let mut event_name: Option<String> = None;
    let mut data_lines = Vec::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }

    if data_lines.is_empty() {
        return append_sse_delimiter(raw_event.to_vec(), use_crlf);
    }

    let data_text = data_lines.join(
        "
",
    );
    if data_text == "[DONE]" {
        return render_sse_event(event_name.as_deref(), "[DONE]", use_crlf);
    }

    let mut value: serde_json::Value = match serde_json::from_str(&data_text) {
        Ok(value) => value,
        Err(_) => return append_sse_delimiter(raw_event.to_vec(), use_crlf),
    };

    if let Some(kind) = value.get("type").and_then(|kind| kind.as_str()) {
        match kind {
            "response.output_item.done" => {
                if let Some(item) = value.get("item").cloned() {
                    let output_index = value
                        .get("output_index")
                        .and_then(|index| index.as_u64())
                        .unwrap_or(output_items.len() as u64)
                        as usize;
                    if output_items.len() <= output_index {
                        output_items.resize(output_index + 1, serde_json::Value::Null);
                    }
                    output_items[output_index] = item;
                }
            }
            "response.completed" => {
                if let Some(response) = value
                    .get_mut("response")
                    .and_then(|response| response.as_object_mut())
                {
                    let should_fill_output = response
                        .get("output")
                        .and_then(|output| output.as_array())
                        .map(|output| output.is_empty())
                        .unwrap_or(true);
                    if should_fill_output && output_items.iter().any(|item| !item.is_null()) {
                        response.insert(
                            "output".to_string(),
                            serde_json::Value::Array(
                                output_items
                                    .iter()
                                    .filter(|item| !item.is_null())
                                    .cloned()
                                    .collect(),
                            ),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let event_name = event_name.or_else(|| {
        value
            .get("type")
            .and_then(|kind| kind.as_str())
            .map(|kind| kind.to_string())
    });
    let data = serde_json::to_string(&value).unwrap_or(data_text);
    render_sse_event(event_name.as_deref(), &data, use_crlf)
}

fn append_sse_delimiter(mut raw_event: Vec<u8>, use_crlf: bool) -> Vec<u8> {
    if use_crlf {
        raw_event.extend_from_slice(
            b"

",
        );
    } else {
        raw_event.extend_from_slice(
            b"

",
        );
    }
    raw_event
}

fn render_sse_event(event_name: Option<&str>, data: &str, use_crlf: bool) -> Vec<u8> {
    let delimiter = if use_crlf {
        "
"
    } else {
        "
"
    };
    let mut out = String::new();
    if let Some(event_name) = event_name {
        out.push_str("event: ");
        out.push_str(event_name);
        out.push_str(delimiter);
    }
    out.push_str("data: ");
    out.push_str(data);
    out.push_str(delimiter);
    out.push_str(delimiter);
    out.into_bytes()
}

fn build_usage_stats(
    tokens: &[UpstreamToken],
    agw_accounts: &[target::antigravity::accounts::AntigravityAccount],
    gemini_accounts: &[target::gemini::accounts::GeminiAccount],
    qwen_accounts: &[target::qwen::accounts::QwenAccount],
    deepseek_accounts: &[target::deepseek::accounts::DeepSeekAccount],
    grok_accounts: &[target::grok::accounts::GrokAccount],
    minimax_accounts: &[target::minimax::accounts::MiniMaxAccount],
    persisted_stats: &StatsStore,
) -> UsageStats {
    let codex_accounts = tokens
        .iter()
        .map(|token| {
            let key = codex_stats_key(token);
            let stored = persisted_stats
                .account_usage(Provider::Codex, &key)
                .cloned()
                .unwrap_or_default();
            AccountUsage {
                key,
                label: token.label.clone(),
                account_id: token.account_id.clone().unwrap_or_default(),
                requests: stored.requests,
                errors: stored.errors,
                prompt_total: stored.prompt_total,
                prompt_error_total: stored.prompt_error_total,
                input_tokens: stored.input_tokens,
                output_tokens: stored.output_tokens,
                total_tokens: stored.total_tokens,
                cache_tokens: stored.cache_tokens,
                reasoning_tokens: stored.reasoning_tokens,
                first_seen_at: stored.first_seen_at,
                last_seen_at: stored.last_seen_at,
                last_success_at: stored.last_success_at,
                last_error_at: stored.last_error_at,
            }
        })
        .collect();

    let agw_accounts = agw_accounts
        .iter()
        .map(|account| {
            let key = antigravity_stats_key(account);
            let stored = persisted_stats
                .account_usage(Provider::Antigravity, &key)
                .cloned()
                .unwrap_or_default();
            AccountUsage {
                key,
                label: account.label.clone(),
                account_id: account.email.clone(),
                requests: stored.requests,
                errors: stored.errors,
                prompt_total: stored.prompt_total,
                prompt_error_total: stored.prompt_error_total,
                input_tokens: stored.input_tokens,
                output_tokens: stored.output_tokens,
                total_tokens: stored.total_tokens,
                cache_tokens: stored.cache_tokens,
                reasoning_tokens: stored.reasoning_tokens,
                first_seen_at: stored.first_seen_at,
                last_seen_at: stored.last_seen_at,
                last_success_at: stored.last_success_at,
                last_error_at: stored.last_error_at,
            }
        })
        .collect();

    let qwen_accounts = qwen_accounts
        .iter()
        .map(|account| {
            let key = qwen_stats_key(account);
            let stored = persisted_stats
                .account_usage(Provider::Qwen, &key)
                .cloned()
                .unwrap_or_default();
            AccountUsage {
                key,
                label: account.label.clone(),
                account_id: account.email.clone(),
                requests: stored.requests,
                errors: stored.errors,
                prompt_total: stored.prompt_total,
                prompt_error_total: stored.prompt_error_total,
                input_tokens: stored.input_tokens,
                output_tokens: stored.output_tokens,
                total_tokens: stored.total_tokens,
                cache_tokens: stored.cache_tokens,
                reasoning_tokens: stored.reasoning_tokens,
                first_seen_at: stored.first_seen_at,
                last_seen_at: stored.last_seen_at,
                last_success_at: stored.last_success_at,
                last_error_at: stored.last_error_at,
            }
        })
        .collect();

    let gemini_accounts = gemini_accounts
        .iter()
        .map(|account| {
            let key = gemini_stats_key(account);
            let stored = persisted_stats
                .account_usage(Provider::Gemini, &key)
                .cloned()
                .unwrap_or_default();
            AccountUsage {
                key,
                label: account.label.clone(),
                account_id: account.email.clone(),
                requests: stored.requests,
                errors: stored.errors,
                prompt_total: stored.prompt_total,
                prompt_error_total: stored.prompt_error_total,
                input_tokens: stored.input_tokens,
                output_tokens: stored.output_tokens,
                total_tokens: stored.total_tokens,
                cache_tokens: stored.cache_tokens,
                reasoning_tokens: stored.reasoning_tokens,
                first_seen_at: stored.first_seen_at,
                last_seen_at: stored.last_seen_at,
                last_success_at: stored.last_success_at,
                last_error_at: stored.last_error_at,
            }
        })
        .collect();

    let deepseek_accounts = deepseek_accounts
        .iter()
        .map(|account| {
            let key = deepseek_stats_key(account);
            let stored = persisted_stats
                .account_usage(Provider::DeepSeek, &key)
                .cloned()
                .unwrap_or_default();
            AccountUsage {
                key,
                label: account.label.clone(),
                account_id: account.account_id.clone(),
                requests: stored.requests,
                errors: stored.errors,
                prompt_total: stored.prompt_total,
                prompt_error_total: stored.prompt_error_total,
                input_tokens: stored.input_tokens,
                output_tokens: stored.output_tokens,
                total_tokens: stored.total_tokens,
                cache_tokens: stored.cache_tokens,
                reasoning_tokens: stored.reasoning_tokens,
                first_seen_at: stored.first_seen_at,
                last_seen_at: stored.last_seen_at,
                last_success_at: stored.last_success_at,
                last_error_at: stored.last_error_at,
            }
        })
        .collect();

    let grok_accounts = grok_accounts
        .iter()
        .map(|account| {
            let key = grok_stats_key(account);
            let stored = persisted_stats
                .account_usage(Provider::Grok, &key)
                .cloned()
                .unwrap_or_default();
            AccountUsage {
                key,
                label: account.label.clone(),
                account_id: account
                    .user_id
                    .clone()
                    .or_else(|| account.email.clone())
                    .unwrap_or_default(),
                requests: stored.requests,
                errors: stored.errors,
                prompt_total: stored.prompt_total,
                prompt_error_total: stored.prompt_error_total,
                input_tokens: stored.input_tokens,
                output_tokens: stored.output_tokens,
                total_tokens: stored.total_tokens,
                cache_tokens: stored.cache_tokens,
                reasoning_tokens: stored.reasoning_tokens,
                first_seen_at: stored.first_seen_at,
                last_seen_at: stored.last_seen_at,
                last_success_at: stored.last_success_at,
                last_error_at: stored.last_error_at,
            }
        })
        .collect();

    let minimax_accounts = minimax_accounts
        .iter()
        .map(|account| {
            let key = minimax_stats_key(account);
            let stored = persisted_stats
                .account_usage(Provider::MiniMax, &key)
                .cloned()
                .unwrap_or_default();
            AccountUsage {
                key,
                label: account.label.clone(),
                account_id: account.account_id.clone(),
                requests: stored.requests,
                errors: stored.errors,
                prompt_total: stored.prompt_total,
                prompt_error_total: stored.prompt_error_total,
                input_tokens: stored.input_tokens,
                output_tokens: stored.output_tokens,
                total_tokens: stored.total_tokens,
                cache_tokens: stored.cache_tokens,
                reasoning_tokens: stored.reasoning_tokens,
                first_seen_at: stored.first_seen_at,
                last_seen_at: stored.last_seen_at,
                last_success_at: stored.last_success_at,
                last_error_at: stored.last_error_at,
            }
        })
        .collect();

    UsageStats {
        codex_accounts,
        agw_accounts,
        gemini_accounts,
        qwen_accounts,
        deepseek_accounts,
        grok_accounts,
        minimax_accounts,
        total_requests: persisted_stats.total_requests,
        total_errors: persisted_stats.total_errors,
        total_prompt_total: persisted_stats.total_prompt_total,
        total_prompt_error_total: persisted_stats.total_prompt_error_total,
        total_input_tokens: persisted_stats.total_input_tokens,
        total_output_tokens: persisted_stats.total_output_tokens,
        total_tokens_used: persisted_stats.total_tokens_used,
        total_cache_tokens: persisted_stats.total_cache_tokens,
        total_reasoning_tokens: persisted_stats.total_reasoning_tokens,
        first_recorded_at: persisted_stats.first_recorded_at.clone(),
        last_recorded_at: persisted_stats.last_recorded_at.clone(),
    }
}

pub(crate) fn sync_usage_stats(state: &AppState) {
    let tokens = state.tokens.lock().unwrap().clone();
    let agw_accounts = state.agw_accounts.lock().unwrap().clone();
    let gemini_accounts = state.gemini_accounts.lock().unwrap().clone();
    let qwen_accounts = state.qwen_accounts.lock().unwrap().clone();
    let deepseek_accounts = state.deepseek_accounts.lock().unwrap().clone();
    let grok_accounts = state.grok_accounts.lock().unwrap().clone();
    let minimax_accounts = state.minimax_accounts.lock().unwrap().clone();
    let persisted_stats = state.persisted_stats.lock().unwrap().clone();
    let mut stats = state.stats.lock().unwrap();
    *stats = build_usage_stats(
        &tokens,
        &agw_accounts,
        &gemini_accounts,
        &qwen_accounts,
        &deepseek_accounts,
        &grok_accounts,
        &minimax_accounts,
        &persisted_stats,
    );
}

pub(crate) fn codex_stats_key(token: &UpstreamToken) -> String {
    if let Some(account_id) = token.account_id.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("codex:account_id:{}", account_id);
    }
    if let Some(file_name) = token.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("codex:file:{}", file_name);
    }
    format!("codex:label:{}", token.label)
}

pub(crate) fn antigravity_stats_key(
    account: &target::antigravity::accounts::AntigravityAccount,
) -> String {
    if !account.email.trim().is_empty() {
        return format!("agw:email:{}", account.email);
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("agw:file:{}", file_name);
    }
    if let Some(project_id) = account.project_id.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("agw:project:{}", project_id);
    }
    format!("agw:label:{}", account.label)
}

pub(crate) fn qwen_stats_key(account: &target::qwen::accounts::QwenAccount) -> String {
    if let Some(subject) = account.subject.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("qwen:subject:{}", subject);
    }
    if !account.account_id.trim().is_empty() {
        return format!("qwen:subject:{}", account.account_id);
    }
    if !account.email.trim().is_empty() {
        return format!("qwen:email:{}", account.email);
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("qwen:file:{}", file_name);
    }
    if let Some(resource_url) = account
        .resource_url
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        return format!("qwen:resource:{}", resource_url);
    }
    format!("qwen:label:{}", account.label)
}

pub(crate) fn gemini_stats_key(account: &target::gemini::accounts::GeminiAccount) -> String {
    if !account.email.trim().is_empty() {
        if let Some(project_id) = account.project_id.as_ref().filter(|s| !s.trim().is_empty()) {
            return format!("gemini:email:{}|project:{}", account.email, project_id);
        }
        return format!("gemini:email:{}", account.email);
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("gemini:file:{}", file_name);
    }
    format!("gemini:label:{}", account.label)
}

pub(crate) fn deepseek_stats_key(account: &target::deepseek::accounts::DeepSeekAccount) -> String {
    if !account.account_id.trim().is_empty() {
        return format!("deepseek:account_id:{}", account.account_id);
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("deepseek:file:{}", file_name);
    }
    format!("deepseek:label:{}", account.label)
}

pub(crate) fn grok_stats_key(account: &target::grok::accounts::GrokAccount) -> String {
    if let Some(user_id) = account.user_id.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("grok:user_id:{}", user_id);
    }
    if let Some(email) = account.email.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("grok:email:{}", email);
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        return format!("grok:file:{}", file_name);
    }
    format!("grok:label:{}", account.label)
}

fn qwen_fallback_stats_keys(account: &target::qwen::accounts::QwenAccount) -> Vec<String> {
    let mut keys = Vec::new();
    if !account.email.trim().is_empty() {
        keys.push(format!("qwen:email:{}", account.email));
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        keys.push(format!("qwen:file:{}", file_name));
    }
    if let Some(resource_url) = account
        .resource_url
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        keys.push(format!("qwen:resource:{}", resource_url));
    }
    if !account.label.trim().is_empty() {
        keys.push(format!("qwen:label:{}", account.label));
    }
    keys
}

fn grok_fallback_stats_keys(account: &target::grok::accounts::GrokAccount) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(email) = account.email.as_ref().filter(|s| !s.trim().is_empty()) {
        keys.push(format!("grok:email:{}", email));
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.trim().is_empty()) {
        keys.push(format!("grok:file:{}", file_name));
    }
    if !account.label.trim().is_empty() {
        keys.push(format!("grok:label:{}", account.label));
    }
    keys
}

fn migrate_qwen_usage_keys(state: &AppState) {
    let accounts = state.qwen_accounts.lock().unwrap().clone();
    let mut changed = false;
    {
        let mut persisted = state.persisted_stats.lock().unwrap();
        for account in &accounts {
            let stable_key = qwen_stats_key(account);
            if persisted.qwen.contains_key(&stable_key) {
                continue;
            }

            for fallback_key in qwen_fallback_stats_keys(account) {
                if fallback_key == stable_key {
                    continue;
                }
                let Some(old_usage) = persisted.qwen.remove(&fallback_key) else {
                    continue;
                };
                let entry = persisted.qwen.entry(stable_key.clone()).or_default();
                merge_usage(entry, old_usage);
                changed = true;
                break;
            }
        }
    }
    if changed {
        persist_stats_store(state);
    }
}

pub(crate) fn migrate_grok_usage_keys(state: &AppState) {
    let accounts = state.grok_accounts.lock().unwrap().clone();
    let mut changed = false;
    {
        let mut persisted = state.persisted_stats.lock().unwrap();
        for account in &accounts {
            let stable_key = grok_stats_key(account);
            if persisted.grok.contains_key(&stable_key) {
                continue;
            }

            for fallback_key in grok_fallback_stats_keys(account) {
                if fallback_key == stable_key {
                    continue;
                }
                let Some(old_usage) = persisted.grok.remove(&fallback_key) else {
                    continue;
                };
                let entry = persisted.grok.entry(stable_key.clone()).or_default();
                merge_usage(entry, old_usage);
                changed = true;
                break;
            }
        }
    }
    if changed {
        persist_stats_store(state);
    }
}

fn merge_usage(
    target: &mut stats_store::StoredAccountUsage,
    source: stats_store::StoredAccountUsage,
) {
    target.label = source.label;
    target.account_id = source.account_id;
    target.requests += source.requests;
    target.errors += source.errors;
    target.prompt_total += source.prompt_total;
    target.prompt_error_total += source.prompt_error_total;
    target.input_tokens += source.input_tokens;
    target.output_tokens += source.output_tokens;
    target.total_tokens += source.total_tokens;
    target.cache_tokens += source.cache_tokens;
    target.reasoning_tokens += source.reasoning_tokens;
    target.first_seen_at = earliest_timestamp(target.first_seen_at.take(), source.first_seen_at);
    target.last_seen_at = latest_timestamp(target.last_seen_at.take(), source.last_seen_at);
    target.last_success_at =
        latest_timestamp(target.last_success_at.take(), source.last_success_at);
    target.last_error_at = latest_timestamp(target.last_error_at.take(), source.last_error_at);
}

pub(crate) fn minimax_stats_key(account: &target::minimax::accounts::MiniMaxAccount) -> String {
    if !account.account_id.trim().is_empty() {
        return format!("minimax:account_id:{}", account.account_id);
    }
    if let Some(file_name) = account.file_name.as_ref().filter(|s| !s.is_empty()) {
        return format!("minimax:file:{}", file_name);
    }
    format!("minimax:label:{}", account.label)
}

pub(crate) fn minimax_usage_context(
    account: &target::minimax::accounts::MiniMaxAccount,
    model: Option<String>,
    request_path: impl Into<String>,
    prompt: PromptMetrics,
) -> UsageContext {
    UsageContext {
        provider: Provider::MiniMax,
        provider_name: "minimax",
        key: minimax_stats_key(account),
        label: account.label.clone(),
        account_id: account.account_id.clone(),
        credential_file: account.file_name.clone(),
        model,
        request_path: request_path.into(),
        prompt,
    }
}

pub(crate) fn codex_usage_context(
    token: &UpstreamToken,
    model: Option<String>,
    request_path: impl Into<String>,
    prompt: PromptMetrics,
) -> UsageContext {
    UsageContext {
        provider: Provider::Codex,
        provider_name: "codex",
        key: codex_stats_key(token),
        label: token.label.clone(),
        account_id: token.account_id.clone().unwrap_or_default(),
        credential_file: token.file_name.clone(),
        model,
        request_path: request_path.into(),
        prompt,
    }
}

pub(crate) fn antigravity_usage_context(
    account: &target::antigravity::accounts::AntigravityAccount,
    model: Option<String>,
    request_path: impl Into<String>,
    prompt: PromptMetrics,
) -> UsageContext {
    UsageContext {
        provider: Provider::Antigravity,
        provider_name: "antigravity",
        key: antigravity_stats_key(account),
        label: account.label.clone(),
        account_id: account.email.clone(),
        credential_file: account.file_name.clone(),
        model,
        request_path: request_path.into(),
        prompt,
    }
}

pub(crate) fn qwen_usage_context(
    account: &target::qwen::accounts::QwenAccount,
    model: Option<String>,
    request_path: impl Into<String>,
    prompt: PromptMetrics,
) -> UsageContext {
    UsageContext {
        provider: Provider::Qwen,
        provider_name: "qwen",
        key: qwen_stats_key(account),
        label: account.label.clone(),
        account_id: if !account.account_id.trim().is_empty() {
            account.account_id.clone()
        } else {
            account
                .subject
                .clone()
                .unwrap_or_else(|| account.email.clone())
        },
        credential_file: account.file_name.clone(),
        model,
        request_path: request_path.into(),
        prompt,
    }
}

pub(crate) fn gemini_usage_context(
    account: &target::gemini::accounts::GeminiAccount,
    model: Option<String>,
    request_path: impl Into<String>,
    prompt: PromptMetrics,
) -> UsageContext {
    UsageContext {
        provider: Provider::Gemini,
        provider_name: "gemini",
        key: gemini_stats_key(account),
        label: account.label.clone(),
        account_id: account.email.clone(),
        credential_file: account.file_name.clone(),
        model,
        request_path: request_path.into(),
        prompt,
    }
}

pub(crate) fn deepseek_usage_context(
    account: &target::deepseek::accounts::DeepSeekAccount,
    model: Option<String>,
    request_path: impl Into<String>,
    prompt: PromptMetrics,
) -> UsageContext {
    UsageContext {
        provider: Provider::DeepSeek,
        provider_name: "deepseek",
        key: deepseek_stats_key(account),
        label: account.label.clone(),
        account_id: account.account_id.clone(),
        credential_file: account.file_name.clone(),
        model,
        request_path: request_path.into(),
        prompt,
    }
}

pub(crate) fn grok_usage_context(
    account: &target::grok::accounts::GrokAccount,
    model: Option<String>,
    request_path: impl Into<String>,
    prompt: PromptMetrics,
) -> UsageContext {
    UsageContext {
        provider: Provider::Grok,
        provider_name: "grok",
        key: grok_stats_key(account),
        label: account.label.clone(),
        account_id: account
            .user_id
            .clone()
            .or_else(|| account.email.clone())
            .unwrap_or_default(),
        credential_file: account.file_name.clone(),
        model,
        request_path: request_path.into(),
        prompt,
    }
}

fn append_usage_history(state: &AppState, entry: usage_store::UsageHistoryEntry) {
    let _guard = state.usage_history_lock.lock().unwrap();
    if let Err(err) = usage_store::append(&state.cfg, &entry) {
        error!("failed to append usage history: {}", err);
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn earliest_timestamp(current: Option<String>, incoming: Option<String>) -> Option<String> {
    match (current, incoming) {
        (Some(current), Some(incoming)) => Some(std::cmp::min(current, incoming)),
        (Some(current), None) => Some(current),
        (None, Some(incoming)) => Some(incoming),
        (None, None) => None,
    }
}

fn latest_timestamp(current: Option<String>, incoming: Option<String>) -> Option<String> {
    match (current, incoming) {
        (Some(current), Some(incoming)) => Some(std::cmp::max(current, incoming)),
        (Some(current), None) => Some(current),
        (None, Some(incoming)) => Some(incoming),
        (None, None) => None,
    }
}

fn persist_stats_store(state: &AppState) {
    let snapshot = state.persisted_stats.lock().unwrap().clone();
    if let Err(err) = stats_store::save(&state.cfg, &snapshot) {
        error!("failed to persist usage stats: {}", err);
    }
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

fn require_admin_session_json(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if !admin_auth::is_enabled(&state.cfg.admin_auth) {
        return None;
    }
    let mut sessions = state.admin_sessions.lock().unwrap();
    if admin_auth::validate_session(headers, &mut sessions) {
        return None;
    }
    Some(
        (
            StatusCode::UNAUTHORIZED,
            [("Content-Type", "application/json")],
            serde_json::to_vec(&serde_json::json!({
                "ok": false,
                "message": "admin login required"
            }))
            .unwrap_or_default(),
        )
            .into_response(),
    )
}

fn require_admin_session_text(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if !admin_auth::is_enabled(&state.cfg.admin_auth) {
        return None;
    }
    let mut sessions = state.admin_sessions.lock().unwrap();
    if admin_auth::validate_session(headers, &mut sessions) {
        return None;
    }
    Some(
        (
            StatusCode::UNAUTHORIZED,
            [("Content-Type", "text/plain; charset=utf-8")],
            "admin login required".to_string(),
        )
            .into_response(),
    )
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

fn update_account_counters(
    state: &AppState,
    provider: Provider,
    key: String,
    label: String,
    account_id: String,
    delta: CounterDelta,
) {
    {
        let mut persisted = state.persisted_stats.lock().unwrap();
        if delta.request_delta > 0 {
            persisted.total_requests += delta.request_delta;
        }
        if delta.error_delta > 0 {
            persisted.total_errors += delta.error_delta;
        }
        persisted.total_prompt_total += delta.prompt_total_delta;
        persisted.total_prompt_error_total += delta.prompt_error_total_delta;
        persisted.total_input_tokens += delta.input_tokens_delta;
        persisted.total_output_tokens += delta.output_tokens_delta;
        persisted.total_tokens_used += delta.total_tokens_delta;
        persisted.total_cache_tokens += delta.cache_tokens_delta;
        persisted.total_reasoning_tokens += delta.reasoning_tokens_delta;
        if let Some(observed_at) = delta.observed_at.clone() {
            if persisted.first_recorded_at.is_none() {
                persisted.first_recorded_at = Some(observed_at.clone());
            }
            persisted.last_recorded_at = Some(observed_at);
        }
        let entry = persisted.account_usage_mut(provider, key);
        entry.label = label;
        entry.account_id = account_id;
        entry.requests += delta.request_delta;
        entry.errors += delta.error_delta;
        entry.prompt_total += delta.prompt_total_delta;
        entry.prompt_error_total += delta.prompt_error_total_delta;
        entry.input_tokens += delta.input_tokens_delta;
        entry.output_tokens += delta.output_tokens_delta;
        entry.total_tokens += delta.total_tokens_delta;
        entry.cache_tokens += delta.cache_tokens_delta;
        entry.reasoning_tokens += delta.reasoning_tokens_delta;
        if let Some(observed_at) = delta.observed_at {
            if entry.first_seen_at.is_none() {
                entry.first_seen_at = Some(observed_at.clone());
            }
            entry.last_seen_at = Some(observed_at);
        }
        if let Some(success_at) = delta.success_at {
            entry.last_success_at = Some(success_at);
        }
        if let Some(error_at) = delta.error_at {
            entry.last_error_at = Some(error_at);
        }
    }
    persist_stats_store(state);
    sync_usage_stats(state);
}

fn record_request_started(state: &AppState, context: &UsageContext) {
    update_account_counters(
        state,
        context.provider,
        context.key.clone(),
        context.label.clone(),
        context.account_id.clone(),
        CounterDelta {
            request_delta: 1,
            prompt_total_delta: if context.prompt.is_prompt { 1 } else { 0 },
            observed_at: Some(now_rfc3339()),
            ..Default::default()
        },
    );
}

fn record_request_error(state: &AppState, context: &UsageContext, message: impl Into<String>) {
    let observed_at = now_rfc3339();
    update_account_counters(
        state,
        context.provider,
        context.key.clone(),
        context.label.clone(),
        context.account_id.clone(),
        CounterDelta {
            error_delta: 1,
            prompt_error_total_delta: if context.prompt.is_prompt { 1 } else { 0 },
            observed_at: Some(observed_at.clone()),
            error_at: Some(observed_at.clone()),
            ..Default::default()
        },
    );
    if context.prompt.is_prompt {
        append_usage_history(
            state,
            usage_store::UsageHistoryEntry {
                recorded_at: observed_at,
                provider: context.provider_name.to_string(),
                account_key: context.key.clone(),
                account_label: context.label.clone(),
                account_id: context.account_id.clone(),
                credential_file: context.credential_file.clone(),
                model: context.model.clone(),
                request_path: context.request_path.clone(),
                success: false,
                error: true,
                request_total: 1,
                prompt_total: 1,
                prompt_error_total: 1,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cache_tokens: 0,
                reasoning_tokens: 0,
                input_chars: context.prompt.input_chars,
                prompt_items: context.prompt.prompt_items,
                error_message: Some(message.into()),
                raw_usage: None,
            },
        );
    }
}

fn record_usage_success(state: &AppState, context: &UsageContext, metrics: &UsageMetrics) {
    let observed_at = now_rfc3339();
    update_account_counters(
        state,
        context.provider,
        context.key.clone(),
        context.label.clone(),
        context.account_id.clone(),
        CounterDelta {
            input_tokens_delta: metrics.input_tokens,
            output_tokens_delta: metrics.output_tokens,
            total_tokens_delta: metrics.total_tokens,
            cache_tokens_delta: metrics.cache_tokens,
            reasoning_tokens_delta: metrics.reasoning_tokens,
            observed_at: Some(observed_at.clone()),
            success_at: Some(observed_at.clone()),
            ..Default::default()
        },
    );
    if context.prompt.is_prompt {
        append_usage_history(
            state,
            usage_store::UsageHistoryEntry {
                recorded_at: observed_at,
                provider: context.provider_name.to_string(),
                account_key: context.key.clone(),
                account_label: context.label.clone(),
                account_id: context.account_id.clone(),
                credential_file: context.credential_file.clone(),
                model: context.model.clone(),
                request_path: context.request_path.clone(),
                success: true,
                error: false,
                request_total: 1,
                prompt_total: 1,
                prompt_error_total: 0,
                input_tokens: metrics.input_tokens,
                output_tokens: metrics.output_tokens,
                total_tokens: metrics.total_tokens,
                cache_tokens: metrics.cache_tokens,
                reasoning_tokens: metrics.reasoning_tokens,
                input_chars: context.prompt.input_chars,
                prompt_items: context.prompt.prompt_items,
                error_message: None,
                raw_usage: metrics.raw_usage.clone(),
            },
        );
    }
}

fn record_codex_request(state: &AppState, context: &UsageContext) {
    record_request_started(state, context);
}

fn record_codex_error(state: &AppState, context: &UsageContext, message: impl Into<String>) {
    record_request_error(state, context, message);
}

pub(crate) fn record_antigravity_request(state: &AppState, context: &UsageContext) {
    record_request_started(state, context);
}

pub(crate) fn record_antigravity_error(
    state: &AppState,
    context: &UsageContext,
    message: impl Into<String>,
) {
    record_request_error(state, context, message);
}

pub(crate) fn record_antigravity_success(
    state: &AppState,
    context: &UsageContext,
    metrics: &UsageMetrics,
) {
    record_usage_success(state, context, metrics);
}

pub(crate) fn record_qwen_request(state: &AppState, context: &UsageContext) {
    record_request_started(state, context);
}

pub(crate) fn record_qwen_error(
    state: &AppState,
    context: &UsageContext,
    message: impl Into<String>,
) {
    record_request_error(state, context, message);
}

pub(crate) fn record_qwen_success(
    state: &AppState,
    context: &UsageContext,
    metrics: &UsageMetrics,
) {
    record_usage_success(state, context, metrics);
}

pub(crate) fn record_gemini_request(state: &AppState, context: &UsageContext) {
    record_request_started(state, context);
}

pub(crate) fn record_gemini_error(
    state: &AppState,
    context: &UsageContext,
    message: impl Into<String>,
) {
    record_request_error(state, context, message);
}

pub(crate) fn record_gemini_success(
    state: &AppState,
    context: &UsageContext,
    metrics: &UsageMetrics,
) {
    record_usage_success(state, context, metrics);
}

pub(crate) fn record_deepseek_request(state: &AppState, context: &UsageContext) {
    record_request_started(state, context);
}

pub(crate) fn record_deepseek_error(
    state: &AppState,
    context: &UsageContext,
    message: impl Into<String>,
) {
    record_request_error(state, context, message);
}

pub(crate) fn record_deepseek_success(
    state: &AppState,
    context: &UsageContext,
    metrics: &UsageMetrics,
) {
    record_usage_success(state, context, metrics);
}

pub(crate) fn record_grok_error(
    state: &AppState,
    context: &UsageContext,
    message: impl Into<String>,
) {
    record_request_error(state, context, message);
}

pub(crate) fn record_grok_success(
    state: &AppState,
    context: &UsageContext,
    metrics: &UsageMetrics,
) {
    record_usage_success(state, context, metrics);
}

pub(crate) fn record_minimax_request(state: &AppState, context: &UsageContext) {
    record_request_started(state, context);
}

pub(crate) fn record_minimax_error(
    state: &AppState,
    context: &UsageContext,
    message: impl Into<String>,
) {
    record_request_error(state, context, message);
}

pub(crate) fn record_minimax_success(
    state: &AppState,
    context: &UsageContext,
    metrics: &UsageMetrics,
) {
    record_usage_success(state, context, metrics);
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

fn augment_codex_models_json(body: &Bytes, state: &AppState) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.clone();
    };
    let Some(root) = value.as_object_mut() else {
        return body.clone();
    };
    if !root.get("models").is_some_and(|value| value.is_array()) {
        root.insert("models".to_string(), serde_json::json!([]));
    }
    let models = root
        .get_mut("models")
        .and_then(|value| value.as_array_mut())
        .expect("models was initialized as an array");

    let mut existing = models
        .iter()
        .filter_map(|model| model.get("slug").and_then(|value| value.as_str()))
        .map(|slug| slug.to_string())
        .collect::<HashSet<_>>();

    for model in codex_provider_model_metadata(state) {
        let Some(slug) = model.get("slug").and_then(|value| value.as_str()) else {
            continue;
        };
        if existing.insert(slug.to_string()) {
            models.push(model);
        }
    }

    serde_json::to_vec(&value)
        .map(Bytes::from)
        .unwrap_or_else(|_| body.clone())
}

fn codex_provider_model_metadata(state: &AppState) -> Vec<serde_json::Value> {
    let mut models = Vec::new();
    if state
        .deepseek_accounts
        .lock()
        .unwrap()
        .iter()
        .any(|account| account.enabled)
    {
        models.push(codex_provider_model(
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            "DeepSeek model routed through the configured DeepSeek account.",
            64_000,
            true,
        ));
        models.push(codex_provider_model(
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            "Fast DeepSeek model routed through the configured DeepSeek account.",
            64_000,
            false,
        ));
    }
    if state
        .gemini_accounts
        .lock()
        .unwrap()
        .iter()
        .any(|account| account.enabled)
    {
        models.push(codex_provider_model(
            "gemini-2.5-pro",
            "Gemini 2.5 Pro",
            "Gemini model routed through the configured Gemini account.",
            1_048_576,
            true,
        ));
        models.push(codex_provider_model(
            "gemini-2.5-flash",
            "Gemini 2.5 Flash",
            "Fast Gemini model routed through the configured Gemini account.",
            1_048_576,
            true,
        ));
        models.push(codex_provider_model(
            "gemini-3-pro",
            "Gemini 3 Pro",
            "Gemini model routed through the configured Gemini account.",
            1_048_576,
            true,
        ));
    }
    if state
        .grok_accounts
        .lock()
        .unwrap()
        .iter()
        .any(|account| account.enabled)
    {
        models.push(codex_provider_model(
            "grok-4.3",
            "Grok 4.3",
            "Grok model routed through the configured xAI account.",
            256_000,
            true,
        ));
        models.push(codex_provider_model(
            "grok-4.1",
            "Grok 4.1",
            "Grok model routed through the configured xAI account.",
            256_000,
            true,
        ));
        models.push(codex_provider_model(
            "grok-3",
            "Grok 3",
            "Grok model routed through the configured xAI account.",
            131_072,
            false,
        ));
    }
    if state
        .minimax_accounts
        .lock()
        .unwrap()
        .iter()
        .any(|account| account.enabled)
    {
        models.push(codex_provider_model(
            "MiniMax-M3",
            "MiniMax M3",
            "MiniMax model routed through the configured MiniMax account.",
            512_000,
            true,
        ));
        models.push(codex_provider_model(
            "MiniMax-M2.7",
            "MiniMax M2.7",
            "MiniMax model routed through the configured MiniMax account.",
            512_000,
            false,
        ));
        models.push(codex_provider_model(
            "MiniMax-M2.7-highspeed",
            "MiniMax M2.7 Highspeed",
            "Fast MiniMax model routed through the configured MiniMax account.",
            512_000,
            true,
        ));
    }
    models
}

fn codex_provider_model(
    slug: &str,
    display_name: &str,
    description: &str,
    context_window: u64,
    supports_reasoning: bool,
) -> serde_json::Value {
    let reasoning_levels = if supports_reasoning {
        serde_json::json!([
            { "effort": "low", "description": "Fast responses with lighter reasoning" },
            { "effort": "medium", "description": "Balanced reasoning depth" },
            { "effort": "high", "description": "More reasoning depth" }
        ])
    } else {
        serde_json::json!([])
    };

    let mut model = serde_json::json!({
        "slug": slug,
        "priority": 1,
        "display_name": display_name,
        "description": description,
        "context_window": context_window,
        "max_context_window": context_window,
        "input_modalities": ["text"],
        "supports_parallel_tool_calls": true,
        "supports_image_detail_original": false,
        "supports_search_tool": false,
        "support_verbosity": false,
        "default_verbosity": "low",
        "supported_in_api": true,
        "visibility": "list",
        "shell_type": "shell_command",
        "tool_mode": null,
        "apply_patch_tool_type": "freeform",
        "web_search_tool_type": "text_and_image",
        "default_reasoning_level": if supports_reasoning { "medium" } else { "none" },
        "supported_reasoning_levels": reasoning_levels,
        "default_reasoning_summary": "none",
        "reasoning_summary_format": "experimental",
        "supports_reasoning_summaries": false,
        "prefer_websockets": false,
        "use_responses_lite": false,
        "base_instructions": "You are a coding agent. Follow the user's instructions and use tools carefully."
    });

    let object = model
        .as_object_mut()
        .expect("codex provider model metadata is an object");
    object.insert(
        "auto_compact_token_limit".to_string(),
        serde_json::Value::Null,
    );
    object.insert(
        "minimal_client_version".to_string(),
        serde_json::Value::Null,
    );
    object.insert("comp_hash".to_string(), serde_json::Value::Null);
    object.insert("availability_nux".to_string(), serde_json::Value::Null);
    object.insert("upgrade".to_string(), serde_json::Value::Null);
    object.insert(
        "available_in_plans".to_string(),
        serde_json::json!(["free", "plus", "pro", "team", "enterprise"]),
    );
    object.insert("additional_speed_tiers".to_string(), serde_json::json!([]));
    object.insert("default_service_tier".to_string(), serde_json::Value::Null);
    object.insert("service_tiers".to_string(), serde_json::json!([]));
    object.insert(
        "experimental_supported_tools".to_string(),
        serde_json::json!([]),
    );
    object.insert("multi_agent_version".to_string(), serde_json::Value::Null);
    object.insert(
        "truncation_policy".to_string(),
        serde_json::json!({
            "mode": "tokens",
            "limit": context_window
        }),
    );
    object.insert(
        "auto_review_model_override".to_string(),
        serde_json::Value::Null,
    );
    object.insert(
        "model_messages".to_string(),
        serde_json::json!({
            "instructions_template": "You are a coding agent. Follow the user's instructions and use tools carefully.\n\n{{ personality }}",
            "instructions_variables": {}
        }),
    );

    model
}

fn should_drop_incoming_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if is_hop_header(&lower)
        || lower == "authorization"
        || lower == "accept-encoding"
        || lower == "host"
        || lower == "content-length"
        || lower == "version"
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
    let mut cfg: Config = serde_json::from_str(&data).expect("invalid config.json");
    admin_auth::apply_env_overrides(&mut cfg.admin_auth);
    cfg
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

fn is_codex_models_list_path(raw_path: &str) -> bool {
    raw_path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .eq("codex/models")
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
