use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use rand::{distr::Alphanumeric, Rng};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;
use url::Url;

use super::super::oauth::{provider_config, OAuthProvider};
use super::accounts::AntigravityAccount;

const GOOGLE_USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const USER_AGENT_VERSION: &str = "antigravity/1.11.5";

pub const ANTIGRAVITY_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];

#[derive(Clone)]
pub struct PendingOAuth {
    pub code_verifier: String,
    pub created_at: std::time::Instant,
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
}

pub fn build_auth_url() -> Result<(String, String, String), String> {
    let provider = provider_config(None, OAuthProvider::Antigravity);
    let google_client_id =
        required_config(&provider.client_id, "oauth.providers.antigravity.client_id")?;
    let redirect_uri = required_config(
        &provider.redirect_uri,
        "oauth.providers.antigravity.redirect_uri",
    )?;
    let authorize_url = required_config(
        &provider.authorize_url,
        "oauth.providers.antigravity.authorize_url",
    )?;
    let code_verifier: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let digest = hasher.finalize();
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    let state_token: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let mut url = Url::parse(&authorize_url).map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("client_id", &google_client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &provider.scopes.join(" "))
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state_token);

    Ok((url.to_string(), state_token, code_verifier))
}

pub fn parse_oauth_callback(redirect_url: &str) -> Result<(String, String), String> {
    let url = Url::parse(redirect_url).map_err(|_| "invalid redirect_url".to_string())?;
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
    if code.is_empty() || state.is_empty() {
        return Err("missing code or state in redirect_url".to_string());
    }
    Ok((code, state))
}

pub async fn exchange_code_for_tokens(
    client: &reqwest::Client,
    code: &str,
    code_verifier: &str,
) -> Result<TokenResponse, String> {
    let provider = provider_config(None, OAuthProvider::Antigravity);
    let google_client_id =
        required_config(&provider.client_id, "oauth.providers.antigravity.client_id")?;
    let google_client_secret = required_config(
        &provider.client_secret,
        "oauth.providers.antigravity.client_secret",
    )?;
    let redirect_uri = required_config(
        &provider.redirect_uri,
        "oauth.providers.antigravity.redirect_uri",
    )?;
    let token_url = required_config(&provider.token_url, "oauth.providers.antigravity.token_url")?;
    let params = [
        ("client_id", google_client_id.as_str()),
        ("client_secret", google_client_secret.as_str()),
        ("code", code),
        ("code_verifier", code_verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri.as_str()),
    ];

    let resp = client
        .post(token_url)
        .form(&params)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token exchange failed: {}", body));
    }

    resp.json::<TokenResponse>()
        .await
        .map_err(|e| e.to_string())
}

pub async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<TokenResponse, String> {
    let provider = provider_config(None, OAuthProvider::Antigravity);
    let google_client_id =
        required_config(&provider.client_id, "oauth.providers.antigravity.client_id")?;
    let google_client_secret = required_config(
        &provider.client_secret,
        "oauth.providers.antigravity.client_secret",
    )?;
    let token_url = required_config(&provider.token_url, "oauth.providers.antigravity.token_url")?;
    let params = [
        ("client_id", google_client_id.as_str()),
        ("client_secret", google_client_secret.as_str()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let resp = client
        .post(token_url)
        .form(&params)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token refresh failed: {}", body));
    }

    resp.json::<TokenResponse>()
        .await
        .map_err(|e| e.to_string())
}

pub async fn get_user_email(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<String, String> {
    let resp = client
        .get(GOOGLE_USER_INFO_URL)
        .bearer_auth(access_token)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("failed to get user info: {}", body));
    }
    let value: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    value
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "user info missing email".to_string())
}

pub async fn discover_project_id(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<Option<String>, String> {
    for endpoint in ANTIGRAVITY_ENDPOINTS {
        let resp = client
            .post(format!("{}/v1internal:loadCodeAssist", endpoint))
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("User-Agent", antigravity_user_agent())
            .header(
                "X-Goog-Api-Client",
                "google-cloud-sdk vscode_cloudshelleditor/0.1",
            )
            .header(
                "Client-Metadata",
                r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
            )
            .body(
                serde_json::json!({
                    "metadata": {
                        "ideType": "IDE_UNSPECIFIED",
                        "platform": "PLATFORM_UNSPECIFIED",
                        "pluginType": "GEMINI"
                    }
                })
                .to_string(),
            )
            .timeout(Duration::from_secs(30))
            .send()
            .await;

        let Ok(resp) = resp else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let value: serde_json::Value = match resp.json().await {
            Ok(value) => value,
            Err(_) => continue,
        };
        if let Some(project) = value
            .get("cloudaicompanionProject")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        {
            return Ok(Some(project));
        }
        if let Some(project) = value
            .get("cloudaicompanionProject")
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        {
            return Ok(Some(project));
        }
    }

    Ok(None)
}

pub fn save_auth(
    state: &crate::AppState,
    email: &str,
    token_resp: &TokenResponse,
    project_id: Option<String>,
) -> Result<String, String> {
    let refresh_token = token_resp
        .refresh_token
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    if refresh_token.is_empty() {
        return Err("google oauth did not return a refresh token; retry consent flow".to_string());
    }

    let expires_at = Utc::now() + ChronoDuration::seconds(token_resp.expires_in.max(0));
    let file_name = format!("antigravity-{}.json", sanitize_label(email));
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    std::fs::create_dir_all(&auth_dir).map_err(|e| e.to_string())?;
    let out = serde_json::json!({
        "type": "antigravity",
        "email": email,
        "label": email,
        "refresh_token": refresh_token,
        "access_token": token_resp.access_token,
        "access_token_expires_at": expires_at.to_rfc3339(),
        "project_id": project_id,
        "last_refresh": Utc::now().to_rfc3339()
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&out).unwrap()).map_err(|e| e.to_string())?;
    super::accounts::reload_state(state);
    Ok(path.to_string_lossy().to_string())
}

pub async fn ensure_access_token(
    state: &crate::AppState,
    account: &AntigravityAccount,
) -> Result<String, String> {
    if let Some(access_token) = account.access_token.as_ref() {
        let still_valid = account
            .access_token_expires_at
            .as_deref()
            .and_then(parse_rfc3339)
            .map(|expires_at| expires_at > Utc::now() + ChronoDuration::seconds(60))
            .unwrap_or(false);
        if still_valid {
            return Ok(access_token.clone());
        }
    }

    let refreshed = refresh_access_token(&state.client, &account.refresh_token).await?;
    persist_refreshed_account(state, account, &refreshed)?;
    Ok(refreshed.access_token)
}

fn persist_refreshed_account(
    state: &crate::AppState,
    account: &AntigravityAccount,
    token_resp: &TokenResponse,
) -> Result<(), String> {
    let Some(file_name) = account.file_name.as_ref() else {
        return Ok(());
    };

    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut value: serde_json::Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    let expires_at =
        (Utc::now() + ChronoDuration::seconds(token_resp.expires_in.max(0))).to_rfc3339();

    if let serde_json::Value::Object(map) = &mut value {
        map.insert(
            "access_token".to_string(),
            serde_json::Value::String(token_resp.access_token.clone()),
        );
        map.insert(
            "access_token_expires_at".to_string(),
            serde_json::Value::String(expires_at.clone()),
        );
        map.insert(
            "last_refresh".to_string(),
            serde_json::Value::String(Utc::now().to_rfc3339()),
        );
        if let Some(refresh_token) = token_resp.refresh_token.as_ref() {
            map.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token.clone()),
            );
        }
    }

    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).map_err(|e| e.to_string())?;

    let mut accounts = state.agw_accounts.lock().unwrap();
    if let Some(current) = accounts
        .iter_mut()
        .find(|current| current.file_name.as_deref() == Some(file_name.as_str()))
    {
        current.access_token = Some(token_resp.access_token.clone());
        current.access_token_expires_at = Some(expires_at);
        if let Some(refresh_token) = token_resp.refresh_token.as_ref() {
            current.refresh_token = refresh_token.clone();
        }
    }

    Ok(())
}

pub fn antigravity_user_agent() -> String {
    format!(
        "{} {}/{}",
        USER_AGENT_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn sanitize_label(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn required_config(value: &Option<String>, field_name: &str) -> Result<String, String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .ok_or_else(|| format!("{} is required for Antigravity OAuth", field_name))
}
