use axum::{
    extract::{Form, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use super::{auth, quota, tokens};

#[derive(Deserialize)]
pub(crate) struct CallbackForm {
    redirect_url: String,
}

#[derive(Deserialize)]
pub(crate) struct DeleteForm {
    file_name: String,
}

#[derive(Deserialize)]
pub(crate) struct ToggleForm {
    file_name: String,
    enabled: String,
}

pub async fn quota_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let accounts = quota::get_quota_summaries(&state).await;
    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn login_start(State(state): State<crate::AppState>) -> impl IntoResponse {
    let (url, state_token, code_verifier) = match auth::build_auth_url() {
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
    let (code, state_token) = match auth::parse_oauth_callback(redirect_url) {
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

    match auth::exchange_code_for_tokens(&state.client, &code, &code_verifier).await {
        Ok(token_resp) => match auth::save_auth(&state, &token_resp) {
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

pub async fn delete_credential(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> impl IntoResponse {
    if !crate::check_api_key(&headers, &state.cfg.proxy_api_key) {
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
            tokens::reload_state(&state);
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

pub async fn toggle_credential(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Form(form): Form<ToggleForm>,
) -> impl IntoResponse {
    if !crate::check_api_key(&headers, &state.cfg.proxy_api_key) {
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

    if let Err(err) = persist_disabled_list(&state.disabled) {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": format!("failed to persist: {}", err)
        }))
        .into_response();
    }

    tokens::reload_state(&state);

    axum::Json(serde_json::json!({
        "ok": true,
        "message": format!("{} {}", if enable { "enabled" } else { "disabled" }, file_name)
    }))
    .into_response()
}

fn persist_disabled_list(disabled: &Arc<Mutex<HashSet<String>>>) -> Result<(), String> {
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
