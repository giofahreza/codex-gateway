use axum::{
    body::Bytes,
    extract::State,
    http::{Method, StatusCode},
    response::{Html, IntoResponse},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct LoginStartRequest {
    api_key: String,
    label: Option<String>,
    base_url: Option<String>,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .minimax_accounts
            .iter()
            .map(|usage| (usage.key.clone(), usage.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .minimax_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            let stats_key = crate::minimax_stats_key(account);
            let usage = usage_by_key.get(&stats_key).cloned().unwrap_or_default();
            serde_json::json!({
                "account_id": account.account_id,
                "label": account.label,
                "file_name": account.file_name,
                "enabled": account.enabled,
                "base_url": account.base_url,
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
            "MiniMax setup only supports GET for instructions or POST for API key submission",
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
                "message": "Submit MiniMax credentials as JSON: {\"api_key\":\"...\",\"label\":\"optional\",\"base_url\":\"optional\"}"
            }))
            .into_response();
        }
    };

    let api_key = payload.api_key.trim();
    if api_key.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "MiniMax api_key is required"
        }))
        .into_response();
    }

    let base_url = super::api::normalize_base_url(payload.base_url.as_deref());
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
        format!("minimax-{}", suffix)
    });
    let account_id = label.clone();

    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let file_name = format!("minimax-{}.json", sanitize_label(&label));
    let path = std::path::Path::new(&auth_dir).join(file_name);
    let now = chrono::Utc::now().to_rfc3339();
    let out = serde_json::json!({
        "type": "minimax",
        "account_id": account_id,
        "label": label,
        "api_key": api_key,
        "base_url": base_url,
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
    axum::Json(serde_json::json!({
        "ok": true,
        "message": format!("saved MiniMax credentials to {}", path.to_string_lossy()),
        "saved_path": path.to_string_lossy()
    }))
    .into_response()
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
    r#"<!doctype html><html><head><meta charset="utf-8"><title>MiniMax Setup</title></head><body>
<h1>MiniMax Setup</h1>
<p>Open <a href="https://platform.minimax.io" target="_blank" rel="noopener">https://platform.minimax.io</a>, sign in, create an API key, then paste it into the dashboard MiniMax modal.</p>
<p>You can also POST the key directly to <code>/login/minimax/start</code> with JSON <code>{"api_key":"...","label":"optional","base_url":"optional"}</code>.</p>
</body></html>"#
    .to_string()
}
