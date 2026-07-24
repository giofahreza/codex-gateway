use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

use super::accounts::{CopilotAccount, CopilotModelInfo};

const GITHUB_BASE_URL: &str = "https://github.com";
const GITHUB_API_BASE_URL: &str = "https://api.github.com";
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const GITHUB_APP_SCOPES: &str = "read:user";

const COPILOT_VERSION: &str = "0.43.0";
const VSCODE_VERSION: &str = "1.114.0";
const GITHUB_API_VERSION: &str = "2026-03-10";
const COPILOT_API_VERSION: &str = "2025-04-01";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingDevice {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at_unix: i64,
    pub interval: i64,
    pub account_type: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AccessTokenResponse {
    pub access_token: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GitHubUser {
    pub login: String,
    pub id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CopilotTokenResponse {
    pub token: String,
    pub expires_at: i64,
    pub refresh_in: Option<i64>,
}

pub fn normalize_account_type(value: Option<&str>) -> String {
    match value
        .unwrap_or("individual")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "business" => "business".to_string(),
        "enterprise" => "enterprise".to_string(),
        _ => "individual".to_string(),
    }
}

pub fn copilot_base_url(account_type: &str) -> String {
    match normalize_account_type(Some(account_type)).as_str() {
        "business" => "https://api.business.githubcopilot.com".to_string(),
        "enterprise" => "https://api.enterprise.githubcopilot.com".to_string(),
        _ => "https://api.githubcopilot.com".to_string(),
    }
}

pub async fn start_device_flow(client: &reqwest::Client) -> Result<DeviceCodeResponse, String> {
    let resp = client
        .post(format!("{}/login/device/code", GITHUB_BASE_URL))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "client_id": GITHUB_CLIENT_ID,
            "scope": GITHUB_APP_SCOPES
        }))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("GitHub device-code request failed: {}", err))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| format!("GitHub device-code body read failed: {}", err))?;
    if !status.is_success() {
        return Err(format!("GitHub device-code returned {}: {}", status, text));
    }

    serde_json::from_str::<DeviceCodeResponse>(&text)
        .map_err(|err| format!("GitHub device-code JSON parse failed: {}", err))
}

pub async fn poll_access_token_once(
    client: &reqwest::Client,
    device_code: &str,
) -> Result<String, String> {
    let resp = client
        .post(format!("{}/login/oauth/access_token", GITHUB_BASE_URL))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "client_id": GITHUB_CLIENT_ID,
            "device_code": device_code,
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
        }))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("GitHub token poll failed: {}", err))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| format!("GitHub token poll body read failed: {}", err))?;
    if !status.is_success() {
        return Err(format!("GitHub token poll returned {}: {}", status, text));
    }

    let parsed: AccessTokenResponse = serde_json::from_str(&text)
        .map_err(|err| format!("GitHub token poll JSON parse failed: {}", err))?;
    if let Some(token) = parsed
        .access_token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(token);
    }

    let code = parsed
        .error
        .unwrap_or_else(|| "authorization_pending".to_string());
    let description = parsed
        .error_description
        .unwrap_or_else(|| "Authorize the device code in GitHub, then submit again.".to_string());
    Err(format!("{}: {}", code, description))
}

pub async fn get_github_user(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<GitHubUser, String> {
    let resp = client
        .get(format!("{}/user", GITHUB_API_BASE_URL))
        .headers(github_headers(github_token))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("GitHub user request failed: {}", err))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| format!("GitHub user body read failed: {}", err))?;
    if !status.is_success() {
        return Err(format!("GitHub user returned {}: {}", status, text));
    }

    serde_json::from_str::<GitHubUser>(&text)
        .map_err(|err| format!("GitHub user JSON parse failed: {}", err))
}

pub async fn fetch_copilot_token(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<CopilotTokenResponse, String> {
    let resp = client
        .get(format!("{}/copilot_internal/v2/token", GITHUB_API_BASE_URL))
        .headers(github_headers(github_token))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Copilot token request failed: {}", err))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| format!("Copilot token body read failed: {}", err))?;
    if !status.is_success() {
        return Err(format!("Copilot token returned {}: {}", status, text));
    }

    serde_json::from_str::<CopilotTokenResponse>(&text)
        .map_err(|err| format!("Copilot token JSON parse failed: {}", err))
}

pub async fn fetch_copilot_user(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<serde_json::Value, String> {
    let resp = client
        .get(format!("{}/copilot_internal/user", GITHUB_API_BASE_URL))
        .headers(github_headers(github_token))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Copilot user request failed: {}", err))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| format!("Copilot user body read failed: {}", err))?;
    if !status.is_success() {
        return Err(format!("Copilot user returned {}: {}", status, text));
    }

    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|err| format!("Copilot user JSON parse failed: {}", err))
}

pub async fn ensure_copilot_token(
    state: &crate::AppState,
    account: &CopilotAccount,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    if let (Some(token), Some(expires_at)) = (&account.copilot_token, account.copilot_expires_at) {
        if expires_at.saturating_sub(now) > 60 && !token.trim().is_empty() {
            return Ok(token.clone());
        }
    }

    let token = fetch_copilot_token(&state.client, &account.github_token).await?;
    persist_copilot_token(state, account, &token)?;
    Ok(token.token)
}

pub fn copilot_headers(copilot_token: &str, vision: bool, initiator: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert_header(
        &mut headers,
        "Authorization",
        &format!("Bearer {}", copilot_token.trim()),
    );
    insert_header(&mut headers, "Content-Type", "application/json");
    insert_header(&mut headers, "copilot-integration-id", "vscode-chat");
    insert_header(
        &mut headers,
        "editor-version",
        &format!("vscode/{}", VSCODE_VERSION),
    );
    insert_header(
        &mut headers,
        "editor-plugin-version",
        &format!("copilot-chat/{}", COPILOT_VERSION),
    );
    insert_header(
        &mut headers,
        "User-Agent",
        &format!("GitHubCopilotChat/{}", COPILOT_VERSION),
    );
    insert_header(&mut headers, "openai-intent", "conversation-panel");
    insert_header(&mut headers, "x-github-api-version", COPILOT_API_VERSION);
    insert_header(&mut headers, "x-request-id", &Uuid::new_v4().to_string());
    insert_header(
        &mut headers,
        "x-vscode-user-agent-library-version",
        "electron-fetch",
    );
    insert_header(&mut headers, "X-Initiator", initiator);
    if vision {
        insert_header(&mut headers, "copilot-vision-request", "true");
    }
    headers
}

pub fn save_auth(
    state: &crate::AppState,
    github_token: &str,
    account_type: &str,
    label: Option<&str>,
    user: &GitHubUser,
    copilot_token: &CopilotTokenResponse,
    models: &[CopilotModelInfo],
) -> Result<String, String> {
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/io-gateway/auths".to_string());
    std::fs::create_dir_all(&auth_dir).map_err(|err| err.to_string())?;

    let account_type = normalize_account_type(Some(account_type));
    let label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&user.login);
    let file_name = format!("copilot-{}.json", sanitize_label(label));
    let path = PathBuf::from(&auth_dir).join(file_name);

    let out = serde_json::json!({
        "type": "copilot",
        "account_id": user.id.map(|id| id.to_string()).unwrap_or_else(|| user.login.clone()),
        "label": label,
        "login": user.login,
        "github_token": github_token.trim(),
        "copilot_token": copilot_token.token,
        "copilot_expires_at": copilot_token.expires_at,
        "copilot_refresh_in": copilot_token.refresh_in,
        "account_type": account_type,
        "models": models,
        "saved_at": chrono::Utc::now().to_rfc3339(),
    });
    super::super::atomic_write_json(&path, &out)?;

    super::accounts::reload_state(state);
    Ok(path.to_string_lossy().to_string())
}

fn persist_copilot_token(
    state: &crate::AppState,
    account: &CopilotAccount,
    token: &CopilotTokenResponse,
) -> Result<(), String> {
    let Some(file_name) = account.file_name.as_deref() else {
        return Ok(());
    };
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| ".".to_string());
    let path = PathBuf::from(&auth_dir).join(file_name);
    let data = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let mut value: serde_json::Value =
        serde_json::from_str(&data).map_err(|err| err.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "copilot_token".to_string(),
            serde_json::Value::String(token.token.clone()),
        );
        object.insert(
            "copilot_expires_at".to_string(),
            serde_json::json!(token.expires_at),
        );
        object.insert(
            "copilot_refresh_in".to_string(),
            token
                .refresh_in
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert(
            "updated_at".to_string(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        );
    }
    super::super::atomic_write_json(&path, &value)?;

    {
        let mut accounts = state.copilot_accounts.lock().unwrap();
        if let Some(stored) = accounts
            .iter_mut()
            .find(|stored| stored.file_name.as_deref() == Some(file_name))
        {
            stored.copilot_token = Some(token.token.clone());
            stored.copilot_expires_at = Some(token.expires_at);
            stored.copilot_refresh_in = token.refresh_in;
        }
    }
    Ok(())
}

fn github_headers(github_token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert_header(
        &mut headers,
        "Authorization",
        &format!("token {}", github_token.trim()),
    );
    insert_header(&mut headers, "Content-Type", "application/json");
    insert_header(&mut headers, "Accept", "application/json");
    insert_header(
        &mut headers,
        "editor-version",
        &format!("vscode/{}", VSCODE_VERSION),
    );
    insert_header(
        &mut headers,
        "editor-plugin-version",
        &format!("copilot-chat/{}", COPILOT_VERSION),
    );
    insert_header(
        &mut headers,
        "User-Agent",
        &format!("GitHubCopilotChat/{}", COPILOT_VERSION),
    );
    insert_header(&mut headers, "x-github-api-version", GITHUB_API_VERSION);
    insert_header(
        &mut headers,
        "x-vscode-user-agent-library-version",
        "electron-fetch",
    );
    headers
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
