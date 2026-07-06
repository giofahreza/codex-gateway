use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{Method, StatusCode},
    response::IntoResponse,
    Form,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct LoginStartRequest {
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    organization_uuid: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginStartQuery {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub organization_uuid: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Deserialize)]
pub struct CallbackForm {
    pub redirect_url: String,
    #[serde(default)]
    pub verifier: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub organization_uuid: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

pub async fn accounts_json(State(state): State<crate::AppState>) -> impl IntoResponse {
    let usage_by_key = {
        let stats = state.stats.lock().unwrap();
        stats
            .claude_accounts
            .iter()
            .map(|usage| (usage.key.clone(), usage.clone()))
            .collect::<std::collections::HashMap<_, _>>()
    };

    let accounts = state
        .claude_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|account| {
            let stats_key = crate::claude_stats_key(account);
            let usage = usage_by_key.get(&stats_key).cloned().unwrap_or_default();
            serde_json::json!({
                "organization_uuid": account.organization_uuid,
                "account_id": account.account_id,
                "label": account.label,
                "email": account.email,
                "file_name": account.file_name,
                "enabled": account.enabled,
                "api_base_url": account.api_base_url,
                "expired_at": account.expires_at,
                "models": account.models,
                "requests": usage.requests,
                "errors": usage.errors,
                "prompt_total": usage.prompt_total,
                "prompt_error_total": usage.prompt_error_total,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "total_tokens": usage.total_tokens,
                "cache_tokens": usage.cache_tokens,
                "reasoning_tokens": usage.reasoning_tokens,
                "first_seen_at": usage.first_seen_at,
                "last_seen_at": usage.last_seen_at,
                "last_success_at": usage.last_success_at,
                "last_error_at": usage.last_error_at
            })
        })
        .collect::<Vec<_>>();

    axum::Json(serde_json::json!({ "accounts": accounts }))
}

pub async fn login_start(
    State(state): State<crate::AppState>,
    method: Method,
    Query(query): Query<LoginStartQuery>,
    body: Bytes,
) -> impl IntoResponse {
    match method {
        Method::GET => start_browser_oauth(&state, query).await,
        Method::POST => save_or_oauth_account(&state, &body).await,
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            "Claude setup supports GET for OAuth start or POST for cookie/token submission",
        )
            .into_response(),
    }
}

async fn start_browser_oauth(
    state: &crate::AppState,
    query: LoginStartQuery,
) -> axum::response::Response {
    let pending = super::auth::PendingOAuth::new();
    let url = match super::auth::build_auth_url(&pending) {
        Ok(url) => url,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to create Claude OAuth URL: {}", err)
            }))
            .into_response()
        }
    };
    let state_token = pending.state_token.clone();
    {
        let mut pending_map = state.claude_oauth_pending.lock().unwrap();
        pending_map.insert(state_token.clone(), pending);
    }
    axum::Json(serde_json::json!({
        "ok": true,
        "url": url,
        "state": state_token,
        "label": query.label,
        "organization_uuid": query.organization_uuid,
        "base_url": query.base_url
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
            "message": "authorization code or callback URL is required"
        }))
        .into_response();
    }

    let (code, parsed_state) = match super::auth::parse_oauth_callback(redirect_url) {
        Ok(value) => value,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };
    let state_value = form
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| (!parsed_state.trim().is_empty()).then_some(parsed_state.trim()));
    let Some(state_value) = state_value else {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "callback URL missing state"
        }))
        .into_response();
    };
    let verifier = if let Some(verifier) = form
        .verifier
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        verifier.to_string()
    } else {
        let mut pending_map = state.claude_oauth_pending.lock().unwrap();
        match pending_map.remove(state_value) {
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

    let token = match super::auth::exchange_code(&state.client, &code, &verifier, Some(state_value))
        .await
    {
        Ok(token) => token,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    save_token_response(
        &state,
        token,
        form.organization_uuid.as_deref(),
        form.label.as_deref(),
        None,
        form.base_url.as_deref(),
    )
    .await
}

async fn save_or_oauth_account(state: &crate::AppState, body: &Bytes) -> axum::response::Response {
    let payload: LoginStartRequest = match serde_json::from_slice(body) {
        Ok(payload) => payload,
        Err(_) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": "Submit Claude credentials as JSON: {\"cookie\":\"...\"} or {\"access_token\":\"...\",\"refresh_token\":\"...\"}"
            }))
            .into_response();
        }
    };

    if let Some(access_token) = payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let token = super::auth::TokenResponse {
            access_token: access_token.to_string(),
            refresh_token: payload
                .refresh_token
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string()),
            token_type: Some("Bearer".to_string()),
            expires_in: payload.expires_in,
        };
        return save_token_response(
            state,
            token,
            payload.organization_uuid.as_deref(),
            payload.label.as_deref(),
            payload.email.as_deref(),
            payload.base_url.as_deref(),
        )
        .await;
    }

    let Some(cookie) = payload
        .cookie
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "Claude cookie or OAuth access_token is required"
        }))
        .into_response();
    };

    let organizations = match super::auth::fetch_organizations(&state.client, cookie).await {
        Ok(organizations) => organizations,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };
    let selected = select_organization(&organizations, payload.organization_uuid.as_deref());
    let Some(organization) = selected else {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": "multiple Claude organizations found; provide organization_uuid",
            "organizations": organizations
        }))
        .into_response();
    };

    let pending = super::auth::PendingOAuth::new();
    let code = match super::auth::authorize_with_cookie(
        &state.client,
        cookie,
        &organization.uuid,
        &pending,
    )
    .await
    {
        Ok(code) => code,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };
    let token = match super::auth::exchange_code(
        &state.client,
        &code,
        &pending.code_verifier,
        Some(&pending.state_token),
    )
    .await
    {
        Ok(token) => token,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "ok": false,
                "message": err
            }))
            .into_response();
        }
    };

    save_token_response(
        state,
        token,
        Some(&organization.uuid),
        payload.label.as_deref().or(organization.name.as_deref()),
        organization.email.as_deref().or(payload.email.as_deref()),
        payload.base_url.as_deref(),
    )
    .await
}

async fn save_token_response(
    state: &crate::AppState,
    token: super::auth::TokenResponse,
    organization_uuid: Option<&str>,
    label: Option<&str>,
    email: Option<&str>,
    base_url: Option<&str>,
) -> axum::response::Response {
    let api_base = super::auth::api_base_url(base_url);
    let models = super::auth::fetch_models(&state.client, &token.access_token, &api_base)
        .await
        .unwrap_or_default();
    match super::auth::save_auth(
        &state.cfg,
        organization_uuid,
        label,
        email,
        &token,
        &models,
        Some(&api_base),
    ) {
        Ok(saved_path) => {
            super::accounts::reload_state(state);
            axum::Json(serde_json::json!({
                "ok": true,
                "message": format!("saved Claude credentials to {}", saved_path),
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

fn select_organization(
    organizations: &[super::auth::ClaudeOrganization],
    requested: Option<&str>,
) -> Option<super::auth::ClaudeOrganization> {
    if let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        return organizations
            .iter()
            .find(|org| org.uuid == requested)
            .cloned();
    }
    let chat_orgs = organizations
        .iter()
        .filter(|org| org.capabilities.iter().any(|cap| cap == "chat"))
        .cloned()
        .collect::<Vec<_>>();
    if chat_orgs.len() == 1 {
        return chat_orgs.into_iter().next();
    }
    if organizations.len() == 1 {
        return organizations.first().cloned();
    }
    None
}
