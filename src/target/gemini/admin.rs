use axum::{
    extract::{Form, State},
    response::IntoResponse,
};
use serde::Deserialize;

const GEMINI_OAUTH_PENDING_TTL: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Deserialize)]
pub struct CallbackForm {
    pub redirect_url: String,
    pub project_id: Option<String>,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .gemini_accounts
            .iter()
            .map(|usage| (usage.key.clone(), usage.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .gemini_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            let stats_key = crate::gemini_stats_key(account);
            let usage = usage_by_key.get(&stats_key).cloned().unwrap_or_default();
            let runtime =
                crate::router_account_runtime_json(&state, "gemini", &stats_key, account.enabled);
            serde_json::json!({
                "label": account.label,
                "email": account.email,
                "file_name": account.file_name,
                "enabled": account.enabled,
                "runtime": runtime,
                "project_id": account.project_id,
                "expired_at": account.expiry,
                "auto": account.auto,
                "checked": account.checked,
                "requests": usage.requests,
                "errors": usage.errors,
                "last_success_at": usage.last_success_at,
                "last_error_at": usage.last_error_at,
                "last_error_message": usage.last_error_message
            })
        })
        .collect::<Vec<_>>();

    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn login_start(State(state): State<crate::AppState>) -> impl IntoResponse {
    let (url, state_token) = match super::auth::build_auth_url() {
        Ok(values) => values,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to create auth url: {}", err)
            }))
            .into_response();
        }
    };

    let mut pending = state.gemini_oauth_pending.lock().unwrap();
    pending.retain(|_, started_at| oauth_state_is_valid(*started_at));
    pending.insert(state_token.clone(), std::time::Instant::now());
    axum::Json(serde_json::json!({ "url": url, "state": state_token })).into_response()
}

pub async fn login_submit(
    State(state): State<crate::AppState>,
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

    let requested_project = form.project_id.unwrap_or_default().trim().to_string();
    let (code, state_token) = match super::auth::parse_oauth_callback(redirect_url) {
        Ok(values) => values,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    let started_at = state
        .gemini_oauth_pending
        .lock()
        .unwrap()
        .remove(&state_token);
    if !started_at.is_some_and(oauth_state_is_valid) {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "invalid or expired state"
        }))
        .into_response();
    }

    let token_resp = match super::auth::exchange_code_for_tokens(&state.client, &code).await {
        Ok(token_resp) => token_resp,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    let email = match super::auth::get_user_email(&state.client, &token_resp.access_token).await {
        Ok(email) => email,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    let setup = match super::auth::ensure_project_and_onboard(
        &state.client,
        &token_resp.access_token,
        &requested_project,
    )
    .await
    {
        Ok(setup) => setup,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    match super::auth::save_auth(
        &state,
        &email,
        &token_resp,
        &setup.project_id,
        setup.auto_project,
        true,
    ) {
        Ok(saved_path) => axum::Json(serde_json::json!({
            "ok": true,
            "message": format!("saved Gemini credentials to {}", saved_path)
        }))
        .into_response(),
        Err(err) => axum::Json(serde_json::json!({
            "ok": false,
            "message": err
        }))
        .into_response(),
    }
}

fn oauth_state_is_valid(started_at: std::time::Instant) -> bool {
    started_at.elapsed() <= GEMINI_OAUTH_PENDING_TTL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_state_expires_after_five_minutes() {
        assert!(oauth_state_is_valid(std::time::Instant::now()));
        assert!(!oauth_state_is_valid(
            std::time::Instant::now()
                - GEMINI_OAUTH_PENDING_TTL
                - std::time::Duration::from_secs(1)
        ));
    }
}
