use axum::{
    body::Bytes,
    extract::State,
    http::{Method, StatusCode},
    response::{Html, IntoResponse},
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct LoginStartRequest {
    api_key: String,
    label: Option<String>,
    account_type: Option<String>,
    base_url: Option<String>,
    openai_base_url: Option<String>,
    anthropic_base_url: Option<String>,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .glm_accounts
            .iter()
            .map(|usage| (usage.key.clone(), usage.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .glm_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            let stats_key = crate::glm_stats_key(account);
            let usage = usage_by_key.get(&stats_key).cloned().unwrap_or_default();
            let runtime =
                crate::router_account_runtime_json(&state, "glm", &stats_key, account.enabled);
            serde_json::json!({
                "account_id": account.account_id,
                "label": account.label,
                "account_type": account.normalized_account_type(),
                "file_name": account.file_name,
                "enabled": account.enabled,
                "runtime": runtime,
                "base_url": account.openai_base_url(),
                "openai_base_url": account.openai_base_url(),
                "anthropic_base_url": account.anthropic_base_url,
                "requests": usage.requests,
                "errors": usage.errors,
                "prompt_total": usage.prompt_total,
                "prompt_error_total": usage.prompt_error_total,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "total_tokens": usage.total_tokens,
                "cache_tokens": usage.cache_tokens,
                "reasoning_tokens": usage.reasoning_tokens,
                "first_seen_at": usage.first_seen_at,
                "last_seen_at": usage.last_seen_at,
                "last_success_at": usage.last_success_at,
                "last_error_at": usage.last_error_at,
                "last_error_message": usage.last_error_message
            })
        })
        .collect::<Vec<_>>();

    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn login_start(
    State(state): State<crate::AppState>,
    method: Method,
    body: Bytes,
) -> impl IntoResponse {
    match method {
        Method::GET => Html(helper_html()).into_response(),
        Method::POST => save_account(&state, &body).await,
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            "GLM setup only supports GET for instructions or POST for API key submission",
        )
            .into_response(),
    }
}

async fn save_account(state: &crate::AppState, body: &Bytes) -> axum::response::Response {
    let payload: LoginStartRequest = match serde_json::from_slice(body) {
        Ok(payload) => payload,
        Err(_) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": "Submit GLM credentials as JSON: {\"api_key\":\"...\",\"label\":\"optional\",\"account_type\":\"api_usage|subscription\",\"base_url\":\"optional\",\"anthropic_base_url\":\"optional\"}"
            }))
            .into_response();
        }
    };

    let api_key = payload.api_key.trim();
    if api_key.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "GLM api_key is required"
        }))
        .into_response();
    }

    let base_url_input = payload
        .base_url
        .as_deref()
        .or(payload.openai_base_url.as_deref());
    let account_type = super::accounts::normalize_account_type(payload.account_type.as_deref());
    let base_url = super::api::normalize_base_url_for_account_type(base_url_input, &account_type);
    let anthropic_base_url = if account_type == super::accounts::ACCOUNT_TYPE_SUBSCRIPTION {
        Some(super::anthropic::normalize_anthropic_base_url(
            payload.anthropic_base_url.as_deref(),
        ))
    } else {
        payload
            .anthropic_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_end_matches('/').to_string())
    };
    if let Err(err) = super::api::validate_api_key(&state.client, api_key, &base_url).await {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": err
        }))
        .into_response();
    }

    let requested_label = payload
        .label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let label = requested_label.unwrap_or_else(|| {
        let suffix = api_key
            .chars()
            .rev()
            .take(6)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        format!("glm-{}", suffix)
    });
    let account_id = label.clone();

    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/io-gateway/auths".to_string());
    let file_name = format!("glm-{}.json", sanitize_label(&label));
    let path = std::path::Path::new(&auth_dir).join(file_name);
    let now = chrono::Utc::now().to_rfc3339();
    let out = serde_json::json!({
        "type": "glm",
        "account_id": account_id,
        "label": label,
        "account_type": account_type,
        "api_key": api_key,
        "base_url": base_url,
        "anthropic_base_url": anthropic_base_url,
        "validated_at": now
    });

    if let Err(err) = std::fs::create_dir_all(&auth_dir) {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": format!("failed to create auth dir: {}", err)
        }))
        .into_response();
    }
    if let Err(err) = super::super::atomic_write_json(&path, &out) {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": format!("failed to write auth file: {}", err)
        }))
        .into_response();
    }

    super::accounts::reload_state(state);

    // Probe the upstream balance endpoint for api_usage keys so the dashboard
    // can show users immediately whether their saved key is a Z.AI billing
    // credential (which exposes /api/finance/balance) or a chat-only key.
    // Subscription (Coding Plan) accounts do not have a numeric balance to
    // probe; we surface a plain "subscription" status for that.
    let balance_probe = probe_balance_after_save(state, &out, &account_type).await;

    let mut response = json!({
        "ok": true,
        "message": format!("saved GLM credentials to {}", path.to_string_lossy()),
        "saved_path": path.to_string_lossy(),
        "account_type": account_type,
    });
    if let Some(probe) = balance_probe {
        if let Value::Object(map) = &mut response {
            map.insert("balance_status".to_string(), Value::String(probe.status));
            map.insert("balance_message".to_string(), Value::String(probe.message));
            if let Some(balances) = probe.balances {
                map.insert("balances".to_string(), Value::Array(balances));
            }
        }
    }
    axum::Json(response).into_response()
}

#[derive(Default)]
struct BalanceProbeResult {
    status: String,
    message: String,
    balances: Option<Vec<Value>>,
}

async fn probe_balance_after_save(
    state: &crate::AppState,
    saved_account: &Value,
    account_type: &str,
) -> Option<BalanceProbeResult> {
    if account_type == super::accounts::ACCOUNT_TYPE_SUBSCRIPTION {
        return Some(BalanceProbeResult {
            status: "subscription".to_string(),
            message: "Coding Plan subscription; no numeric balance is exposed.".to_string(),
            balances: None,
        });
    }
    let account = super::accounts::GlmAccount {
        account_id: saved_account
            .get("account_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        label: saved_account
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        account_type: account_type.to_string(),
        api_key: saved_account
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        base_url: saved_account
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        anthropic_base_url: saved_account
            .get("anthropic_base_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        file_name: saved_account
            .get("file_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        enabled: true,
    };
    match super::quota::fetch_balance(&state.client, &account).await {
        Ok(super::quota::BalanceResult::Found(entries)) => {
            let balances: Vec<Value> = entries
                .into_iter()
                .map(|entry| {
                    json!({
                        "currency": entry.currency,
                        "total_balance": entry.total_balance,
                        "granted_balance": entry.granted_balance,
                        "topped_up_balance": entry.topped_up_balance,
                    })
                })
                .collect();
            let human = human_balance_summary(&balances);
            Some(BalanceProbeResult {
                status: "live".to_string(),
                message: format!("Live balance: {human}"),
                balances: Some(balances),
            })
        }
        Ok(super::quota::BalanceResult::NotAvailable { user_note, .. }) => {
            Some(BalanceProbeResult {
                status: "unreachable".to_string(),
                message: user_note.to_string(),
                balances: None,
            })
        }
        Err(err) => Some(BalanceProbeResult {
            status: "probe_error".to_string(),
            message: format!("Could not probe balance endpoint: {err}"),
            balances: None,
        }),
    }
}

fn human_balance_summary(balances: &[Value]) -> String {
    if balances.is_empty() {
        return "no balance entries".to_string();
    }
    let parts: Vec<String> = balances
        .iter()
        .map(|b| {
            let cur = b.get("currency").and_then(|v| v.as_str()).unwrap_or("");
            let total = b
                .get("total_balance")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("{total} {cur}")
        })
        .collect();
    parts.join(", ")
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn helper_html() -> String {
    r#"<!doctype html><html><head><meta charset="utf-8"><title>GLM Setup</title></head><body>
<h1>GLM Setup</h1>
<p>Open <a href="https://z.ai/manage-apikey/apikey-list" target="_blank" rel="noopener">Z.AI API Keys</a>, create an API key, then paste it into the dashboard GLM modal.</p>
<p>Use <code>account_type=api_usage</code> for normal API keys. Default OpenAI/Codex base URL: <code>https://api.z.ai/api/paas/v4</code>. Claude requests are translated through Chat Completions for this type.</p>
<p>Use <code>account_type=subscription</code> for GLM Coding Plan subscription keys. Default OpenAI/Codex base URL: <code>https://api.z.ai/api/coding/paas/v4</code>. Default Claude Code base URL: <code>https://api.z.ai/api/anthropic</code>.</p>
<p>You can also POST the key directly to <code>/login/glm/start</code> with JSON <code>{"api_key":"...","label":"optional","account_type":"api_usage","base_url":"optional","anthropic_base_url":"optional"}</code>.</p>
</body></html>"#
    .to_string()
}
