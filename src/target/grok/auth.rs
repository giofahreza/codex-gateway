use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Instant;

pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const ISSUER: &str = "https://auth.x.ai";
const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const REDIRECT_HOST: &str = "http://localhost:56121";
const REDIRECT_PATH: &str = "/callback";

#[derive(Clone)]
pub struct PendingOAuth {
    pub code_verifier: String,
    pub code_challenge: String,
    pub state_token: String,
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
        Self {
            code_verifier,
            code_challenge,
            state_token,
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
            ("plan", "generic"),
            ("referrer", "grokcli"),
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
        ("redirect_uri", &format!("{}{}", REDIRECT_HOST, REDIRECT_PATH)),
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

    let mut token: GrokTokenResponse =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse refreshed token: {}", e))?;

    if token.refresh_token.is_none() || token.refresh_token.as_deref() == Some("") {
        token.refresh_token = Some(refresh_token.to_string());
    }

    Ok(token)
}

pub fn save_auth(
    cfg: &crate::Config,
    token: &GrokTokenResponse,
    label: Option<&str>,
    email: Option<&str>,
) -> Result<String, String> {
    let auth_dir = cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());

    std::fs::create_dir_all(&auth_dir).map_err(|e| format!("mkdir: {}", e))?;

    let display_label = label.unwrap_or("grok");
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
        "email": email,
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "token_type": token.token_type.as_deref().unwrap_or("Bearer"),
        "expires_in": token.expires_in,
        "expires_at": if expires_at.is_empty() { serde_json::Value::Null } else { serde_json::json!(expires_at) },
        "scopes": SCOPE,
        "saved_at": chrono::Utc::now().to_rfc3339()
    });

    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&out).map_err(|e| format!("serialize: {}", e))?,
    )
    .map_err(|e| format!("write auth file: {}", e))?;

    Ok(path.to_string_lossy().to_string())
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

fn base64_url_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}
