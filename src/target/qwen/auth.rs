use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use rand::{distr::Alphanumeric, Rng};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

use super::accounts::QwenAccount;

const QWEN_DEVICE_CODE_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/device/code";
const QWEN_TOKEN_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/token";
const QWEN_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_SCOPE: &str = "openid profile email model.completion";
const QWEN_DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const QWEN_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";
const QWEN_X_GOOG_API_CLIENT: &str = "gl-node/22.17.0";
const QWEN_CLIENT_METADATA: &str =
    "ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI";

#[derive(Clone)]
pub struct PendingDeviceFlow {
    pub verification_uri_complete: String,
    pub user_code: String,
    pub interval_seconds: u64,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub status: PendingStatus,
}

#[derive(Clone)]
pub enum PendingStatus {
    Pending,
    Completed { saved_path: String, label: String },
    Error { message: String },
}

#[derive(Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: Option<u64>,
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub resource_url: Option<String>,
    pub expires_in: i64,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
}

pub struct RefreshResult {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub resource_url: Option<String>,
    pub expired_at: String,
}

#[derive(Clone)]
pub struct QwenIdentity {
    pub email: Option<String>,
    pub subject: Option<String>,
    pub label: String,
    pub file_key: String,
}

pub fn qwen_headers(
    builder: reqwest::RequestBuilder,
    access_token: &str,
) -> reqwest::RequestBuilder {
    builder
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", QWEN_USER_AGENT)
        .header("X-Goog-Api-Client", QWEN_X_GOOG_API_CLIENT)
        .header("Client-Metadata", QWEN_CLIENT_METADATA)
}

pub async fn initiate_device_flow(
    client: &reqwest::Client,
) -> Result<(DeviceCodeResponse, String), String> {
    let code_verifier = generate_code_verifier();
    let code_challenge = code_challenge(&code_verifier);
    let params = [
        ("client_id", QWEN_CLIENT_ID),
        ("scope", QWEN_SCOPE),
        ("code_challenge", code_challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];

    let resp = client
        .post(QWEN_DEVICE_CODE_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&params)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("device flow start failed: {}", body));
    }

    let flow = resp
        .json::<DeviceCodeResponse>()
        .await
        .map_err(|e| e.to_string())?;

    if flow.device_code.trim().is_empty() || flow.verification_uri_complete.trim().is_empty() {
        return Err("device flow response was missing device_code or verification url".to_string());
    }

    Ok((flow, code_verifier))
}

pub fn track_pending_login(state: &crate::AppState, session_id: &str, flow: &DeviceCodeResponse) {
    let mut pending = state.qwen_oauth_pending.lock().unwrap();
    pending.insert(
        session_id.to_string(),
        PendingDeviceFlow {
            verification_uri_complete: flow.verification_uri_complete.clone(),
            user_code: flow.user_code.clone(),
            interval_seconds: flow.interval.unwrap_or(5).max(1),
            created_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(flow.expires_in.max(60)),
            status: PendingStatus::Pending,
        },
    );
}

pub fn spawn_device_poll(
    state: crate::AppState,
    session_id: String,
    device_code: String,
    code_verifier: String,
    interval_seconds: u64,
) {
    tokio::spawn(async move {
        let result = poll_for_token(
            &state.client,
            &device_code,
            &code_verifier,
            Duration::from_secs(interval_seconds.max(1)),
        )
        .await;

        match result {
            Ok(token_resp) => match save_auth(&state, &token_resp) {
                Ok((saved_path, label)) => set_completed(&state, &session_id, saved_path, label),
                Err(err) => set_error(&state, &session_id, err),
            },
            Err(err) => set_error(&state, &session_id, err),
        }
    });
}

pub fn identity_from_access_token(access_token: &str, fallback_label: &str) -> QwenIdentity {
    let claims = parse_access_token_claims(access_token);
    let email = claims
        .as_ref()
        .and_then(|claims| claims.get("email"))
        .and_then(|v| v.as_str())
        .map(|value| value.to_string());
    let preferred_username = claims
        .as_ref()
        .and_then(|claims| claims.get("preferred_username"))
        .and_then(|v| v.as_str())
        .map(|value| value.to_string());
    let name = claims
        .as_ref()
        .and_then(|claims| claims.get("name"))
        .and_then(|v| v.as_str())
        .map(|value| value.to_string());
    let subject = claims
        .as_ref()
        .and_then(|claims| claims.get("sub"))
        .and_then(|v| v.as_str())
        .map(|value| value.to_string());

    let label = email
        .clone()
        .or(preferred_username.clone())
        .or(name)
        .or(subject.clone())
        .unwrap_or_else(|| fallback_label.to_string());
    let file_key = email
        .clone()
        .or(subject.clone())
        .unwrap_or_else(|| fallback_label.to_string());

    QwenIdentity {
        email,
        subject,
        label,
        file_key,
    }
}

fn parse_access_token_claims(access_token: &str) -> Option<serde_json::Value> {
    let mut parts = access_token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _sig = parts.next()?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

pub async fn ensure_access_token(
    state: &crate::AppState,
    account: &QwenAccount,
) -> Result<String, String> {
    if let Some(access_token) = account.access_token.as_ref() {
        let still_valid = account
            .expired_at
            .as_deref()
            .and_then(parse_rfc3339)
            .map(|expires_at| expires_at > Utc::now() + ChronoDuration::seconds(60))
            .unwrap_or(true);
        if still_valid {
            return Ok(access_token.clone());
        }
    }

    let refreshed = refresh_access_token(&state.client, &account.refresh_token).await?;
    persist_refreshed_account(state, account, &refreshed)?;
    Ok(refreshed.access_token)
}

pub async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<RefreshResult, String> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", QWEN_CLIENT_ID),
    ];

    let resp = client
        .post(QWEN_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&params)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token refresh failed: {}", body));
    }

    let token_resp = resp
        .json::<TokenResponse>()
        .await
        .map_err(|e| e.to_string())?;
    build_refresh_result(token_resp)
}

pub fn base_url(account: &QwenAccount) -> String {
    resource_to_base_url(account.resource_url.as_deref())
}

fn set_completed(state: &crate::AppState, session_id: &str, saved_path: String, label: String) {
    let mut pending = state.qwen_oauth_pending.lock().unwrap();
    if let Some(entry) = pending.get_mut(session_id) {
        entry.status = PendingStatus::Completed { saved_path, label };
    }
}

fn set_error(state: &crate::AppState, session_id: &str, message: String) {
    let mut pending = state.qwen_oauth_pending.lock().unwrap();
    if let Some(entry) = pending.get_mut(session_id) {
        entry.status = PendingStatus::Error { message };
    }
}

async fn poll_for_token(
    client: &reqwest::Client,
    device_code: &str,
    code_verifier: &str,
    initial_interval: Duration,
) -> Result<TokenResponse, String> {
    let mut poll_interval = initial_interval.max(Duration::from_secs(1));
    let deadline = Instant::now() + Duration::from_secs(300);

    loop {
        let params = [
            ("grant_type", QWEN_DEVICE_GRANT_TYPE),
            ("client_id", QWEN_CLIENT_ID),
            ("device_code", device_code),
            ("code_verifier", code_verifier),
        ];

        let resp = client
            .post(QWEN_TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .form(&params)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            return resp
                .json::<TokenResponse>()
                .await
                .map_err(|e| e.to_string());
        }

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let error = serde_json::from_str::<OAuthErrorResponse>(&body).ok();
        let error_code = error
            .as_ref()
            .and_then(|value| value.error.as_deref())
            .unwrap_or("");

        match error_code {
            "authorization_pending" => {}
            "slow_down" => {
                poll_interval = std::cmp::min(
                    poll_interval + Duration::from_secs(2),
                    Duration::from_secs(10),
                );
            }
            "expired_token" => {
                return Err("device code expired; start Qwen login again".to_string());
            }
            "access_denied" => {
                return Err("Qwen authorization was denied".to_string());
            }
            _ => {
                let message = error
                    .and_then(|value| value.error_description)
                    .unwrap_or(body);
                return Err(format!(
                    "device token poll failed ({}): {}",
                    status, message
                ));
            }
        }

        if Instant::now() >= deadline {
            return Err("Qwen login timed out; start again".to_string());
        }

        tokio::time::sleep(poll_interval).await;
    }
}

fn build_refresh_result(token_resp: TokenResponse) -> Result<RefreshResult, String> {
    if token_resp.access_token.trim().is_empty() {
        return Err("token response missing access token".to_string());
    }

    Ok(RefreshResult {
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token,
        resource_url: token_resp.resource_url,
        expired_at: (Utc::now() + ChronoDuration::seconds(token_resp.expires_in.max(0)))
            .to_rfc3339(),
    })
}

fn save_auth(
    state: &crate::AppState,
    token_resp: &TokenResponse,
) -> Result<(String, String), String> {
    let refresh_token = token_resp
        .refresh_token
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    if refresh_token.is_empty() {
        return Err("Qwen login did not return a refresh token".to_string());
    }

    let fallback_label = format!("qwen-{}", Utc::now().timestamp_millis());
    let identity = identity_from_access_token(&token_resp.access_token, &fallback_label);
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let file_name = format!("qwen-{}.json", sanitize_label(&identity.file_key));
    let path = std::path::Path::new(&auth_dir).join(file_name);
    let out = serde_json::json!({
        "type": "qwen",
        "email": identity.email.clone().unwrap_or_else(|| identity.label.clone()),
        "subject": identity.subject,
        "label": identity.label.clone(),
        "access_token": token_resp.access_token,
        "refresh_token": refresh_token,
        "resource_url": token_resp.resource_url,
        "last_refresh": Utc::now().to_rfc3339(),
        "expired": (Utc::now() + ChronoDuration::seconds(token_resp.expires_in.max(0))).to_rfc3339()
    });

    std::fs::create_dir_all(&auth_dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, serde_json::to_vec_pretty(&out).unwrap()).map_err(|e| e.to_string())?;
    super::accounts::reload_state(state);
    Ok((path.to_string_lossy().to_string(), identity.label))
}

fn persist_refreshed_account(
    state: &crate::AppState,
    account: &QwenAccount,
    refreshed: &RefreshResult,
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
    let identity = identity_from_access_token(&refreshed.access_token, &account.label);

    if let serde_json::Value::Object(map) = &mut value {
        map.insert(
            "access_token".to_string(),
            serde_json::Value::String(refreshed.access_token.clone()),
        );
        map.insert(
            "expired".to_string(),
            serde_json::Value::String(refreshed.expired_at.clone()),
        );
        map.insert(
            "last_refresh".to_string(),
            serde_json::Value::String(Utc::now().to_rfc3339()),
        );
        map.insert(
            "email".to_string(),
            serde_json::Value::String(
                identity
                    .email
                    .clone()
                    .unwrap_or_else(|| identity.label.clone()),
            ),
        );
        map.insert(
            "label".to_string(),
            serde_json::Value::String(identity.label.clone()),
        );
        if let Some(subject) = identity.subject.clone() {
            map.insert("subject".to_string(), serde_json::Value::String(subject));
        }
        if let Some(refresh_token) = refreshed.refresh_token.as_ref() {
            map.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token.clone()),
            );
        }
        if let Some(resource_url) = refreshed.resource_url.as_ref() {
            map.insert(
                "resource_url".to_string(),
                serde_json::Value::String(resource_url.clone()),
            );
        }
    }

    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).map_err(|e| e.to_string())?;

    let mut accounts = state.qwen_accounts.lock().unwrap();
    if let Some(current) = accounts
        .iter_mut()
        .find(|current| current.file_name.as_deref() == Some(file_name.as_str()))
    {
        current.access_token = Some(refreshed.access_token.clone());
        current.expired_at = Some(refreshed.expired_at.clone());
        current.email = identity
            .email
            .clone()
            .unwrap_or_else(|| identity.label.clone());
        current.label = identity.label;
        current.subject = identity.subject;
        if let Some(refresh_token) = refreshed.refresh_token.as_ref() {
            current.refresh_token = refresh_token.clone();
        }
        if let Some(resource_url) = refreshed.resource_url.as_ref() {
            current.resource_url = Some(resource_url.clone());
        }
    }

    Ok(())
}

fn generate_code_verifier() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect()
}

fn code_challenge(code_verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
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

fn resource_to_base_url(resource_url: Option<&str>) -> String {
    let Some(resource_url) = resource_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return "https://portal.qwen.ai/v1".to_string();
    };

    if resource_url.starts_with("http://") || resource_url.starts_with("https://") {
        return resource_url.trim_end_matches('/').to_string();
    }

    if resource_url.contains('/') {
        return format!("https://{}", resource_url.trim_end_matches('/'));
    }

    format!("https://{}/v1", resource_url.trim_end_matches('/'))
}
