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
    maybe_backfill_missing_metadata(&state).await;

    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .grok_accounts
            .iter()
            .map(|u| {
                (
                    u.key.clone(),
                    (
                        u.requests,
                        u.errors,
                        u.prompt_total,
                        u.input_tokens,
                        u.output_tokens,
                        u.total_tokens,
                        u.cache_tokens,
                        u.reasoning_tokens,
                        u.last_success_at.clone(),
                        u.last_error_at.clone(),
                    ),
                )
            })
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .grok_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|a| {
            let stats_key = crate::grok_stats_key(a);
            let (
                requests,
                errors,
                prompt_total,
                input_tokens,
                output_tokens,
                total_tokens,
                cache_tokens,
                reasoning_tokens,
                last_success_at,
                last_error_at,
            ) = usage_by_key
                .get(&stats_key)
                .cloned()
                .unwrap_or((0, 0, 0, 0, 0, 0, 0, 0, None, None));
            serde_json::json!({
                "account_id": a.user_id.as_ref().or(a.email.as_ref()).cloned().unwrap_or_else(|| a.label.clone()),
                "label": a.label,
                "name": a.name,
                "email": a.email,
                "email_verified": a.email_verified,
                "user_id": a.user_id,
                "team_id": a.team_id,
                "team_blocked": a.team_blocked,
                "zdr_status": a.zdr_status,
                "file_name": a.file_name,
                "enabled": a.enabled,
                "api_base_url": a.api_base_url,
                "models": a.models,
                "rate_limits": a.rate_limits,
                "last_effective_model": a.last_effective_model,
                "requests": requests,
                "errors": errors,
                "prompt_total": prompt_total,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": total_tokens,
                "cache_tokens": cache_tokens,
                "reasoning_tokens": reasoning_tokens,
                "last_success_at": last_success_at,
                "last_error_at": last_error_at,
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
            let mut profile = token
                .id_token
                .as_deref()
                .and_then(super::auth::profile_from_id_token)
                .unwrap_or_default();
            if let Ok(fetched_profile) =
                super::auth::fetch_profile(&state.client, &token.access_token).await
            {
                super::auth::merge_profile(&mut profile, fetched_profile);
            }
            let models = super::auth::fetch_models(&state.client, &token.access_token)
                .await
                .unwrap_or_default();
            let profile = if grok_profile_is_empty(&profile) {
                None
            } else {
                Some(profile)
            };

            let label = profile
                .as_ref()
                .and_then(|profile| profile.name.as_deref())
                .or_else(|| {
                    profile
                        .as_ref()
                        .and_then(|profile| profile.email.as_deref())
                })
                .unwrap_or("grok");

            match super::auth::save_auth(&state.cfg, &token, profile.as_ref(), &models) {
                Ok(path) => {
                    super::accounts::reload_state(&state);
                    axum::Json(serde_json::json!({
                        "ok": true,
                        "message": format!("Grok account saved: {} ({})", label, path),
                        "email": profile.as_ref().and_then(|profile| profile.email.clone()),
                        "name": profile.as_ref().and_then(|profile| profile.name.clone()),
                        "models": models.len(),
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

async fn maybe_backfill_missing_metadata(state: &crate::AppState) {
    let accounts = state.grok_accounts.lock().unwrap().clone();
    let mut changed = false;
    for account in accounts
        .iter()
        .filter(|account| needs_metadata_backfill(account))
    {
        match super::auth::backfill_account_metadata(state, account).await {
            Ok(account_changed) => changed |= account_changed,
            Err(err) => tracing::warn!(
                "failed to backfill Grok metadata for {}: {}",
                account.label,
                err
            ),
        }
    }
    if changed {
        super::accounts::reload_state(state);
    }
}

fn needs_metadata_backfill(account: &super::accounts::GrokAccount) -> bool {
    account.user_id.is_none() || account.name.is_none() || account.models.is_empty()
}

fn grok_profile_is_empty(profile: &super::auth::GrokProfile) -> bool {
    profile.name.is_none()
        && profile.email.is_none()
        && profile.user_id.is_none()
        && profile.team_id.is_none()
        && profile.team_blocked.is_none()
        && profile.zdr_status.is_none()
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
