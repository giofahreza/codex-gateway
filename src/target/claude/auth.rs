use base64::Engine;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

use super::accounts::ClaudeModelInfo;
use crate::target::oauth::{provider_config, OAuthProvider};

pub const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_AI_BASE_URL: &str = "https://claude.ai";
const DEFAULT_AUTHORIZE_URL: &str = "https://claude.ai/v1/oauth/{organization_uuid}/authorize";
const DEFAULT_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const DEFAULT_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const DEFAULT_SCOPE: &str = "user:profile user:inference";
const TOKEN_USER_AGENT: &str = "claude-cli/2.1.81 (external, cli)";

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
    serde_json::json!({
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
    let authorize_url = authorize_url_template().replace("{organization_uuid}", organization_uuid);
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
    let mut params = vec![
        ("code", code.to_string()),
        ("grant_type", "authorization_code".to_string()),
        ("client_id", client_id()),
        ("redirect_uri", redirect_uri()),
        ("code_verifier", verifier.to_string()),
    ];
    if let Some(state) = state.map(str::trim).filter(|value| !value.is_empty()) {
        params.push(("state", state.to_string()));
    }

    token_request(client, params, "token exchange").await
}

pub async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<TokenResponse, String> {
    token_request(
        client,
        vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.to_string()),
            ("client_id", client_id()),
        ],
        "token refresh",
    )
    .await
}

async fn token_request(
    client: &reqwest::Client,
    params: Vec<(&str, String)>,
    label: &str,
) -> Result<TokenResponse, String> {
    let resp = client
        .post(token_url())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", TOKEN_USER_AGENT)
        .form(&params)
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
    if !access_token_needs_refresh(account.expires_at.as_deref()) {
        return Ok(account.access_token.clone());
    }
    let refresh_token = account
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Claude account token expired and no refresh_token is saved".to_string())?;
    let mut token = refresh_access_token(&state.client, refresh_token).await?;
    if token
        .refresh_token
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        token.refresh_token = Some(refresh_token.to_string());
    }
    persist_refreshed_token(state, account, &token)?;
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
        .unwrap_or("claude");
    let label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(email.map(str::trim).filter(|value| !value.is_empty()))
        .unwrap_or(organization_uuid);
    let file_name = format!(
        "claude-{}-{}.json",
        sanitize_label(organization_uuid),
        sanitize_label(label)
    );
    let path = Path::new(&auth_dir).join(&file_name);
    remove_duplicate_auth_files(&auth_dir, &path, organization_uuid, email)?;

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
        "account_id": organization_uuid,
        "label": label,
        "email": email,
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "token_type": token.token_type.as_deref().unwrap_or("Bearer"),
        "expires_at": expires_at,
        "api_base_url": base_url.map(str::trim).filter(|value| !value.is_empty()).unwrap_or(super::DEFAULT_API_BASE_URL),
        "models": models,
        "created_at": now,
        "updated_at": now
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&out).unwrap())
        .map_err(|err| err.to_string())?;
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
    object.insert(
        "updated_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap())
        .map_err(|err| err.to_string())?;
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
            "https://console.anthropic.com/oauth/code/callback?code=abc&state=xyz",
        )
        .unwrap();
        assert_eq!(code, "abc");
        assert_eq!(state, "xyz");

        let (code, state) = parse_oauth_callback("abc#xyz").unwrap();
        assert_eq!(code, "abc");
        assert_eq!(state, "xyz");
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
