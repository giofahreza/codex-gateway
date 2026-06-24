use axum::{
    extract::{Form, State},
    response::IntoResponse,
};
use serde::Deserialize;

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
            serde_json::json!({
                "label": account.label,
                "email": account.email,
                "file_name": account.file_name,
                "enabled": account.enabled,
                "project_id": account.project_id,
                "expired_at": account.expiry,
                "auto": account.auto,
                "checked": account.checked,
                "requests": usage.requests,
                "errors": usage.errors,
                "last_success_at": usage.last_success_at,
                "last_error_at": usage.last_error_at
            })
        })
        .collect::<Vec<_>>();

    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn quota_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let accounts = super::quota::get_quota_summaries(&state).await;
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

    state
        .gemini_oauth_pending
        .lock()
        .unwrap()
        .insert(state_token.clone());
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

    let removed = state
        .gemini_oauth_pending
        .lock()
        .unwrap()
        .remove(&state_token);
    if !removed {
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

    let project_id = if !requested_project.is_empty() {
        requested_project.clone()
    } else {
        if let Ok(Some(project_id)) =
            super::auth::discover_project_id(&state.client, &token_resp.access_token, None).await
        {
            project_id
        } else {
            let projects =
                match super::auth::fetch_projects(&state.client, &token_resp.access_token).await {
                    Ok(projects) => projects,
                    Err(err) => {
                        return axum::Json(serde_json::json!({
                            "ok": false,
                            "message": err
                        }))
                        .into_response();
                    }
                };

            if projects.len() == 1 {
                projects[0].project_id.clone()
            } else if projects.is_empty() {
                return axum::Json(serde_json::json!({
                    "ok": false,
                    "message": "no Google Cloud projects were returned for this account; retry with an explicit project id"
                }))
                .into_response();
            } else {
                let project_list = projects
                    .iter()
                    .map(|project| format!("{} ({})", project.project_id, project.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                return axum::Json(serde_json::json!({
                    "ok": false,
                    "message": format!("multiple Google Cloud projects found for this account; retry with a project id. Available projects: {}", project_list),
                    "projects": projects
                }))
                .into_response();
            }
        }
    };

    let final_project = match super::auth::ensure_project_and_onboard(
        &state.client,
        &token_resp.access_token,
        &project_id,
    )
    .await
    {
        Ok(project_id) => project_id,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    if let Err(err) = super::auth::ensure_cloud_api_enabled(
        &state.client,
        &token_resp.access_token,
        &final_project,
    )
    .await
    {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": err
        }))
        .into_response();
    }

    match super::auth::save_auth(
        &state,
        &email,
        &token_resp,
        &final_project,
        requested_project.is_empty(),
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
