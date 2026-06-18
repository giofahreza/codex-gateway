use axum::{
    extract::{Form, Query, State},
    response::IntoResponse,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CallbackForm {
    pub redirect_url: String,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginStatusQuery {
    pub state: String,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .grok_accounts
            .iter()
            .map(|u| (u.key.clone(), (u.requests, u.errors)))
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .grok_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|a| {
            let stats_key = crate::grok_stats_key(a);
            let (requests, errors) =
                usage_by_key.get(&stats_key).copied().unwrap_or((0, 0));
            serde_json::json!({
                "label": a.label,
                "email": a.email,
                "file_name": a.file_name,
                "enabled": a.enabled,
                "requests": requests,
                "errors": errors,
                "expired_at": a.expires_at
            })
        })
        .collect::<Vec<_>>();

    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn login_start(State(state): State<crate::AppState>) -> impl IntoResponse {
    let pending = super::auth::PendingOAuth::new();
    let url = pending.build_authorize_url();
    let state_token = pending.state_token.clone();

    {
        let mut lock = state.grok_oauth_pending.lock().unwrap();
        lock.insert(state_token.clone(), pending);
    }

    axum::Json(serde_json::json!({
        "ok": true,
        "state": state_token,
        "url": url,
        "message": "Open the URL, complete login, then paste the URL you're redirected to."
    }))
    .into_response()
}

pub async fn login_submit(
    State(state): State<crate::AppState>,
    Form(form): Form<CallbackForm>,
) -> impl IntoResponse {
    let redirect_url = form.redirect_url.trim();
    if redirect_url.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "Callback URL is required"
        }))
        .into_response();
    }

    // Parse callback URL
    let parsed = match url::Url::parse(redirect_url) {
        Ok(url) => url,
        Err(_) => {
            // Try adding a scheme
            match url::Url::parse(&format!("http://{}", redirect_url)) {
                Ok(url) => url,
                Err(_) => {
                    return axum::Json(serde_json::json!({
                        "ok": false,
                        "message": "Invalid callback URL"
                    }))
                    .into_response();
                }
            }
        }
    };

    let code = match parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
    {
        Some(code) if !code.is_empty() => code,
        _ => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": "No authorization code found in callback URL"
            }))
            .into_response();
        }
    };

    let state_param = parsed
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .or_else(|| form.state.clone());

    // Find pending OAuth state
    let pending = if let Some(ref st) = state_param {
        let mut lock = state.grok_oauth_pending.lock().unwrap();
        lock.remove(st)
    } else {
        // Try all pending states
        let mut lock = state.grok_oauth_pending.lock().unwrap();
        if lock.is_empty() {
            None
        } else {
            let keys: Vec<_> = lock.keys().cloned().collect();
            lock.remove(&keys[0])
        }
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            // Allow code exchange without a stored state (for copy-paste flows)
            // Create a temporary pending with empty verifier/challenge
            // This may fail with xAI if they validate the challenge
            super::auth::PendingOAuth::new()
        }
    };

    // Exchange code
    match super::auth::exchange_code(&state.client, &code, &pending).await {
        Ok(token) => {
            // Extract email from id_token JWT
            let email = token
                .id_token
                .as_deref()
                .and_then(|jwt| {
                    jwt.split('.')
                        .nth(1)
                        .and_then(|payload| {
                            use base64::Engine;
                            let padded = payload.to_string() + "=";
                            base64::engine::general_purpose::URL_SAFE
                                .decode(padded.as_bytes())
                                .ok()
                        })
                        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                        .and_then(|claims| {
                            claims
                                .get("email")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string())
                        })
                });

            let email_display = email.as_deref().unwrap_or("unknown@x.ai");
            let label = email.as_deref().unwrap_or("grok");

            match super::auth::save_auth(&state.cfg, &token, Some(label), email.as_deref()) {
                Ok(path) => {
                    super::accounts::reload_state(&state);
                    axum::Json(serde_json::json!({
                        "ok": true,
                        "message": format!("Grok account saved: {} ({})", email_display, path),
                        "email": email,
                        "saved_path": path
                    }))
                    .into_response()
                }
                Err(err) => axum::Json(serde_json::json!({
                    "ok": false,
                    "message": format!("Failed to save credentials: {}", err)
                }))
                .into_response(),
            }
        }
        Err(err) => axum::Json(serde_json::json!({
            "ok": false,
            "message": err
        }))
        .into_response(),
    }
}

pub async fn login_status(
    State(state): State<crate::AppState>,
    Query(query): Query<LoginStatusQuery>,
) -> impl IntoResponse {
    let lock = state.grok_oauth_pending.lock().unwrap();
    let pending = lock.get(&query.state);
    axum::Json(serde_json::json!({
        "ok": true,
        "pending": pending.is_some()
    }))
    .into_response()
}
