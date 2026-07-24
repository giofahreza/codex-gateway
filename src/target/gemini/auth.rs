use chrono::{Duration as ChronoDuration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use url::Url;

use super::super::oauth::{provider_config, OAuthProvider};
use super::accounts::GeminiAccount;

const GOOGLE_USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json";
const GOOGLE_PROJECTS_URL: &str = "https://cloudresourcemanager.googleapis.com/v1/projects";
const SERVICE_USAGE_URL: &str = "https://serviceusage.googleapis.com";
const GEMINI_CLI_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
const GEMINI_CLI_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";
const GEMINI_CLI_API_CLIENT: &str = "gl-node/22.17.0";
const GEMINI_CLI_CLIENT_METADATA: &str =
    "ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI";

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub expires_in: i64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct GcpProject {
    #[serde(rename = "projectId")]
    pub project_id: String,
    pub name: String,
}

#[derive(Deserialize)]
struct ProjectsResponse {
    #[serde(default)]
    projects: Vec<GcpProject>,
}

pub fn build_auth_url() -> Result<(String, String), String> {
    let provider = provider_config(None, OAuthProvider::Gemini);
    let client_id = env_client_id()?;
    let redirect_uri = required_config(
        &provider.redirect_uri,
        "oauth.providers.gemini.redirect_uri",
    )?;
    let authorize_url = required_config(
        &provider.authorize_url,
        "oauth.providers.gemini.authorize_url",
    )?;
    let state_token: String = rand::rng()
        .sample_iter(&rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let mut url = Url::parse(&authorize_url).map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &provider.scopes.join(" "))
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", &state_token);

    Ok((url.to_string(), state_token))
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
) -> Result<TokenResponse, String> {
    let provider = provider_config(None, OAuthProvider::Gemini);
    let client_id = env_client_id()?;
    let client_secret = env_client_secret()?;
    let redirect_uri = required_config(
        &provider.redirect_uri,
        "oauth.providers.gemini.redirect_uri",
    )?;
    let token_url = required_config(&provider.token_url, "oauth.providers.gemini.token_url")?;
    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("code", code),
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
    account: &GeminiAccount,
) -> Result<TokenResponse, String> {
    let provider = provider_config(None, OAuthProvider::Gemini);
    let client_id = account_client_id(account)?;
    let client_secret = account_client_secret(account)?;
    let token_url = required_config(&provider.token_url, "oauth.providers.gemini.token_url")?;
    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", account.refresh_token.as_str()),
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
        .map(|value| value.to_string())
        .ok_or_else(|| "user info missing email".to_string())
}

pub async fn fetch_projects(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<Vec<GcpProject>, String> {
    let resp = client
        .get(GOOGLE_PROJECTS_URL)
        .bearer_auth(access_token)
        .header("User-Agent", GEMINI_CLI_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("failed to fetch projects: {}", body));
    }

    let value = resp
        .json::<ProjectsResponse>()
        .await
        .map_err(|e| e.to_string())?;
    Ok(value.projects)
}

pub async fn discover_project_id(
    client: &reqwest::Client,
    access_token: &str,
    requested_project: Option<&str>,
) -> Result<Option<String>, String> {
    let mut body = serde_json::json!({
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });
    if let Some(project_id) = requested_project
        .map(str::trim)
        .filter(|project_id| !project_id.is_empty())
    {
        body["cloudaicompanionProject"] = serde_json::Value::String(project_id.to_string());
    }

    let value = call_gemini_cli(client, access_token, "loadCodeAssist", &body).await?;
    Ok(extract_project_id(&value))
}

pub async fn ensure_project_and_onboard(
    client: &reqwest::Client,
    access_token: &str,
    requested_project: &str,
) -> Result<String, String> {
    let metadata = serde_json::json!({
        "ideType": "IDE_UNSPECIFIED",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI"
    });
    let trimmed_request = requested_project.trim();
    let explicit_project = !trimmed_request.is_empty();

    let mut load_body = serde_json::json!({
        "metadata": metadata.clone()
    });
    if explicit_project {
        load_body["cloudaicompanionProject"] =
            serde_json::Value::String(trimmed_request.to_string());
    }

    let load_resp = call_gemini_cli(client, access_token, "loadCodeAssist", &load_body).await?;
    let tier_id = load_resp
        .get("allowedTiers")
        .and_then(|value| value.as_array())
        .and_then(|tiers| {
            tiers.iter().find_map(|tier| {
                let is_default = tier
                    .get("isDefault")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                if !is_default {
                    return None;
                }
                tier.get("id")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
            })
        })
        .unwrap_or_else(|| "legacy-tier".to_string());

    let project_id = if explicit_project {
        trimmed_request.to_string()
    } else {
        extract_project_id(&load_resp)
            .ok_or_else(|| "Gemini onboarding requires a Google Cloud project ID".to_string())?
    };

    let onboard_body = serde_json::json!({
        "tierId": tier_id,
        "metadata": metadata,
        "cloudaicompanionProject": project_id.clone()
    });
    let started_at = std::time::Instant::now();

    loop {
        let onboard_resp =
            call_gemini_cli(client, access_token, "onboardUser", &onboard_body).await?;
        if onboard_resp
            .get("done")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            let response_project = onboard_resp
                .get("response")
                .and_then(extract_project_id)
                .or_else(|| {
                    onboard_resp
                        .get("response")
                        .and_then(|value| value.get("cloudaicompanionProject"))
                        .and_then(|value| value.as_str())
                        .map(|value| value.trim().to_string())
                })
                .filter(|value| !value.is_empty());
            let final_project = if explicit_project {
                project_id.clone()
            } else {
                response_project.unwrap_or_else(|| project_id.clone())
            };
            if final_project.trim().is_empty() {
                return Err("Gemini onboarding completed without a project id".to_string());
            }
            return Ok(final_project);
        }

        if started_at.elapsed() > Duration::from_secs(300) {
            return Err("Gemini onboarding timed out".to_string());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub async fn ensure_cloud_api_enabled(
    client: &reqwest::Client,
    access_token: &str,
    project_id: &str,
) -> Result<(), String> {
    let project_id = project_id.trim();
    if project_id.is_empty() {
        return Err("project id is required".to_string());
    }

    let service = "cloudaicompanion.googleapis.com";
    let check_url = format!(
        "{}/v1/projects/{}/services/{}",
        SERVICE_USAGE_URL, project_id, service
    );
    let check_resp = client
        .get(&check_url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("User-Agent", GEMINI_CLI_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if check_resp.status().is_success() {
        let body: serde_json::Value = check_resp.json().await.map_err(|e| e.to_string())?;
        if body
            .get("state")
            .and_then(|value| value.as_str())
            .map(|value| value.eq_ignore_ascii_case("ENABLED"))
            .unwrap_or(false)
        {
            return Ok(());
        }
    }

    let enable_url = format!(
        "{}/v1/projects/{}/services/{}:enable",
        SERVICE_USAGE_URL, project_id, service
    );
    let resp = client
        .post(&enable_url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("User-Agent", GEMINI_CLI_USER_AGENT)
        .body("{}")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        return Ok(());
    }

    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let message = body
        .get("error")
        .and_then(|value| value.get("message"))
        .and_then(|value| value.as_str())
        .unwrap_or("project activation required");
    if message.to_ascii_lowercase().contains("already enabled") {
        return Ok(());
    }
    Err(format!("project activation required: {}", message))
}

pub fn save_auth(
    state: &crate::AppState,
    email: &str,
    token_resp: &TokenResponse,
    project_id: &str,
    auto: bool,
    checked: bool,
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

    let expires_at =
        (Utc::now() + ChronoDuration::seconds(token_resp.expires_in.max(0))).to_rfc3339();
    let file_name = credential_file_name(email, project_id);
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/io-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    std::fs::create_dir_all(&auth_dir).map_err(|e| e.to_string())?;

    let client_id = env_client_id()?;
    let client_secret = env_client_secret()?;
    let provider = provider_config(None, OAuthProvider::Gemini);
    let token_url = required_config(&provider.token_url, "oauth.providers.gemini.token_url")?;

    let token = serde_json::json!({
        "access_token": token_resp.access_token.clone(),
        "refresh_token": refresh_token,
        "token_type": token_resp.token_type.clone().unwrap_or_else(|| "Bearer".to_string()),
        "expiry": expires_at,
        "token_uri": token_url,
        "client_id": client_id,
        "client_secret": client_secret,
        "scopes": provider.scopes,
        "universe_domain": "googleapis.com"
    });

    let out = serde_json::json!({
        "type": "gemini",
        "email": email,
        "project_id": project_id,
        "auto": auto,
        "checked": checked,
        "token": token
    });
    super::super::atomic_write_json(&path, &out)?;
    super::accounts::reload_state(state);
    Ok(path.to_string_lossy().to_string())
}

pub async fn ensure_access_token(
    state: &crate::AppState,
    account: &GeminiAccount,
) -> Result<String, String> {
    let account_key = crate::gemini_stats_key(account);
    let refresh_lock = crate::account_refresh_lock(state, "gemini", &account_key);
    let _guard = refresh_lock.lock().await;
    let current = state
        .gemini_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|candidate| candidate.file_name == account.file_name)
        .cloned()
        .unwrap_or_else(|| account.clone());
    if let Some(access_token) = current.access_token.as_ref() {
        let still_valid = current
            .expiry
            .as_deref()
            .and_then(parse_rfc3339)
            .map(|expires_at| expires_at > Utc::now() + ChronoDuration::seconds(60))
            .unwrap_or(false);
        if still_valid {
            return Ok(access_token.clone());
        }
    }

    let refreshed = refresh_access_token(&state.client, &current).await?;
    persist_refreshed_account(state, &current, &refreshed)?;
    Ok(refreshed.access_token)
}

pub fn request_project_id(account: &GeminiAccount) -> String {
    account
        .project_id
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

pub fn gemini_headers(
    builder: reqwest::RequestBuilder,
    access_token: &str,
) -> reqwest::RequestBuilder {
    builder
        .bearer_auth(access_token)
        .header("User-Agent", GEMINI_CLI_USER_AGENT)
        .header("X-Goog-Api-Client", GEMINI_CLI_API_CLIENT)
        .header("Client-Metadata", GEMINI_CLI_CLIENT_METADATA)
}

fn extract_project_id(value: &serde_json::Value) -> Option<String> {
    if let Some(project_id) = value
        .get("cloudaicompanionProject")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(project_id.to_string());
    }

    value
        .get("cloudaicompanionProject")
        .and_then(|value| value.get("id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

async fn call_gemini_cli(
    client: &reqwest::Client,
    access_token: &str,
    endpoint: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let provider = provider_config(None, OAuthProvider::Gemini);
    let base_url = provider
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(GEMINI_CLI_ENDPOINT)
        .trim_end_matches('/')
        .to_string();
    let resp = gemini_headers(
        client
            .post(format!("{}/v1internal:{}", base_url, endpoint))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.to_string())
            .timeout(Duration::from_secs(30)),
        access_token,
    )
    .send()
    .await
    .map_err(|e| e.to_string())?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Gemini {} failed: {}", endpoint, text));
    }

    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn persist_refreshed_account(
    state: &crate::AppState,
    account: &GeminiAccount,
    token_resp: &TokenResponse,
) -> Result<(), String> {
    let Some(file_name) = account.file_name.as_ref() else {
        return Ok(());
    };

    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/io-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut value: serde_json::Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    let expires_at =
        (Utc::now() + ChronoDuration::seconds(token_resp.expires_in.max(0))).to_rfc3339();
    let client_id = account_client_id(account)?;
    let client_secret = account_client_secret(account)?;
    let provider = provider_config(None, OAuthProvider::Gemini);
    let token_url = required_config(&provider.token_url, "oauth.providers.gemini.token_url")?;

    if let serde_json::Value::Object(map) = &mut value {
        let token_entry = map
            .entry("token".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !token_entry.is_object() {
            *token_entry = serde_json::json!({});
        }
        if let Some(token_map) = token_entry.as_object_mut() {
            token_map.insert(
                "access_token".to_string(),
                serde_json::Value::String(token_resp.access_token.clone()),
            );
            token_map.insert(
                "expiry".to_string(),
                serde_json::Value::String(expires_at.clone()),
            );
            token_map.insert(
                "token_type".to_string(),
                serde_json::Value::String(
                    token_resp
                        .token_type
                        .clone()
                        .unwrap_or_else(|| "Bearer".to_string()),
                ),
            );
            token_map.insert(
                "token_uri".to_string(),
                serde_json::Value::String(token_url.clone()),
            );
            token_map.insert(
                "client_id".to_string(),
                serde_json::Value::String(client_id),
            );
            token_map.insert(
                "client_secret".to_string(),
                serde_json::Value::String(client_secret),
            );
            token_map.insert("scopes".to_string(), serde_json::json!(provider.scopes));
            token_map.insert(
                "universe_domain".to_string(),
                serde_json::Value::String("googleapis.com".to_string()),
            );
            if let Some(refresh_token) = token_resp.refresh_token.as_ref() {
                token_map.insert(
                    "refresh_token".to_string(),
                    serde_json::Value::String(refresh_token.clone()),
                );
            }
        }
    }

    super::super::atomic_write_json(&path, &value)?;

    let mut accounts = state.gemini_accounts.lock().unwrap();
    if let Some(current) = accounts
        .iter_mut()
        .find(|current| current.file_name.as_deref() == Some(file_name.as_str()))
    {
        current.access_token = Some(token_resp.access_token.clone());
        current.token_type = Some(
            token_resp
                .token_type
                .clone()
                .unwrap_or_else(|| "Bearer".to_string()),
        );
        current.expiry = Some(expires_at);
        if let Some(refresh_token) = token_resp.refresh_token.as_ref() {
            current.refresh_token = refresh_token.clone();
        }
    }

    Ok(())
}

fn credential_file_name(email: &str, project_id: &str) -> String {
    let email = sanitize_label(email);
    let project = project_id.trim();
    let project = if project.eq_ignore_ascii_case("all") || project.contains(',') {
        "all".to_string()
    } else {
        sanitize_label(project)
    };
    format!("gemini-{}-{}.json", email, project)
}

fn env_client_id() -> Result<String, String> {
    let provider = provider_config(None, OAuthProvider::Gemini);
    required_config(&provider.client_id, "oauth.providers.gemini.client_id")
}

fn env_client_secret() -> Result<String, String> {
    let provider = provider_config(None, OAuthProvider::Gemini);
    required_config(
        &provider.client_secret,
        "oauth.providers.gemini.client_secret",
    )
}

fn account_client_id(account: &GeminiAccount) -> Result<String, String> {
    if let Some(client_id) = account
        .oauth_client_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(client_id.to_string());
    }
    env_client_id()
}

fn account_client_secret(account: &GeminiAccount) -> Result<String, String> {
    if let Some(client_secret) = account
        .oauth_client_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(client_secret.to_string());
    }
    env_client_secret()
}

fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
                ch
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
        .ok_or_else(|| format!("{} is required for Gemini OAuth login", field_name))
}
