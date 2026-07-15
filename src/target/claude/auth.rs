use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

use super::accounts::ClaudeModelInfo;
use crate::target::oauth::{provider_config, OAuthProvider};

pub const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_AI_BASE_URL: &str = "https://claude.ai";
const DEFAULT_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const DEFAULT_COOKIE_AUTHORIZE_URL: &str =
    "https://claude.ai/v1/oauth/{organization_uuid}/authorize";
const DEFAULT_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const DEFAULT_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const DEFAULT_SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const PROFILE_PATH: &str = "/api/oauth/profile";
const ROLES_PATH: &str = "/api/oauth/claude_cli/roles";

#[derive(Clone, Debug)]
pub struct PendingOAuth {
    pub code_verifier: String,
    pub code_challenge: String,
    pub state_token: String,
    #[allow(dead_code)]
    pub created_at: std::time::Instant,
}

impl PendingOAuth {
    pub fn new() -> Self {
        let code_verifier = random_urlsafe(32);
        let hash = Sha256::digest(code_verifier.as_bytes());
        let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
        let state_token = random_urlsafe(32);
        Self {
            code_verifier,
            code_challenge,
            state_token,
            created_at: std::time::Instant::now(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub account: Option<ClaudeOAuthTokenAccount>,
    #[serde(default)]
    pub organization: Option<ClaudeOAuthTokenOrganization>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClaudeOrganization {
    #[serde(default, alias = "uuid", alias = "id")]
    pub uuid: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClaudeOAuthTokenAccount {
    #[serde(default, alias = "id")]
    pub uuid: String,
    #[serde(default, alias = "email")]
    pub email_address: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClaudeOAuthTokenOrganization {
    #[serde(default, alias = "id")]
    pub uuid: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClaudeProfileAccount {
    #[serde(default, alias = "id")]
    pub uuid: String,
    #[serde(default, alias = "emailAddress", alias = "email_address")]
    pub email: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClaudeProfileOrganization {
    #[serde(default, alias = "id")]
    pub uuid: String,
    #[serde(default)]
    pub organization_type: Option<String>,
    #[serde(default)]
    pub rate_limit_tier: Option<String>,
    #[serde(default)]
    pub has_extra_usage_enabled: Option<bool>,
    #[serde(default)]
    pub billing_type: Option<String>,
    #[serde(default)]
    pub subscription_created_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClaudeOAuthProfile {
    #[serde(default)]
    pub account: ClaudeProfileAccount,
    #[serde(default)]
    pub organization: ClaudeProfileOrganization,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClaudeOAuthRoles {
    #[serde(default)]
    pub organization_uuid: Option<String>,
    #[serde(default)]
    pub organization_name: Option<String>,
    #[serde(default)]
    pub organization_role: Option<String>,
    #[serde(default)]
    pub workspace_uuid: Option<String>,
    #[serde(default)]
    pub workspace_name: Option<String>,
    #[serde(default)]
    pub workspace_role: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ClaudeOAuthIdentity {
    pub account_uuid: Option<String>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub organization_uuid: Option<String>,
    pub organization_name: Option<String>,
    pub organization_role: Option<String>,
    pub workspace_uuid: Option<String>,
    pub workspace_name: Option<String>,
    pub workspace_role: Option<String>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
    pub has_extra_usage_enabled: Option<bool>,
    pub billing_type: Option<String>,
    pub account_created_at: Option<String>,
    pub subscription_created_at: Option<String>,
    pub scopes: Vec<String>,
}

pub fn client_id() -> String {
    provider_config(None, OAuthProvider::Claude)
        .client_id
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
}

pub fn token_url() -> String {
    provider_config(None, OAuthProvider::Claude)
        .token_url
        .unwrap_or_else(|| DEFAULT_TOKEN_URL.to_string())
}

pub fn redirect_uri() -> String {
    provider_config(None, OAuthProvider::Claude)
        .redirect_uri
        .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string())
}

pub fn api_base_url(account_base: Option<&str>) -> String {
    account_base
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .or_else(|| {
            provider_config(None, OAuthProvider::Claude)
                .base_url
                .map(|value| value.trim_end_matches('/').to_string())
        })
        .unwrap_or_else(|| super::DEFAULT_API_BASE_URL.to_string())
}

pub fn authorize_url_template() -> String {
    provider_config(None, OAuthProvider::Claude)
        .authorize_url
        .unwrap_or_else(|| DEFAULT_AUTHORIZE_URL.to_string())
}

fn cookie_authorize_url_template() -> String {
    let configured = provider_config(None, OAuthProvider::Claude).authorize_url;
    configured
        .filter(|value| value.contains("{organization_uuid}"))
        .unwrap_or_else(|| DEFAULT_COOKIE_AUTHORIZE_URL.to_string())
}

pub fn scopes() -> Vec<String> {
    let scopes = provider_config(None, OAuthProvider::Claude).scopes;
    if scopes.is_empty() {
        DEFAULT_SCOPE
            .split_whitespace()
            .map(|value| value.to_string())
            .collect()
    } else {
        scopes
    }
}

pub fn build_manual_authorize_payload(
    organization_uuid: &str,
    pending: &PendingOAuth,
) -> serde_json::Value {
    json!({
        "response_type": "code",
        "client_id": client_id(),
        "organization_uuid": organization_uuid,
        "redirect_uri": redirect_uri(),
        "scope": scopes().join(" "),
        "state": pending.state_token,
        "code_challenge": pending.code_challenge,
        "code_challenge_method": "S256"
    })
}

pub fn build_auth_url(
    pending: &PendingOAuth,
    organization_uuid: Option<&str>,
    login_hint: Option<&str>,
    login_method: Option<&str>,
) -> Result<String, String> {
    let mut url = url::Url::parse(&authorize_url_template()).map_err(|err| err.to_string())?;
    let mut pairs = url.query_pairs_mut();
    pairs
        .append_pair("code", "true")
        .append_pair("client_id", &client_id())
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri())
        .append_pair("scope", &scopes().join(" "))
        .append_pair("state", &pending.state_token)
        .append_pair("code_challenge", &pending.code_challenge)
        .append_pair("code_challenge_method", "S256");
    if let Some(organization_uuid) = organization_uuid
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        pairs.append_pair("orgUUID", organization_uuid);
    }
    if let Some(login_hint) = login_hint.map(str::trim).filter(|value| !value.is_empty()) {
        pairs.append_pair("login_hint", login_hint);
    }
    if let Some(login_method) = login_method
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        pairs.append_pair("login_method", login_method);
    }
    drop(pairs);
    Ok(url.to_string())
}

pub async fn fetch_organizations(
    client: &reqwest::Client,
    cookie: &str,
) -> Result<Vec<ClaudeOrganization>, String> {
    let resp = client
        .get(format!("{}/api/organizations", CLAUDE_AI_BASE_URL))
        .headers(claude_ai_headers(cookie))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Claude organizations request failed: {}", err))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Claude organizations returned {}: {}",
            status, text
        ));
    }

    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("Claude organizations JSON parse failed: {}", err))?;
    let organizations = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| serde_json::from_value::<ClaudeOrganization>(item.clone()).ok())
        .filter(|org| !org.uuid.trim().is_empty())
        .collect::<Vec<_>>();
    if organizations.is_empty() {
        return Err("Claude account did not return any organizations".to_string());
    }
    Ok(organizations)
}

pub async fn authorize_with_cookie(
    client: &reqwest::Client,
    cookie: &str,
    organization_uuid: &str,
    pending: &PendingOAuth,
) -> Result<String, String> {
    let authorize_url =
        cookie_authorize_url_template().replace("{organization_uuid}", organization_uuid);
    let resp = client
        .post(authorize_url)
        .headers(claude_ai_headers(cookie))
        .header("Content-Type", "application/json")
        .json(&build_manual_authorize_payload(organization_uuid, pending))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Claude OAuth authorize request failed: {}", err))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Claude OAuth authorize returned {}: {}",
            status, text
        ));
    }

    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("Claude OAuth authorize JSON parse failed: {}", err))?;
    let redirect = value
        .get("redirect_uri")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Claude OAuth authorize response missing redirect_uri".to_string())?;
    let (code, state) = parse_oauth_callback(redirect)?;
    if state != pending.state_token {
        return Err("Claude OAuth state mismatch".to_string());
    }
    Ok(code)
}

pub fn parse_oauth_callback(input: &str) -> Result<(String, String), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("authorization code or callback URL is required".to_string());
    }

    if let Ok(url) = url::Url::parse(trimmed) {
        let code = url
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        let error = url
            .query_pairs()
            .find(|(key, _)| key == "error")
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        if !error.is_empty() {
            return Err(format!("oauth error: {}", error));
        }
        if code.is_empty() {
            return Err("callback URL missing code".to_string());
        }
        return Ok((code, state));
    }

    if let Some((code, state)) = trimmed.split_once('#') {
        return Ok((code.trim().to_string(), state.trim().to_string()));
    }

    Ok((trimmed.to_string(), String::new()))
}

pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
    state: Option<&str>,
) -> Result<TokenResponse, String> {
    let mut payload = json!({
        "code": code,
        "grant_type": "authorization_code",
        "client_id": client_id(),
        "redirect_uri": redirect_uri(),
        "code_verifier": verifier
    });
    if let Some(state) = state.map(str::trim).filter(|value| !value.is_empty()) {
        payload["state"] = json!(state);
    }
    token_request(client, payload, "token exchange").await
}

pub async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
    scopes_override: Option<&[String]>,
) -> Result<TokenResponse, String> {
    let scopes = scopes_override
        .filter(|value| !value.is_empty())
        .map(|value| value.join(" "))
        .unwrap_or_else(|| scopes().join(" "));
    token_request(
        client,
        json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": client_id(),
            "scope": scopes
        }),
        "token refresh",
    )
    .await
}

async fn token_request(
    client: &reqwest::Client,
    payload: serde_json::Value,
    label: &str,
) -> Result<TokenResponse, String> {
    let resp = client
        .post(token_url())
        .header("Content-Type", "application/json")
        .header("User-Agent", super::CLAUDE_CODE_CLI_USER_AGENT)
        .header("x-app", "cli")
        .json(&payload)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Claude OAuth {} failed: {}", label, err))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Claude OAuth {} returned {}: {}",
            label, status, text
        ));
    }
    let token: TokenResponse = serde_json::from_str(&text)
        .map_err(|err| format!("Claude OAuth token JSON parse failed: {}", err))?;
    if token.access_token.trim().is_empty() {
        return Err("Claude OAuth token response missing access_token".to_string());
    }
    Ok(token)
}

pub async fn ensure_access_token(
    state: &crate::AppState,
    account: &super::accounts::ClaudeAccount,
) -> Result<String, String> {
    let account_key = crate::claude_stats_key(account);
    let refresh_lock = crate::account_refresh_lock(state, "claude", &account_key);
    let _guard = refresh_lock.lock().await;
    let current = state
        .claude_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|candidate| candidate.file_name == account.file_name)
        .cloned()
        .unwrap_or_else(|| account.clone());
    if !access_token_needs_refresh(current.expires_at.as_deref()) {
        return Ok(current.access_token.clone());
    }
    let refresh_token = current
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Claude account token expired and no refresh_token is saved".to_string())?;
    let mut token =
        refresh_access_token(&state.client, refresh_token, Some(&current.scopes)).await?;
    if token
        .refresh_token
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        token.refresh_token = Some(refresh_token.to_string());
    }
    persist_refreshed_token(state, &current, &token)?;
    Ok(token.access_token)
}

fn access_token_needs_refresh(expires_at: Option<&str>) -> bool {
    let Some(expires_at) = expires_at else {
        return false;
    };
    let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return false;
    };
    expires_at.timestamp() - chrono::Utc::now().timestamp() <= 120
}

pub async fn fetch_models(
    client: &reqwest::Client,
    access_token: &str,
    base_url: &str,
) -> Result<Vec<ClaudeModelInfo>, String> {
    let resp = client
        .get(format!("{}/v1/models", base_url.trim_end_matches('/')))
        .headers(anthropic_headers(access_token, None))
        .header("User-Agent", super::CLAUDE_CODE_CLI_USER_AGENT)
        .header("x-app", "cli")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Claude models request failed: {}", err))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Claude models returned {}: {}", status, text));
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("Claude models JSON parse failed: {}", err))?;
    let models = value
        .get("data")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("id").and_then(|value| value.as_str())?.trim();
            if id.is_empty() {
                return None;
            }
            Some(ClaudeModelInfo {
                id: id.to_string(),
                display_name: item
                    .get("display_name")
                    .or_else(|| item.get("name"))
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
                created_at: item
                    .get("created_at")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
                model_type: item
                    .get("type")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
            })
        })
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("Claude models response did not include any models".to_string());
    }
    Ok(models)
}

pub async fn fetch_profile_info(
    client: &reqwest::Client,
    access_token: &str,
    base_url: &str,
) -> Result<ClaudeOAuthProfile, String> {
    let resp = client
        .get(format!(
            "{}{}",
            base_url.trim_end_matches('/'),
            PROFILE_PATH
        ))
        .header("Authorization", format!("Bearer {}", access_token.trim()))
        .header("Content-Type", "application/json")
        .header("User-Agent", super::CLAUDE_CODE_CLI_USER_AGENT)
        .header("x-app", "cli")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Claude profile request failed: {}", err))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Claude profile returned {}: {}", status, text));
    }
    serde_json::from_str(&text).map_err(|err| format!("Claude profile JSON parse failed: {}", err))
}

pub async fn fetch_user_roles(
    client: &reqwest::Client,
    access_token: &str,
    base_url: &str,
) -> Result<ClaudeOAuthRoles, String> {
    let resp = client
        .get(format!("{}{}", base_url.trim_end_matches('/'), ROLES_PATH))
        .header("Authorization", format!("Bearer {}", access_token.trim()))
        .header("User-Agent", super::CLAUDE_CODE_CLI_USER_AGENT)
        .header("x-app", "cli")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Claude roles request failed: {}", err))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Claude roles returned {}: {}", status, text));
    }
    serde_json::from_str(&text).map_err(|err| format!("Claude roles JSON parse failed: {}", err))
}

pub fn parse_scope_string(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split_whitespace()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn build_oauth_identity(
    token: &TokenResponse,
    profile: Option<&ClaudeOAuthProfile>,
    roles: Option<&ClaudeOAuthRoles>,
) -> ClaudeOAuthIdentity {
    let mut identity = ClaudeOAuthIdentity {
        account_uuid: token
            .account
            .as_ref()
            .map(|account| account.uuid.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        email: token
            .account
            .as_ref()
            .and_then(|account| account.email_address.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        organization_uuid: token
            .organization
            .as_ref()
            .map(|organization| organization.uuid.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        scopes: parse_scope_string(token.scope.as_deref()),
        ..Default::default()
    };

    if let Some(profile) = profile {
        if !profile.account.uuid.trim().is_empty() {
            identity.account_uuid = Some(profile.account.uuid.trim().to_string());
        }
        if let Some(email) = profile
            .account
            .email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            identity.email = Some(email.to_string());
        }
        if let Some(display_name) = profile
            .account
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            identity.display_name = Some(display_name.to_string());
        }
        if !profile.organization.uuid.trim().is_empty() {
            identity.organization_uuid = Some(profile.organization.uuid.trim().to_string());
        }
        identity.subscription_type = subscription_type_from_profile(profile);
        identity.rate_limit_tier = profile.organization.rate_limit_tier.clone();
        identity.has_extra_usage_enabled = profile.organization.has_extra_usage_enabled;
        identity.billing_type = profile.organization.billing_type.clone();
        identity.account_created_at = profile.account.created_at.clone();
        identity.subscription_created_at = profile.organization.subscription_created_at.clone();
    }

    if let Some(roles) = roles {
        identity.organization_name = roles.organization_name.clone();
        identity.organization_role = roles.organization_role.clone();
        identity.workspace_uuid = roles.workspace_uuid.clone();
        identity.workspace_name = roles.workspace_name.clone();
        identity.workspace_role = roles.workspace_role.clone();
        if let Some(organization_uuid) = roles
            .organization_uuid
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            identity.organization_uuid = Some(organization_uuid.to_string());
        }
    }

    identity
}

fn subscription_type_from_profile(profile: &ClaudeOAuthProfile) -> Option<String> {
    match profile.organization.organization_type.as_deref() {
        Some("claude_max") => Some("max".to_string()),
        Some("claude_pro") => Some("pro".to_string()),
        Some("claude_enterprise") => Some("enterprise".to_string()),
        Some("claude_team") => Some("team".to_string()),
        _ => None,
    }
}

pub fn anthropic_headers(
    access_token: &str,
    incoming_beta: Option<&str>,
) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    insert_header(
        &mut headers,
        "Authorization",
        &format!("Bearer {}", access_token.trim()),
    );
    insert_header(&mut headers, "Content-Type", "application/json");
    insert_header(&mut headers, "Accept", "application/json");
    insert_header(&mut headers, "anthropic-version", "2023-06-01");
    insert_header(
        &mut headers,
        "anthropic-beta",
        &merged_beta_header(incoming_beta),
    );
    headers
}

pub fn merged_beta_header(incoming_beta: Option<&str>) -> String {
    let mut betas = vec!["oauth-2025-04-20".to_string()];
    if let Some(incoming) = incoming_beta {
        for beta in incoming.split(',') {
            let beta = beta.trim();
            if !beta.is_empty() && !betas.iter().any(|existing| existing == beta) {
                betas.push(beta.to_string());
            }
        }
    }
    betas.join(",")
}

pub fn save_auth(
    cfg: &crate::Config,
    organization_uuid: Option<&str>,
    label: Option<&str>,
    email: Option<&str>,
    token: &TokenResponse,
    identity: Option<&ClaudeOAuthIdentity>,
    models: &[ClaudeModelInfo],
    base_url: Option<&str>,
) -> Result<String, String> {
    let auth_dir = cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    std::fs::create_dir_all(&auth_dir).map_err(|err| err.to_string())?;

    let organization_uuid = organization_uuid
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            identity
                .and_then(|identity| identity.organization_uuid.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("claude");
    let label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            identity
                .and_then(|identity| identity.display_name.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .or(email.map(str::trim).filter(|value| !value.is_empty()))
        .or_else(|| {
            identity
                .and_then(|identity| identity.email.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(organization_uuid);
    let saved_email = email
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            identity
                .and_then(|identity| identity.email.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
    let account_id = identity
        .and_then(|identity| identity.account_uuid.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(organization_uuid);
    let file_name = format!(
        "claude-{}-{}.json",
        sanitize_label(organization_uuid),
        sanitize_label(label)
    );
    let path = Path::new(&auth_dir).join(&file_name);
    remove_duplicate_auth_files(&auth_dir, &path, organization_uuid, saved_email.as_deref())?;

    let expires_at = token
        .expires_in
        .filter(|value| *value > 0)
        .and_then(|expires_in| {
            chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + expires_in, 0)
        })
        .map(|value| value.to_rfc3339());
    let now = chrono::Utc::now().to_rfc3339();
    let out = serde_json::json!({
        "type": super::PROVIDER_NAME,
        "organization_uuid": organization_uuid,
        "account_id": account_id,
        "label": label,
        "email": saved_email,
        "oauth_account_uuid": identity.and_then(|identity| identity.account_uuid.as_deref()),
        "display_name": identity.and_then(|identity| identity.display_name.as_deref()),
        "organization_name": identity.and_then(|identity| identity.organization_name.as_deref()),
        "organization_role": identity.and_then(|identity| identity.organization_role.as_deref()),
        "workspace_uuid": identity.and_then(|identity| identity.workspace_uuid.as_deref()),
        "workspace_name": identity.and_then(|identity| identity.workspace_name.as_deref()),
        "workspace_role": identity.and_then(|identity| identity.workspace_role.as_deref()),
        "subscription_type": identity.and_then(|identity| identity.subscription_type.as_deref()),
        "rate_limit_tier": identity.and_then(|identity| identity.rate_limit_tier.as_deref()),
        "has_extra_usage_enabled": identity.and_then(|identity| identity.has_extra_usage_enabled),
        "billing_type": identity.and_then(|identity| identity.billing_type.as_deref()),
        "account_created_at": identity.and_then(|identity| identity.account_created_at.as_deref()),
        "subscription_created_at": identity.and_then(|identity| identity.subscription_created_at.as_deref()),
        "scopes": if let Some(identity) = identity { identity.scopes.clone() } else { parse_scope_string(token.scope.as_deref()) },
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "token_type": token.token_type.as_deref().unwrap_or("Bearer"),
        "expires_at": expires_at,
        "api_base_url": base_url.map(str::trim).filter(|value| !value.is_empty()).unwrap_or(super::DEFAULT_API_BASE_URL),
        "models": models,
        "created_at": now,
        "updated_at": now
    });
    super::super::atomic_write_json(&path, &out)?;
    Ok(path.to_string_lossy().to_string())
}

fn persist_refreshed_token(
    state: &crate::AppState,
    account: &super::accounts::ClaudeAccount,
    token: &TokenResponse,
) -> Result<(), String> {
    let Some(file_name) = account.file_name.as_ref() else {
        return Ok(());
    };
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let path = Path::new(&auth_dir).join(file_name);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).map_err(|err| err.to_string())?)
            .map_err(|err| err.to_string())?;
    let Some(object) = value.as_object_mut() else {
        return Err("Claude auth file is not a JSON object".to_string());
    };
    object.insert(
        "access_token".to_string(),
        serde_json::Value::String(token.access_token.clone()),
    );
    if let Some(refresh_token) = token
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert(
            "refresh_token".to_string(),
            serde_json::Value::String(refresh_token.to_string()),
        );
    }
    if let Some(expires_at) = token
        .expires_in
        .filter(|value| *value > 0)
        .and_then(|expires_in| {
            chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + expires_in, 0)
        })
        .map(|value| value.to_rfc3339())
    {
        object.insert(
            "expires_at".to_string(),
            serde_json::Value::String(expires_at),
        );
    }
    let scopes = parse_scope_string(token.scope.as_deref());
    if !scopes.is_empty() {
        object.insert("scopes".to_string(), serde_json::json!(scopes));
    }
    object.insert(
        "updated_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    super::super::atomic_write_json(&path, &value)?;
    super::accounts::reload_state(state);
    Ok(())
}

fn remove_duplicate_auth_files(
    auth_dir: &str,
    keep_path: &Path,
    organization_uuid: &str,
    email: Option<&str>,
) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(auth_dir) else {
        return Ok(());
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path == keep_path || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) != Some(super::PROVIDER_NAME) {
            continue;
        }
        let same_org = value
            .get("organization_uuid")
            .or_else(|| value.get("account_id"))
            .and_then(|value| value.as_str())
            .map(|value| value == organization_uuid)
            .unwrap_or(false);
        let same_email = email
            .map(|email| {
                value
                    .get("email")
                    .and_then(|value| value.as_str())
                    .map(|value| value.eq_ignore_ascii_case(email))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if same_org || same_email {
            std::fs::remove_file(path).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn claude_ai_headers(cookie: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    insert_header(&mut headers, "Accept", "application/json");
    insert_header(&mut headers, "Accept-Language", "en-US,en;q=0.9");
    insert_header(&mut headers, "Cache-Control", "no-cache");
    insert_header(&mut headers, "Cookie", cookie);
    insert_header(&mut headers, "Origin", CLAUDE_AI_BASE_URL);
    insert_header(
        &mut headers,
        "Referer",
        &format!("{}/new", CLAUDE_AI_BASE_URL),
    );
    insert_header(
        &mut headers,
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
    );
    headers
}

fn insert_header(headers: &mut reqwest::header::HeaderMap, key: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        reqwest::header::HeaderName::from_bytes(key.as_bytes()),
        reqwest::header::HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

fn random_urlsafe(byte_len: usize) -> String {
    let bytes = rand::rng()
        .sample_iter(rand::distr::StandardUniform)
        .take(byte_len)
        .collect::<Vec<u8>>();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '@' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_callback_accepts_url_and_code_hash_state() {
        let (code, state) = parse_oauth_callback(
            "https://platform.claude.com/oauth/code/callback?code=abc&state=xyz",
        )
        .unwrap();
        assert_eq!(code, "abc");
        assert_eq!(state, "xyz");

        let (code, state) = parse_oauth_callback("abc#xyz").unwrap();
        assert_eq!(code, "abc");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn browser_auth_url_matches_claude_code_subscription_flow() {
        let pending = PendingOAuth::new();
        let url = build_auth_url(&pending, Some("org_123"), None, None).unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let params = parsed
            .query_pairs()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(
            parsed.as_str().split('?').next().unwrap(),
            DEFAULT_AUTHORIZE_URL
        );
        assert_eq!(params.get("code").map(String::as_str), Some("true"));
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some(DEFAULT_CLIENT_ID)
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some(DEFAULT_REDIRECT_URI)
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        let scope = params.get("scope").cloned().unwrap_or_default();
        assert!(scope.contains("user:inference"));
        assert!(scope.contains("user:sessions:claude_code"));
        assert!(scope.contains("user:mcp_servers"));
        assert_eq!(
            params.get("state").map(String::as_str),
            Some(pending.state_token.as_str())
        );
        assert_eq!(params.get("orgUUID").map(String::as_str), Some("org_123"));
    }

    #[test]
    fn beta_header_keeps_oauth_first_and_merges_client_values() {
        assert_eq!(
            merged_beta_header(Some(
                "fine-grained-tool-streaming-2025-05-14,oauth-2025-04-20"
            )),
            "oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14"
        );
    }
}
