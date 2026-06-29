use axum::{
    body::Bytes,
    extract::{Form, State},
    http::{Method, StatusCode},
    response::{Html, IntoResponse},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct LoginStartRequest {
    github_token: Option<String>,
    label: Option<String>,
    account_type: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginSubmitForm {
    pub device_code: String,
    pub label: Option<String>,
    pub account_type: Option<String>,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .copilot_accounts
            .iter()
            .map(|usage| (usage.key.clone(), usage.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .copilot_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            let stats_key = crate::copilot_stats_key(account);
            let usage = usage_by_key.get(&stats_key).cloned().unwrap_or_default();
            serde_json::json!({
                "account_id": account.account_id,
                "label": account.label,
                "login": account.login,
                "file_name": account.file_name,
                "enabled": account.enabled,
                "account_type": account.account_type,
                "copilot_expires_at": unix_to_rfc3339(account.copilot_expires_at),
                "models": account.models.iter().map(|model| serde_json::json!({
                    "model_id": format!("cop:{}", model.id),
                    "display_name": model.name.as_deref().unwrap_or(&model.id),
                    "upstream_model": model.id,
                    "vendor": model.vendor,
                    "preview": model.preview
                })).collect::<Vec<_>>(),
                "requests": usage.requests,
                "errors": usage.errors,
                "prompt_total": usage.prompt_total,
                "prompt_error_total": usage.prompt_error_total,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "total_tokens": usage.total_tokens,
                "cache_tokens": usage.cache_tokens,
                "reasoning_tokens": usage.reasoning_tokens,
                "last_success_at": usage.last_success_at,
                "last_error_at": usage.last_error_at
            })
        })
        .collect::<Vec<_>>();

    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn quota_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let accounts = state
        .copilot_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            serde_json::json!({
                "label": account.label,
                "login": account.login,
                "file_name": account.file_name,
                "account_type": account.account_type,
                "is_available": account.enabled,
                "status_msg": "GitHub Copilot does not expose quota or reset counters through this token endpoint.",
                "available_models": account.models.iter().map(|model| serde_json::json!({
                    "model_id": format!("cop:{}", model.id),
                    "display_name": model.name.as_deref().unwrap_or(&model.id),
                    "upstream_model": model.id,
                    "vendor": model.vendor,
                    "preview": model.preview
                })).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    axum::Json(serde_json::json!({ "accounts": accounts }))
}

fn unix_to_rfc3339(value: Option<i64>) -> Option<String> {
    value.and_then(|timestamp| {
        chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
            .map(|datetime| datetime.to_rfc3339())
    })
}

pub async fn login_start(
    State(state): State<crate::AppState>,
    method: Method,
    body: Bytes,
) -> impl IntoResponse {
    match method {
        Method::GET => Html(helper_html()).into_response(),
        Method::POST => start_or_save(&state, &body).await,
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            "Copilot setup only supports GET for instructions or POST for JSON",
        )
            .into_response(),
    }
}

pub async fn login_submit(
    State(state): State<crate::AppState>,
    Form(form): Form<LoginSubmitForm>,
) -> impl IntoResponse {
    let device_code = form.device_code.trim();
    if device_code.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "device_code is required"
        }))
        .into_response();
    }

    let pending = {
        let pending = state.copilot_oauth_pending.lock().unwrap();
        pending.get(device_code).cloned()
    };
    let Some(pending) = pending else {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "unknown or expired device_code; start login again"
        }))
        .into_response();
    };
    if pending.expires_at_unix <= chrono::Utc::now().timestamp() {
        state
            .copilot_oauth_pending
            .lock()
            .unwrap()
            .remove(device_code);
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "device_code expired; start login again"
        }))
        .into_response();
    }

    let github_token = match super::auth::poll_access_token_once(&state.client, device_code).await {
        Ok(token) => token,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response()
        }
    };
    let account_type = form
        .account_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&pending.account_type)
        .to_string();
    match save_from_github_token(&state, &github_token, &account_type, form.label.as_deref()).await
    {
        Ok(saved_path) => {
            state
                .copilot_oauth_pending
                .lock()
                .unwrap()
                .remove(device_code);
            axum::Json(serde_json::json!({
                "ok": true,
                "message": format!("saved Copilot credentials to {}", saved_path),
                "saved_path": saved_path
            }))
            .into_response()
        }
        Err(err) => axum::Json(serde_json::json!({
            "ok": false,
            "message": err
        }))
        .into_response(),
    }
}

async fn start_or_save(state: &crate::AppState, body: &Bytes) -> axum::response::Response {
    let payload: LoginStartRequest = if body.is_empty() {
        LoginStartRequest {
            github_token: None,
            label: None,
            account_type: None,
        }
    } else {
        match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => {
                return axum::Json(serde_json::json!({
                    "ok": false,
                    "message": "Submit JSON: {\"github_token\":\"optional\",\"label\":\"optional\",\"account_type\":\"individual|business|enterprise\"}"
                }))
                .into_response()
            }
        }
    };

    let account_type = super::auth::normalize_account_type(payload.account_type.as_deref());
    if let Some(github_token) = payload
        .github_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return match save_from_github_token(
            state,
            github_token,
            &account_type,
            payload.label.as_deref(),
        )
        .await
        {
            Ok(saved_path) => axum::Json(serde_json::json!({
                "ok": true,
                "message": format!("saved Copilot credentials to {}", saved_path),
                "saved_path": saved_path
            }))
            .into_response(),
            Err(err) => axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response(),
        };
    }

    let device = match super::auth::start_device_flow(&state.client).await {
        Ok(device) => device,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response()
        }
    };
    let expires_at_unix = chrono::Utc::now()
        .timestamp()
        .saturating_add(device.expires_in);
    let pending = super::auth::PendingDevice {
        device_code: device.device_code.clone(),
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        expires_at_unix,
        interval: device.interval,
        account_type,
    };
    state
        .copilot_oauth_pending
        .lock()
        .unwrap()
        .insert(device.device_code.clone(), pending);

    axum::Json(serde_json::json!({
        "ok": true,
        "mode": "device_code",
        "device_code": device.device_code,
        "user_code": device.user_code,
        "verification_uri": device.verification_uri,
        "expires_in": device.expires_in,
        "interval": device.interval,
        "message": format!("Open {} and enter code {}", device.verification_uri, device.user_code)
    }))
    .into_response()
}

async fn save_from_github_token(
    state: &crate::AppState,
    github_token: &str,
    account_type: &str,
    label: Option<&str>,
) -> Result<String, String> {
    let user = super::auth::get_github_user(&state.client, github_token).await?;
    let copilot_token = super::auth::fetch_copilot_token(&state.client, github_token).await?;
    let models = super::api::fetch_models(&state.client, account_type, &copilot_token.token)
        .await
        .unwrap_or_default();
    super::auth::save_auth(
        state,
        github_token,
        account_type,
        label,
        &user,
        &copilot_token,
        &models,
    )
}

fn helper_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>GitHub Copilot Provider Setup</title>
</head>
<body>
  <h1>GitHub Copilot Provider Setup</h1>
  <p>Start device-code login:</p>
  <pre>curl -X POST /login/copilot/start -H 'Content-Type: application/json' -d '{"account_type":"individual"}'</pre>
  <p>Open the returned verification URL, enter the user code, then submit the returned device code:</p>
  <pre>curl -X POST /login/copilot/submit -d 'device_code=DEVICE_CODE'</pre>
  <p>You can also paste a GitHub token directly:</p>
  <pre>{"github_token":"YOUR_GITHUB_TOKEN","label":"optional","account_type":"individual"}</pre>
  <p>Use model ids with the <code>cop:</code> prefix, for example <code>cop:gpt-5.1</code> for Codex CLI or <code>cop:claude-sonnet-4.5</code> for Claude Code.</p>
</body>
</html>"#
        .to_string()
}
