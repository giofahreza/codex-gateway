use chrono::{Duration as ChronoDuration, Utc};
use rand::Rng;
use serde::Deserialize;
use std::time::Duration;
use url::Url;

use super::super::oauth::{provider_config, OAuthProvider};
use super::accounts::GeminiAccount;

const GOOGLE_USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json";
const GEMINI_CLI_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
const GEMINI_CLI_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";
const GEMINI_CLI_API_CLIENT: &str = "gl-node/22.17.0";
const GEMINI_CLI_CLIENT_METADATA: &str =
    "ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI";
const GEMINI_FREE_TIER_ID: &str = "free-tier";
const GEMINI_LEGACY_TIER_ID: &str = "legacy-tier";

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub expires_in: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiSetup {
    pub project_id: String,
    pub auto_project: bool,
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

pub async fn ensure_project_and_onboard(
    client: &reqwest::Client,
    access_token: &str,
    requested_project: &str,
) -> Result<GeminiSetup, String> {
    let requested_project = requested_project
        .trim()
        .split(',')
        .map(str::trim)
        .find(|project_id| !project_id.is_empty())
        .map(str::to_string);
    let load_body = build_load_code_assist_request(requested_project.as_deref());

    let load_resp = call_gemini_cli(client, access_token, "loadCodeAssist", &load_body).await?;
    if let Some(error) = validation_required_error(&load_resp) {
        return Err(error);
    }
    if has_current_tier(&load_resp) {
        if let Some(project_id) = extract_project_id(&load_resp) {
            return Ok(GeminiSetup {
                project_id,
                auto_project: requested_project.is_none(),
            });
        }
        if let Some(project_id) = requested_project {
            return Ok(GeminiSetup {
                project_id,
                auto_project: false,
            });
        }
        return Err(project_required_error(&load_resp));
    }

    let tier_id = default_onboarding_tier(&load_resp);
    // Gemini Code Assist for individuals provisions a managed project. The
    // current official Gemini CLI deliberately omits the project for this tier.
    let onboarding_project = onboarding_project_for_tier(&tier_id, requested_project.as_deref());
    let onboard_body = build_onboarding_request(&tier_id, onboarding_project);
    let started_at = std::time::Instant::now();
    let mut onboard_resp =
        call_gemini_cli(client, access_token, "onboardUser", &onboard_body).await?;

    while !onboard_resp
        .get("done")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        if started_at.elapsed() > Duration::from_secs(300) {
            return Err("Gemini onboarding timed out".to_string());
        }
        let operation_name = onboard_resp
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Gemini onboarding did not return an operation name".to_string())?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        onboard_resp = get_gemini_cli_operation(client, access_token, operation_name).await?;
    }

    if let Some(error) = onboarding_operation_error(&onboard_resp) {
        return Err(error);
    }

    if let Some(project_id) = onboard_resp.get("response").and_then(extract_project_id) {
        return Ok(GeminiSetup {
            project_id,
            auto_project: onboarding_project.is_none(),
        });
    }
    if let Some(project_id) = onboarding_project {
        return Ok(GeminiSetup {
            project_id: project_id.to_string(),
            auto_project: false,
        });
    }
    Err(project_required_error(&load_resp))
}

fn has_current_tier(value: &serde_json::Value) -> bool {
    value.get("currentTier").is_some_and(|tier| !tier.is_null())
}

fn default_onboarding_tier(value: &serde_json::Value) -> String {
    value
        .get("allowedTiers")
        .and_then(|tiers| tiers.as_array())
        .and_then(|tiers| {
            tiers.iter().find_map(|tier| {
                tier.get("isDefault")
                    .and_then(|is_default| is_default.as_bool())
                    .filter(|is_default| *is_default)
                    .and_then(|_| tier.get("id"))
                    .and_then(|id| id.as_str())
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| GEMINI_LEGACY_TIER_ID.to_string())
}

fn code_assist_metadata(project_id: Option<&str>) -> serde_json::Value {
    let mut metadata = serde_json::json!({
        "ideType": "IDE_UNSPECIFIED",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI"
    });
    if let Some(project_id) = project_id
        .map(str::trim)
        .filter(|project_id| !project_id.is_empty())
    {
        metadata["duetProject"] = serde_json::Value::String(project_id.to_string());
    }
    metadata
}

pub(crate) fn build_load_code_assist_request(project_id: Option<&str>) -> serde_json::Value {
    let project_id = project_id
        .map(str::trim)
        .filter(|project_id| !project_id.is_empty());
    let mut request = serde_json::json!({
        "metadata": code_assist_metadata(project_id)
    });
    if let Some(project_id) = project_id {
        request["cloudaicompanionProject"] = serde_json::Value::String(project_id.to_string());
    }
    request
}

fn build_onboarding_request(tier_id: &str, project_id: Option<&str>) -> serde_json::Value {
    let mut request = serde_json::json!({
        "tierId": tier_id,
        "metadata": code_assist_metadata(project_id)
    });
    if let Some(project_id) = project_id
        .map(str::trim)
        .filter(|project_id| !project_id.is_empty())
    {
        request["cloudaicompanionProject"] = serde_json::Value::String(project_id.to_string());
    }
    request
}

fn onboarding_project_for_tier<'a>(
    tier_id: &str,
    requested_project: Option<&'a str>,
) -> Option<&'a str> {
    if tier_id.eq_ignore_ascii_case(GEMINI_FREE_TIER_ID) {
        None
    } else {
        requested_project
    }
}

fn project_required_error(load_resp: &serde_json::Value) -> String {
    let reasons = load_resp
        .get("ineligibleTiers")
        .and_then(|tiers| tiers.as_array())
        .map(|tiers| {
            tiers
                .iter()
                .filter_map(|tier| tier.get("reasonMessage").and_then(|reason| reason.as_str()))
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if reasons.is_empty() {
        "Gemini Code Assist requires a Google Cloud project for this account; provide one only for an organization or subscription account".to_string()
    } else {
        format!(
            "Gemini Code Assist account is not eligible: {}",
            reasons.join("; ")
        )
    }
}

fn validation_required_error(load_resp: &serde_json::Value) -> Option<String> {
    if has_current_tier(load_resp) {
        return None;
    }

    let tier = load_resp
        .get("ineligibleTiers")
        .and_then(|tiers| tiers.as_array())?
        .iter()
        .find(|tier| {
            tier.get("reasonCode")
                .and_then(|reason| reason.as_str())
                .is_some_and(|reason| reason.eq_ignore_ascii_case("VALIDATION_REQUIRED"))
        })?;
    let description = tier
        .get("reasonMessage")
        .and_then(|message| message.as_str())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("Google requires account verification");
    let validation_url = tier
        .get("validationUrl")
        .and_then(|url| url.as_str())
        .map(str::trim)
        .filter(|url| !url.is_empty());

    Some(match validation_url {
        Some(url) => format!(
            "Gemini Code Assist account verification is required: {}. Open {}, complete verification, then start a new Gemini login.",
            description, url
        ),
        None => format!(
            "Gemini Code Assist account verification is required: {}. Complete the Google verification, then start a new Gemini login.",
            description
        ),
    })
}

fn onboarding_operation_error(operation: &serde_json::Value) -> Option<String> {
    let error = operation.get("error")?;
    let message = error
        .get("message")
        .and_then(|message| message.as_str())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string());
    Some(format!("Gemini onboarding failed: {}", message))
}

pub fn save_auth(
    state: &crate::AppState,
    email: &str,
    token_resp: &TokenResponse,
    project_id: &str,
    auto: bool,
    checked: bool,
) -> Result<String, String> {
    let expires_at =
        (Utc::now() + ChronoDuration::seconds(token_resp.expires_in.max(0))).to_rfc3339();
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/io-gateway/auths".to_string());
    std::fs::create_dir_all(&auth_dir).map_err(|e| e.to_string())?;
    let generated_path =
        std::path::Path::new(&auth_dir).join(credential_file_name(email, project_id));
    let path = if auto {
        existing_auto_managed_account_path(std::path::Path::new(&auth_dir), email)
            .unwrap_or(generated_path)
    } else {
        generated_path
    };
    let refresh_token = token_resp
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .or_else(|| existing_refresh_token(&path))
        .ok_or_else(|| {
            "google oauth did not return a refresh token; retry consent flow".to_string()
        })?;

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
        .or_else(|| {
            value
                .get("project")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

pub(crate) fn gemini_cli_base_url() -> String {
    let provider = provider_config(None, OAuthProvider::Gemini);
    provider
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(GEMINI_CLI_ENDPOINT)
        .trim_end_matches('/')
        .to_string()
}

fn existing_auto_managed_account_path(
    auth_dir: &std::path::Path,
    email: &str,
) -> Option<std::path::PathBuf> {
    let mut entries = std::fs::read_dir(auth_dir)
        .ok()?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());

    entries.into_iter().find_map(|entry| {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            return None;
        }
        let value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str::<serde_json::Value>(&data).ok())?;
        let is_matching_account = value.get("type").and_then(|kind| kind.as_str())
            == Some("gemini")
            && value.get("auto").and_then(|auto| auto.as_bool()) == Some(true)
            && value
                .get("email")
                .and_then(|candidate| candidate.as_str())
                .is_some_and(|candidate| candidate.trim().eq_ignore_ascii_case(email.trim()));
        is_matching_account.then_some(path)
    })
}

fn existing_refresh_token(path: &std::path::Path) -> Option<String> {
    let value = std::fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str::<serde_json::Value>(&data).ok())?;
    value
        .get("token")
        .and_then(|token| token.get("refresh_token"))
        .or_else(|| value.get("refresh_token"))
        .and_then(|token| token.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

async fn call_gemini_cli(
    client: &reqwest::Client,
    access_token: &str,
    endpoint: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let base_url = gemini_cli_base_url();
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

async fn get_gemini_cli_operation(
    client: &reqwest::Client,
    access_token: &str,
    operation_name: &str,
) -> Result<serde_json::Value, String> {
    let operation_name = operation_name.trim().trim_start_matches('/');
    if operation_name.is_empty() {
        return Err("Gemini onboarding operation name is empty".to_string());
    }
    let base_url = gemini_cli_base_url();
    let resp = gemini_headers(
        client
            .get(format!("{}/v1internal/{}", base_url, operation_name))
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(30)),
        access_token,
    )
    .send()
    .await
    .map_err(|e| e.to_string())?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "Gemini onboarding operation {} failed: {}",
            operation_name, text
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn free_tier_onboarding_uses_a_managed_project() {
        let project = onboarding_project_for_tier(GEMINI_FREE_TIER_ID, Some("user-project"));
        let body = build_onboarding_request(GEMINI_FREE_TIER_ID, project);

        assert!(body.get("cloudaicompanionProject").is_none());
        assert!(body["metadata"].get("duetProject").is_none());
        assert_eq!(body["tierId"], GEMINI_FREE_TIER_ID);
    }

    #[test]
    fn load_request_uses_the_explicit_project_in_code_assist_metadata() {
        let body = build_load_code_assist_request(Some("organization-project"));

        assert_eq!(body["cloudaicompanionProject"], "organization-project");
        assert_eq!(body["metadata"]["duetProject"], "organization-project");
    }

    #[test]
    fn organization_tier_includes_the_configured_project_everywhere() {
        let project = onboarding_project_for_tier("standard-tier", Some("organization-project"));
        let body = build_onboarding_request("standard-tier", project);

        assert_eq!(body["cloudaicompanionProject"], "organization-project");
        assert_eq!(body["metadata"]["duetProject"], "organization-project");
    }

    #[test]
    fn default_tier_uses_google_free_tier_when_available() {
        let response = json!({
            "allowedTiers": [
                { "id": "standard-tier", "isDefault": false },
                { "id": GEMINI_FREE_TIER_ID, "isDefault": true }
            ]
        });

        assert_eq!(default_onboarding_tier(&response), GEMINI_FREE_TIER_ID);
    }

    #[test]
    fn extract_project_id_accepts_code_assist_response_shapes() {
        assert_eq!(
            extract_project_id(&json!({ "cloudaicompanionProject": { "id": "managed-project" } })),
            Some("managed-project".to_string())
        );
        assert_eq!(
            extract_project_id(&json!({ "project": "legacy-project" })),
            Some("legacy-project".to_string())
        );
    }

    #[test]
    fn validation_required_response_includes_the_google_verification_link() {
        let error = validation_required_error(&json!({
            "currentTier": null,
            "ineligibleTiers": [{
                "reasonCode": "VALIDATION_REQUIRED",
                "reasonMessage": "Verify this account",
                "validationUrl": "https://accounts.google.com/verify"
            }]
        }))
        .unwrap();

        assert!(error.contains("Verify this account"));
        assert!(error.contains("https://accounts.google.com/verify"));
    }

    #[test]
    fn onboarding_operation_error_is_not_reported_as_a_missing_project() {
        let error = onboarding_operation_error(&json!({
            "done": true,
            "error": { "message": "Google rejected the requested tier" }
        }))
        .unwrap();

        assert_eq!(
            error,
            "Gemini onboarding failed: Google rejected the requested tier"
        );
    }

    #[test]
    fn auto_managed_relogin_reuses_the_existing_account_file() {
        let auth_dir = std::env::temp_dir().join(format!(
            "io-gateway-gemini-auth-tests-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&auth_dir).unwrap();
        let existing_path = auth_dir.join("gemini-person-old-project.json");
        std::fs::write(
            &existing_path,
            json!({
                "type": "gemini",
                "email": "person@example.com",
                "auto": true,
                "token": { "refresh_token": "existing-refresh-token" }
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            existing_auto_managed_account_path(&auth_dir, "PERSON@example.com"),
            Some(existing_path.clone())
        );
        assert_eq!(
            existing_refresh_token(&existing_path),
            Some("existing-refresh-token".to_string())
        );

        let _ = std::fs::remove_dir_all(auth_dir);
    }
}
