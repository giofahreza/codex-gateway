use axum::{
    extract::{Query, State},
    response::IntoResponse,
};
use rand::{distr::Alphanumeric, Rng};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct LoginStatusQuery {
    pub state: String,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .qwen_accounts
            .iter()
            .map(|usage| (usage.key.clone(), (usage.requests, usage.errors)))
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .qwen_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            let stats_key = crate::qwen_stats_key(account);
            let (requests, errors) = usage_by_key.get(&stats_key).copied().unwrap_or((0, 0));
            serde_json::json!({
                "label": account.label,
                "email": account.email,
                "file_name": account.file_name,
                "enabled": account.enabled,
                "resource_url": account.resource_url,
                "expired_at": account.expired_at,
                "requests": requests,
                "errors": errors
            })
        })
        .collect::<Vec<_>>();

    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn login_start(State(state): State<crate::AppState>) -> impl IntoResponse {
    let session_id: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect();

    let (flow, code_verifier) = match super::auth::initiate_device_flow(&state.client).await {
        Ok(values) => values,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    super::auth::track_pending_login(&state, &session_id, &flow);
    super::auth::spawn_device_poll(
        state.clone(),
        session_id.clone(),
        flow.device_code.clone(),
        code_verifier,
        flow.interval.unwrap_or(5),
    );

    axum::Json(serde_json::json!({
        "ok": true,
        "state": session_id,
        "url": flow.verification_uri_complete,
        "verification_uri": flow.verification_uri,
        "user_code": flow.user_code,
        "expires_in": flow.expires_in,
        "interval": flow.interval.unwrap_or(5)
    }))
    .into_response()
}

pub async fn login_status(
    State(state): State<crate::AppState>,
    Query(query): Query<LoginStatusQuery>,
) -> impl IntoResponse {
    let pending = state.qwen_oauth_pending.lock().unwrap();
    let Some(entry) = pending.get(query.state.trim()) else {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "invalid or expired Qwen login state"
        }))
        .into_response();
    };

    let elapsed = entry.created_at.elapsed().as_secs();
    let expires_in = entry
        .expires_at
        .checked_duration_since(std::time::Instant::now())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let payload = match &entry.status {
        super::auth::PendingStatus::Pending => {
            let status = if std::time::Instant::now() >= entry.expires_at {
                "expired"
            } else {
                "pending"
            };
            serde_json::json!({
                "ok": true,
                "status": status,
                "message": if status == "expired" {
                    "Qwen device code expired; start login again"
                } else {
                    "Waiting for Qwen authorization"
                },
                "url": entry.verification_uri_complete,
                "user_code": entry.user_code,
                "interval": entry.interval_seconds,
                "elapsed_seconds": elapsed,
                "expires_in": expires_in
            })
        }
        super::auth::PendingStatus::Completed { saved_path, label } => serde_json::json!({
            "ok": true,
            "status": "completed",
            "message": format!("saved Qwen credentials to {}", saved_path),
            "saved_path": saved_path,
            "label": label
        }),
        super::auth::PendingStatus::Error { message } => serde_json::json!({
            "ok": false,
            "status": "error",
            "message": message
        }),
    };

    axum::Json(payload).into_response()
}
