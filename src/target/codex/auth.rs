use base64::Engine;
use chrono::Utc;
use rand::{distr::Alphanumeric, Rng};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct PendingOAuth {
    pub code_verifier: String,
    pub created_at: std::time::Instant,
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: i64,
    pub token_type: Option<String>,
}

pub fn build_auth_url() -> Result<(String, String, String), String> {
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

    let mut url =
        Url::parse("https://auth.openai.com/oauth/authorize").map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", "http://localhost:1455/auth/callback")
        .append_pair("scope", "openid email profile offline_access")
        .append_pair("state", &state_token)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "login")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true");

    Ok((url.to_string(), state_token, code_verifier))
}

pub fn parse_oauth_callback(redirect_url: &str) -> Result<(String, String), String> {
    let url = Url::parse(redirect_url).map_err(|_| "invalid redirect_url".to_string())?;
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let code = params.get("code").cloned().unwrap_or_default();
    let state = params.get("state").cloned().unwrap_or_default();
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
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"),
        ("code", code),
        ("redirect_uri", "http://localhost:1455/auth/callback"),
        ("code_verifier", code_verifier),
    ];
    let resp = client
        .post("https://auth.openai.com/oauth/token")
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

pub fn save_auth(state: &crate::AppState, token_resp: &TokenResponse) -> Result<String, String> {
    let id_token = &token_resp.id_token;
    let account_id = parse_jwt_account_id(id_token).unwrap_or_default();
    let email = parse_jwt_email(id_token).unwrap_or_else(|| "unknown".to_string());
    let now = Utc::now();
    let expires_at = now + chrono::Duration::seconds(token_resp.expires_in);

    let file_name = format!("codex-{}.json", sanitize_label(&email));
    let auth_dir = state
        .cfg
        .auth_dir
        .clone()
        .unwrap_or_else(|| "/root/dev/yow/gpt-gateway/auths".to_string());
    let path = std::path::Path::new(&auth_dir).join(file_name);
    std::fs::create_dir_all(&auth_dir).map_err(|e| e.to_string())?;
    let out = serde_json::json!({
        "id_token": token_resp.id_token,
        "access_token": token_resp.access_token,
        "refresh_token": token_resp.refresh_token,
        "account_id": account_id,
        "last_refresh": now.to_rfc3339(),
        "email": email,
        "type": "codex",
        "expired": expires_at.to_rfc3339()
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&out).unwrap()).map_err(|e| e.to_string())?;

    super::tokens::reload_state(state);
    Ok(path.to_string_lossy().to_string())
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

fn parse_jwt_email(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("email")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            v.get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
}

fn parse_jwt_account_id(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
}
