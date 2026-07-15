use rand::Rng;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const ISSUER: &str = "https://auth.x.ai";
const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const USERINFO_URL: &str = "https://auth.x.ai/oauth2/userinfo";
const ME_URL: &str = "https://api.x.ai/v1/me";
const MODELS_URL: &str = "https://api.x.ai/v1/models";
const API_BASE_URL: &str = "https://api.x.ai/v1";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const REDIRECT_HOST: &str = "http://127.0.0.1:56121";
const REDIRECT_PATH: &str = "/callback";
const REFERRER: &str = "hermes-agent";

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GrokProfile {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub given_name: Option<String>,
    #[serde(default)]
    pub family_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_blocked: Option<bool>,
    #[serde(default)]
    pub zdr_status: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GrokModelInfo {
    pub model_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub owned_by: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GrokRateLimitInfo {
    pub label: String,
    pub scope: String,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub remaining: Option<f64>,
    #[serde(default)]
    pub used: Option<f64>,
    #[serde(default)]
    pub used_percent: Option<f64>,
    #[serde(default)]
    pub remaining_percent: Option<f64>,
    #[serde(default)]
    pub limit_text: String,
    #[serde(default)]
    pub remaining_text: String,
    #[serde(default)]
    pub used_text: String,
    #[serde(default)]
    pub reset_label: String,
}

#[derive(Clone)]
pub struct PendingOAuth {
    pub code_verifier: String,
    pub code_challenge: String,
    pub state_token: String,
    pub nonce: String,
    #[allow(dead_code)]
    pub created_at: Instant,
}

impl PendingOAuth {
    fn generate_code_verifier() -> String {
        let s: String = rand::rng()
            .sample_iter(&rand::distr::Alphanumeric)
            .take(96)
            .map(char::from)
            .collect();
        s
    }

    fn code_challenge_from_verifier(verifier: &str) -> String {
        let hash = Sha256::digest(verifier.as_bytes());
        base64_url_encode(&hash)
    }

    pub fn new() -> Self {
        let code_verifier = Self::generate_code_verifier();
        let code_challenge = Self::code_challenge_from_verifier(&code_verifier);
        let state_token = uuid::Uuid::new_v4().simple().to_string();
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        Self {
            code_verifier,
            code_challenge,
            state_token,
            nonce,
            created_at: Instant::now(),
        }
    }

    pub fn build_authorize_url(&self) -> String {
        let redirect_uri = format!("{}{}", REDIRECT_HOST, REDIRECT_PATH);
        let params: Vec<(&str, &str)> = vec![
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", &redirect_uri),
            ("scope", SCOPE),
            ("code_challenge", &self.code_challenge),
            ("code_challenge_method", "S256"),
            ("state", &self.state_token),
            ("nonce", &self.nonce),
            ("plan", "generic"),
            ("referrer", REFERRER),
        ];
        let qs: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{}?{}", AUTHORIZE_URL, qs)
    }
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    pending: &PendingOAuth,
) -> Result<GrokTokenResponse, String> {
    let body = serde_urlencoded::to_string([
        ("grant_type", "authorization_code"),
        ("code", code),
        (
            "redirect_uri",
            &format!("{}{}", REDIRECT_HOST, REDIRECT_PATH),
        ),
        ("client_id", CLIENT_ID),
        ("code_verifier", &pending.code_verifier),
        ("code_challenge", &pending.code_challenge),
        ("code_challenge_method", "S256"),
    ])
    .map_err(|e| format!("failed to encode token request: {}", e))?;

    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("token request failed: {}", e))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if status == 403 {
        return Err(
            "Your account does not have an active Grok subscription (SuperGrok or X Premium+)"
                .to_string(),
        );
    }

    if !status.is_success() {
        return Err(format!(
            "token exchange failed (HTTP {}): {}",
            status.as_u16(),
            text
        ));
    }

    let token: GrokTokenResponse =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse token: {}", e))?;

    if token.access_token.is_empty() {
        return Err("token response missing access_token".to_string());
    }

    Ok(token)
}

pub async fn refresh_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<GrokTokenResponse, String> {
    let body = serde_urlencoded::to_string([
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", refresh_token),
    ])
    .map_err(|e| format!("failed to encode refresh request: {}", e))?;

    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("refresh request failed: {}", e))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(format!(
            "token refresh failed (HTTP {}): {}",
            status.as_u16(),
            text
        ));
    }

    let mut token: GrokTokenResponse = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse refreshed token: {}", e))?;

    if token.refresh_token.is_none() || token.refresh_token.as_deref() == Some("") {
        token.refresh_token = Some(refresh_token.to_string());
    }

    Ok(token)
}

pub async fn fetch_profile(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<GrokProfile, String> {
    let mut profile = GrokProfile::default();
    let mut errors = Vec::new();

    match client
        .get(USERINFO_URL)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let value = resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| format!("failed to parse Grok userinfo: {}", e))?;
            merge_profile(
                &mut profile,
                GrokProfile {
                    name: value
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    given_name: value
                        .get("given_name")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    family_name: value
                        .get("family_name")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    email: value
                        .get("email")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    email_verified: value.get("email_verified").and_then(|v| v.as_bool()),
                    user_id: value
                        .get("sub")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    ..GrokProfile::default()
                },
            );
        }
        Ok(resp) => errors.push(format!("userinfo returned HTTP {}", resp.status().as_u16())),
        Err(err) => errors.push(format!("userinfo request failed: {}", err)),
    }

    match client
        .get(ME_URL)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let value = resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| format!("failed to parse Grok /me response: {}", e))?;
            merge_profile(
                &mut profile,
                GrokProfile {
                    user_id: value
                        .get("user_id")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    team_id: value
                        .get("team_id")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    team_blocked: value.get("team_blocked").and_then(|v| v.as_bool()),
                    zdr_status: value
                        .get("zdr_status")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    ..GrokProfile::default()
                },
            );
        }
        Ok(resp) => errors.push(format!("/v1/me returned HTTP {}", resp.status().as_u16())),
        Err(err) => errors.push(format!("/v1/me request failed: {}", err)),
    }

    if profile.email.is_none() && profile.name.is_none() && profile.user_id.is_none() {
        return Err(errors.join("; "));
    }

    Ok(profile)
}

pub async fn fetch_models(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<Vec<GrokModelInfo>, String> {
    let resp = client
        .get(MODELS_URL)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Grok models request failed: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "Grok models request failed (HTTP {})",
            status.as_u16()
        ));
    }
    let value = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("failed to parse Grok models response: {}", e))?;
    Ok(value
        .get("data")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let model_id = item.get("id").and_then(|v| v.as_str())?.trim().to_string();
            if model_id.is_empty() {
                return None;
            }
            let aliases = item
                .get("aliases")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|alias| alias.as_str().map(|v| v.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(GrokModelInfo {
                display_name: aliases
                    .iter()
                    .find(|alias| alias.eq_ignore_ascii_case("grok-latest"))
                    .cloned()
                    .unwrap_or_else(|| model_id.clone()),
                model_id,
                owned_by: item
                    .get("owned_by")
                    .and_then(|v| v.as_str())
                    .unwrap_or("xai")
                    .to_string(),
                aliases,
                context_window: item.get("context_length").and_then(|v| v.as_u64()),
            })
        })
        .collect())
}

pub fn profile_from_id_token(id_token: &str) -> Option<GrokProfile> {
    let payload = id_token.split('.').nth(1)?;
    let padded = match payload.len() % 4 {
        2 => format!("{}==", payload),
        3 => format!("{}=", payload),
        _ => payload.to_string(),
    };
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded.as_bytes())
        .ok()?;
    let claims = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    Some(GrokProfile {
        name: claims
            .get("name")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        given_name: claims
            .get("given_name")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        family_name: claims
            .get("family_name")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        email: claims
            .get("email")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        email_verified: claims.get("email_verified").and_then(|v| v.as_bool()),
        user_id: claims
            .get("sub")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        ..GrokProfile::default()
    })
}

pub fn merge_profile(target: &mut GrokProfile, source: GrokProfile) {
    if target.name.is_none() {
        target.name = source.name;
    }
    if target.given_name.is_none() {
        target.given_name = source.given_name;
    }
    if target.family_name.is_none() {
        target.family_name = source.family_name;
    }
    if target.email.is_none() {
        target.email = source.email;
    }
    if target.email_verified.is_none() {
        target.email_verified = source.email_verified;
    }
    if target.user_id.is_none() {
        target.user_id = source.user_id;
    }
    if target.team_id.is_none() {
        target.team_id = source.team_id;
    }
    if target.team_blocked.is_none() {
        target.team_blocked = source.team_blocked;
    }
    if target.zdr_status.is_none() {
        target.zdr_status = source.zdr_status;
    }
}

pub fn save_auth(
    cfg: &crate::Config,
    token: &GrokTokenResponse,
    profile: Option<&GrokProfile>,
    models: &[GrokModelInfo],
) -> Result<String, String> {
    let auth_dir = cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());

    std::fs::create_dir_all(&auth_dir).map_err(|e| format!("mkdir: {}", e))?;

    let display_label = profile
        .and_then(|profile| profile.name.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            profile
                .and_then(|profile| profile.email.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("grok");
    let sanitized = display_label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();

    let email = profile.and_then(|profile| profile.email.as_deref());

    let email_sanitized = email
        .unwrap_or("unknown")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '@' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();

    let file_name = format!("grok-{}-{}.json", email_sanitized, sanitized);
    let path = std::path::Path::new(&auth_dir).join(&file_name);

    remove_duplicate_auth_files(
        &auth_dir,
        &path,
        email,
        profile.and_then(|profile| profile.user_id.as_deref()),
    )?;

    let expires_at = if token.expires_in > 0 {
        let ts = chrono::Utc::now().timestamp() + token.expires_in as i64;
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let out = serde_json::json!({
        "type": "grok",
        "label": display_label,
        "name": profile.and_then(|profile| profile.name.as_deref()),
        "email": email,
        "email_verified": profile.and_then(|profile| profile.email_verified),
        "user_id": profile.and_then(|profile| profile.user_id.as_deref()),
        "team_id": profile.and_then(|profile| profile.team_id.as_deref()),
        "team_blocked": profile.and_then(|profile| profile.team_blocked),
        "zdr_status": profile.and_then(|profile| profile.zdr_status.as_deref()),
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "token_type": token.token_type.as_deref().unwrap_or("Bearer"),
        "expires_in": token.expires_in,
        "expires_at": if expires_at.is_empty() { serde_json::Value::Null } else { serde_json::json!(expires_at) },
        "scopes": SCOPE,
        "api_base_url": API_BASE_URL,
        "models": models,
        "rate_limits": Vec::<GrokRateLimitInfo>::new(),
        "last_effective_model": serde_json::Value::Null,
        "saved_at": chrono::Utc::now().to_rfc3339()
    });

    super::super::atomic_write_json(&path, &out)
        .map_err(|err| format!("write auth file: {}", err))?;

    Ok(path.to_string_lossy().to_string())
}

pub fn extract_rate_limits(headers: &HeaderMap) -> Vec<GrokRateLimitInfo> {
    ["requests", "tokens"]
        .iter()
        .filter_map(|scope| {
            let limit = header_f64(headers, &format!("x-ratelimit-limit-{}", scope));
            let remaining = header_f64(headers, &format!("x-ratelimit-remaining-{}", scope));
            if limit.is_none() && remaining.is_none() {
                return None;
            }
            let used = match (limit, remaining) {
                (Some(limit), Some(remaining)) => Some((limit - remaining).max(0.0)),
                _ => None,
            };
            let used_percent = match (used, limit) {
                (Some(used), Some(limit)) if limit > 0.0 => Some((used / limit) * 100.0),
                _ => None,
            };
            let remaining_percent = match (remaining, limit) {
                (Some(remaining), Some(limit)) if limit > 0.0 => Some((remaining / limit) * 100.0),
                _ => None,
            };
            Some(GrokRateLimitInfo {
                label: if *scope == "requests" {
                    "Requests".to_string()
                } else {
                    "Tokens".to_string()
                },
                scope: (*scope).to_string(),
                limit,
                remaining,
                used,
                used_percent,
                remaining_percent,
                limit_text: limit.map(format_count).unwrap_or_else(|| "N/A".to_string()),
                remaining_text: remaining
                    .map(format_count)
                    .unwrap_or_else(|| "N/A".to_string()),
                used_text: used.map(format_count).unwrap_or_else(|| "N/A".to_string()),
                reset_label: header_string(headers, &format!("x-ratelimit-reset-{}", scope))
                    .unwrap_or_default(),
            })
        })
        .collect()
}

pub fn persist_runtime_metadata(
    cfg: &crate::Config,
    file_name: Option<&str>,
    effective_model: Option<&str>,
    rate_limits: &[GrokRateLimitInfo],
) -> Result<(), String> {
    if let Some(file_name) = file_name.filter(|value| !value.trim().is_empty()) {
        update_auth_file(cfg, file_name, |value| {
            if let Some(model) = effective_model
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                value["last_effective_model"] = serde_json::Value::String(model.to_string());
            }
            if !rate_limits.is_empty() {
                value["rate_limits"] =
                    serde_json::to_value(rate_limits).unwrap_or(serde_json::Value::Null);
            }
        })?;
    }
    Ok(())
}

pub async fn backfill_account_metadata(
    state: &crate::AppState,
    account: &super::accounts::GrokAccount,
) -> Result<bool, String> {
    let Some(file_name) = account
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(false);
    };

    let mut profile = profile_from_access_token_claims(&account.access_token).unwrap_or_default();
    if let Ok(fetched) = fetch_profile(&state.client, &account.access_token).await {
        merge_profile(&mut profile, fetched);
    }

    let models = fetch_models(&state.client, &account.access_token)
        .await
        .unwrap_or_else(|_| account.models.clone());

    let mut changed = false;
    if !profile_is_empty(&profile) {
        changed |= update_auth_file(&state.cfg, file_name, |value| {
            set_profile_fields(value, &profile);
        })?;
    }
    if !models.is_empty() {
        changed |= update_auth_file(&state.cfg, file_name, |value| {
            value["models"] = serde_json::to_value(&models).unwrap_or(serde_json::Value::Null);
        })?;
    }
    Ok(changed)
}

#[derive(Debug, Deserialize)]
pub struct GrokTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default)]
    pub id_token: Option<String>,
}

fn profile_from_access_token_claims(access_token: &str) -> Option<GrokProfile> {
    let payload = access_token.split('.').nth(1)?;
    let padded = match payload.len() % 4 {
        2 => format!("{}==", payload),
        3 => format!("{}=", payload),
        _ => payload.to_string(),
    };
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded.as_bytes())
        .ok()?;
    let claims = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    Some(GrokProfile {
        user_id: claims
            .get("principal_id")
            .or_else(|| claims.get("sub"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        team_id: claims
            .get("team_id")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        ..GrokProfile::default()
    })
}

fn profile_is_empty(profile: &GrokProfile) -> bool {
    profile.name.is_none()
        && profile.email.is_none()
        && profile.user_id.is_none()
        && profile.team_id.is_none()
        && profile.team_blocked.is_none()
        && profile.zdr_status.is_none()
}

fn set_profile_fields(value: &mut serde_json::Value, profile: &GrokProfile) {
    if let Some(name) = profile.name.as_deref() {
        value["label"] = serde_json::Value::String(name.to_string());
        value["name"] = serde_json::Value::String(name.to_string());
    }
    if let Some(email) = profile.email.as_deref() {
        value["email"] = serde_json::Value::String(email.to_string());
    }
    if let Some(email_verified) = profile.email_verified {
        value["email_verified"] = serde_json::Value::Bool(email_verified);
    }
    if let Some(user_id) = profile.user_id.as_deref() {
        value["user_id"] = serde_json::Value::String(user_id.to_string());
    }
    if let Some(team_id) = profile.team_id.as_deref() {
        value["team_id"] = serde_json::Value::String(team_id.to_string());
    }
    if let Some(team_blocked) = profile.team_blocked {
        value["team_blocked"] = serde_json::Value::Bool(team_blocked);
    }
    if let Some(zdr_status) = profile.zdr_status.as_deref() {
        value["zdr_status"] = serde_json::Value::String(zdr_status.to_string());
    }
}

fn update_auth_file(
    cfg: &crate::Config,
    file_name: &str,
    mut update: impl FnMut(&mut serde_json::Value),
) -> Result<bool, String> {
    let auth_dir = cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    let data = std::fs::read_to_string(&path).map_err(|e| format!("read auth file: {}", e))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("parse auth file: {}", e))?;
    let before = value.clone();
    update(&mut value);
    if value == before {
        return Ok(false);
    }
    super::super::atomic_write_json(&path, &value)
        .map_err(|err| format!("write auth file: {}", err))?;
    Ok(true)
}

fn remove_duplicate_auth_files(
    auth_dir: &str,
    new_path: &std::path::Path,
    email: Option<&str>,
    user_id: Option<&str>,
) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(auth_dir) else {
        return Ok(());
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path == new_path || path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("grok") {
            continue;
        }
        let matches_email = email.is_some_and(|expected| {
            value.get("email").and_then(|v| v.as_str()).map(str::trim) == Some(expected.trim())
        });
        let matches_user_id = user_id.is_some_and(|expected| {
            value.get("user_id").and_then(|v| v.as_str()).map(str::trim) == Some(expected.trim())
        });
        if matches_email || matches_user_id {
            std::fs::remove_file(&path).map_err(|e| {
                format!("remove duplicate Grok auth file {}: {}", path.display(), e)
            })?;
        }
    }
    Ok(())
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    header_string(headers, name).and_then(|value| value.parse::<f64>().ok())
}

fn format_count(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{}", value as u64)
    } else {
        format!("{:.2}", value)
    }
}

fn base64_url_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn grok_authorize_url_matches_current_loopback_shape() {
        let pending = PendingOAuth::new();
        let url = url::Url::parse(&pending.build_authorize_url()).expect("valid authorize url");
        let params = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(url.as_str().split('?').next(), Some(AUTHORIZE_URL));
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:56121/callback")
        );
        assert_eq!(params.get("referrer").map(String::as_str), Some(REFERRER));
        assert_eq!(
            params.get("nonce").map(String::as_str),
            Some(pending.nonce.as_str())
        );
        assert_eq!(
            params.get("state").map(String::as_str),
            Some(pending.state_token.as_str())
        );
    }

    #[test]
    fn extracts_request_and_token_rate_limits() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ratelimit-limit-requests",
            HeaderValue::from_static("120"),
        );
        headers.insert(
            "x-ratelimit-remaining-requests",
            HeaderValue::from_static("117"),
        );
        headers.insert(
            "x-ratelimit-limit-tokens",
            HeaderValue::from_static("5000000"),
        );
        headers.insert(
            "x-ratelimit-remaining-tokens",
            HeaderValue::from_static("4999500"),
        );

        let limits = extract_rate_limits(&headers);
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].scope, "requests");
        assert_eq!(limits[0].used_text, "3");
        assert_eq!(limits[1].scope, "tokens");
        assert_eq!(limits[1].used_text, "500");
    }
}
