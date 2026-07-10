use axum::{
    extract::{Form, State},
    response::IntoResponse,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct CallbackForm {
    pub redirect_url: String,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .agw_accounts
            .iter()
            .map(|usage| (usage.key.clone(), usage.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    };
    let accounts = state
        .agw_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            let stats_key = crate::antigravity_stats_key(account);
            let usage = usage_by_key.get(&stats_key).cloned().unwrap_or_default();
            serde_json::json!({
                "label": account.label,
                "email": account.email,
                "file_name": account.file_name,
                "enabled": account.enabled,
                "project_id": account.project_id,
                "expired_at": account.access_token_expires_at,
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
    let (url, state_token, code_verifier) = match super::auth::build_auth_url() {
        Ok(values) => values,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to create auth url: {}", err)
            }))
            .into_response()
        }
    };

    {
        let mut pending = state.agw_oauth_pending.lock().unwrap();
        pending.insert(
            state_token.clone(),
            super::auth::PendingOAuth {
                code_verifier,
                created_at: std::time::Instant::now(),
            },
        );
    }

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

    let (code, state_token) = match super::auth::parse_oauth_callback(redirect_url) {
        Ok(values) => values,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response()
        }
    };

    let code_verifier = {
        let mut pending = state.agw_oauth_pending.lock().unwrap();
        match pending.remove(&state_token) {
            Some(pending) => pending.code_verifier,
            None => {
                return axum::Json(serde_json::json!({
                    "ok": false,
                    "message": "invalid or expired state"
                }))
                .into_response()
            }
        }
    };

    let token_resp =
        match super::auth::exchange_code_for_tokens(&state.client, &code, &code_verifier).await {
            Ok(token_resp) => token_resp,
            Err(err) => {
                return axum::Json(serde_json::json!({
                    "ok": false,
                    "message": err
                }))
                .into_response()
            }
        };

    let email = match super::auth::get_user_email(&state.client, &token_resp.access_token).await {
        Ok(email) => email,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response()
        }
    };

    let project_id = super::auth::discover_project_id(&state.client, &token_resp.access_token)
        .await
        .unwrap_or(None);
    match super::auth::save_auth(&state, &email, &token_resp, project_id) {
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
    }
}
