use axum::{
    extract::{Form, Query, State},
    response::IntoResponse,
};
use serde::Deserialize;
use std::collections::HashMap;

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
            let (requests, errors) = usage_by_key.get(&stats_key).copied().unwrap_or((0, 0));
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
        "message": "Open the URL, complete login, then paste the callback URL, ?code=...&state=... fragment, or the bare authorization code if xAI shows it directly."
    }))
    .into_response()
}

pub async fn login_submit(
    State(state): State<crate::AppState>,
    Form(form): Form<CallbackForm>,
) -> impl IntoResponse {
    let submitted = form.redirect_url.trim();
    if submitted.is_empty() {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "Callback URL or authorization code is required"
        }))
        .into_response();
    }

    let (code, state_param) = match parse_submitted_code(submitted, form.state.as_deref()) {
        Ok(result) => result,
        Err(message) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": message
            }))
            .into_response();
        }
    };

    // Find pending OAuth state
    let pending = if let Some(ref st) = state_param {
        let mut lock = state.grok_oauth_pending.lock().unwrap();
        lock.remove(st)
    } else {
        take_latest_pending_oauth(&mut state.grok_oauth_pending.lock().unwrap())
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": "No pending Grok login was found for that code. Click Start Login again, then paste the callback URL or authorization code from the same attempt."
            }))
            .into_response();
        }
    };

    // Exchange code
    match super::auth::exchange_code(&state.client, &code, &pending).await {
        Ok(token) => {
            // Extract email from id_token JWT
            let email = token.id_token.as_deref().and_then(|jwt| {
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

fn parse_submitted_code(
    submitted: &str,
    fallback_state: Option<&str>,
) -> Result<(String, Option<String>), String> {
    if let Ok(url) = url::Url::parse(submitted) {
        return parse_query_pairs(url.query_pairs().into_owned().collect(), fallback_state);
    }

    if let Ok(url) = url::Url::parse(&format!("http://{}", submitted)) {
        if url.query().is_some() {
            return parse_query_pairs(url.query_pairs().into_owned().collect(), fallback_state);
        }
    }

    let trimmed = submitted.trim_start_matches('?').trim();
    if trimmed.contains("code=") || trimmed.contains("state=") {
        let params = url::form_urlencoded::parse(trimmed.as_bytes())
            .into_owned()
            .collect::<Vec<_>>();
        return parse_query_pairs(params, fallback_state);
    }

    if is_probable_auth_code(trimmed) {
        return Ok((
            trimmed.to_string(),
            fallback_state
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        ));
    }

    Err("Invalid callback value. Paste the full callback URL, ?code=...&state=... fragment, or the bare authorization code shown by xAI.".to_string())
}

fn parse_query_pairs(
    pairs: Vec<(String, String)>,
    fallback_state: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let params = pairs.into_iter().collect::<HashMap<_, _>>();
    let code = params
        .get("code")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "No authorization code was found. Paste the full callback URL, ?code=...&state=... fragment, or the bare authorization code shown by xAI.".to_string())?;
    let state = params
        .get("state")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            fallback_state
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        });
    Ok((code.to_string(), state))
}

fn is_probable_auth_code(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(char::is_whitespace)
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~'))
}

fn take_latest_pending_oauth(
    pending: &mut HashMap<String, super::auth::PendingOAuth>,
) -> Option<super::auth::PendingOAuth> {
    let latest_state = pending
        .iter()
        .max_by_key(|(_, oauth)| oauth.created_at)
        .map(|(state, _)| state.clone())?;
    pending.remove(&latest_state)
}

#[cfg(test)]
mod tests {
    use super::parse_submitted_code;

    #[test]
    fn accepts_full_callback_url() {
        let (code, state) = parse_submitted_code(
            "http://127.0.0.1:56121/callback?code=test-code&state=test-state",
            None,
        )
        .expect("parse");
        assert_eq!(code, "test-code");
        assert_eq!(state.as_deref(), Some("test-state"));
    }

    #[test]
    fn accepts_query_fragment() {
        let (code, state) =
            parse_submitted_code("?code=test-code&state=test-state", None).expect("parse");
        assert_eq!(code, "test-code");
        assert_eq!(state.as_deref(), Some("test-state"));
    }

    #[test]
    fn accepts_bare_code_with_fallback_state() {
        let (code, state) = parse_submitted_code("test-code", Some("saved-state")).expect("parse");
        assert_eq!(code, "test-code");
        assert_eq!(state.as_deref(), Some("saved-state"));
    }
}
