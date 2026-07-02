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

/// Probes `api.x.ai` (text/image/video endpoints) with the first enabled
/// account's bearer token and returns the live `x-ratelimit-*` headers as
/// a single JSON snapshot. Mirrors the data model of the
/// `JoshuaWang2211/grok-usage-watch` userscript (requestKind + modelName +
/// remaining/total), since the grok.com `/rest/rate-limits` endpoint rejects
/// OAuth2 bearers with `oauth2-auth-forbidden`.
pub async fn quota_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let account = match super::accounts::first_enabled(&state) {
        Some(a) => a,
        None => {
            return axum::Json(serde_json::json!({
                "error": { "code": "no_account", "message": "No Grok accounts configured" }
            }));
        }
    };

    let api_base = account
        .api_base_url
        .as_deref()
        .unwrap_or("https://api.x.ai/v1")
        .trim_end_matches('/');
    let auth = format!("{} {}", account.token_type, account.access_token);
    let client = state.client.clone();

    // Helper that posts a tiny payload and returns the parsed `x-ratelimit-*`
    // headers + a per-endpoint `cost_in_usd_ticks` (if any) for the cost panel.
    async fn probe(
        client: &reqwest::Client,
        url: &str,
        auth: &str,
        body: serde_json::Value,
    ) -> (u16, serde_json::Value, serde_json::Value, serde_json::Value) {
        let req = client
            .post(url)
            .header("Authorization", auth)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(20));
        match req.send().await {
            Ok(resp) => {
                let code = resp.status().as_u16();
                let mut rl = serde_json::Map::new();
                let mut reset = serde_json::Map::new();
                for (k, v) in resp.headers().iter() {
                    let kl = k.as_str().to_ascii_lowercase();
                    if kl.starts_with("x-ratelimit-limit-")
                        || kl.starts_with("x-ratelimit-remaining-")
                    {
                        if let Ok(s) = v.to_str() {
                            rl.insert(kl, serde_json::Value::String(s.to_string()));
                        }
                    } else if kl.starts_with("x-ratelimit-reset-") {
                        if let Ok(s) = v.to_str() {
                            reset.insert(kl, serde_json::Value::String(s.to_string()));
                        }
                    }
                }
                let body_bytes = resp.bytes().await.unwrap_or_default();
                let parsed: serde_json::Value =
                    serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
                (
                    code,
                    serde_json::Value::Object(rl),
                    serde_json::Value::Object(reset),
                    parsed,
                )
            }
            Err(err) => (
                0,
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({ "error": err.to_string() }),
            ),
        }
    }

    // Text
    let (tcode, trl, treset, tbody) = probe(
        &client,
        &format!("{}/responses", api_base),
        &auth,
        serde_json::json!({"model": "grok-3", "input": "ok"}),
    )
    .await;
    // Image
    let (icode, irl, _, ibody) = probe(
        &client,
        &format!("{}/images/generations", api_base),
        &auth,
        serde_json::json!({"model": "grok-imagine-image", "prompt": "x", "n": 1}),
    )
    .await;
    // Video
    let (vcode, vrl, _, vbody) = probe(
        &client,
        &format!("{}/videos/generations", api_base),
        &auth,
        serde_json::json!({"model": "grok-imagine-video", "prompt": "x", "duration": 1}),
    )
    .await;

    // Userinfo for plan context
    let me_url = format!("{}/me", api_base);
    let me = match client
        .get(&me_url)
        .header("Authorization", &auth)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    };

    let extract_cost = |body: &serde_json::Value| -> Option<u64> {
        body.get("usage")
            .and_then(|u| u.get("cost_in_usd_ticks"))
            .and_then(|v| v.as_u64())
    };

    let extract_lim_rem = |rl: &serde_json::Value, scope: &str| -> (Option<f64>, Option<f64>) {
        let lim = rl
            .get(format!("x-ratelimit-limit-{}", scope))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        let rem = rl
            .get(format!("x-ratelimit-remaining-{}", scope))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        (lim, rem)
    };

    let (text_req_lim, text_req_rem) = extract_lim_rem(&trl, "requests");
    let (text_tok_lim, text_tok_rem) = extract_lim_rem(&trl, "tokens");
    let (img_req_lim, img_req_rem) = extract_lim_rem(&irl, "requests");
    let (vid_req_lim, vid_req_rem) = extract_lim_rem(&vrl, "requests");

    axum::Json(serde_json::json!({
        "account": {
            "email": account.email,
            "user_id": account.user_id,
            "team_id": account.team_id,
            "zdr_status": account.zdr_status,
            "team_blocked": account.team_blocked,
            "label": account.label,
            "expires_at": account.expires_at,
        },
        "userinfo": me,
        "kinds": {
            "DEFAULT_TEXT": {
                "modelName": "grok-3",
                "requestKind": "DEFAULT",
                "upstream": "POST /v1/responses",
                "status": tcode,
                "rate_limits": {
                    "requests": { "limit": text_req_lim, "remaining": text_req_rem },
                    "tokens":   { "limit": text_tok_lim, "remaining": text_tok_rem },
                },
                "reset": treset,
                "cost_in_usd_ticks": extract_cost(&tbody),
                "note": "Probed with model=grok-3, single token 'ok' to keep cost minimal.",
            },
            "DEFAULT_IMAGE": {
                "modelName": "grok-imagine-image",
                "requestKind": "IMAGE",
                "upstream": "POST /v1/images/generations",
                "status": icode,
                "rate_limits": {
                    "requests": { "limit": img_req_lim, "remaining": img_req_rem },
                },
                "cost_in_usd_ticks": extract_cost(&ibody),
                "note": "1 image generated (cost ~$0.20). The grok-imagine-image-quality model has its own quota.",
            },
            "DEFAULT_VIDEO": {
                "modelName": "grok-imagine-video",
                "requestKind": "VIDEO",
                "upstream": "POST /v1/videos/generations",
                "status": vcode,
                "rate_limits": {
                    "requests": { "limit": vid_req_lim, "remaining": vid_req_rem },
                },
                "request_id": vbody.get("request_id").cloned().unwrap_or(serde_json::Value::Null),
                "cost_in_usd_ticks": extract_cost(&vbody),
                "note": "1-second video clip requested. Long clips (10s) cost more ($5) but use the same quota bucket.",
            },
        },
        "note": "grok.com /rest/rate-limits rejects OAuth2 bearers (oauth2-auth-forbidden); this endpoint uses api.x.ai x-ratelimit-* headers from real probes as the equivalent signal. Reset windows are not exposed by xAI.",
    }))
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
