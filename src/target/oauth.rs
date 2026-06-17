use axum::{
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::borrow::Cow;

const OAUTH_STATE_COOKIE_MAX_AGE_SECONDS: u64 = 900;

const CODEX_SCOPES: &[&str] = &["openid", "email", "profile", "offline_access"];
const ANTIGRAVITY_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];
const GEMINI_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
];
const QWEN_SCOPES: &[&str] = &["openid", "profile", "email", "model.completion"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthProvider {
    Codex,
    Antigravity,
    Gemini,
    Qwen,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct OAuthConfig {
    #[serde(default)]
    pub providers: OAuthProvidersConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct OAuthProvidersConfig {
    #[serde(default)]
    pub codex: OAuthProviderOverride,
    #[serde(default)]
    pub antigravity: OAuthProviderOverride,
    #[serde(default)]
    pub gemini: OAuthProviderOverride,
    #[serde(default)]
    pub qwen: OAuthProviderOverride,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct OAuthProviderOverride {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
    pub authorize_url: Option<String>,
    pub token_url: Option<String>,
    pub device_code_url: Option<String>,
    pub validate_url: Option<String>,
    pub refresh_url: Option<String>,
    pub session_url: Option<String>,
    pub base_url: Option<String>,
    pub scopes: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct OAuthProviderConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
    pub authorize_url: Option<String>,
    pub token_url: Option<String>,
    pub device_code_url: Option<String>,
    pub validate_url: Option<String>,
    pub refresh_url: Option<String>,
    pub session_url: Option<String>,
    pub base_url: Option<String>,
    pub scopes: Vec<String>,
}

impl OAuthProvider {
    fn env_prefix(self) -> &'static str {
        match self {
            Self::Codex => "CODEX_OAUTH",
            Self::Antigravity => "ANTIGRAVITY_OAUTH",
            Self::Gemini => "GEMINI_OAUTH",
            Self::Qwen => "QWEN_OAUTH",
        }
    }

    pub fn route_segment(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::Gemini => "gemini",
            Self::Qwen => "qwen",
        }
    }

    pub fn from_route_segment(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "antigravity" => Some(Self::Antigravity),
            "gemini" => Some(Self::Gemini),
            "qwen" => Some(Self::Qwen),
            _ => None,
        }
    }
}

pub fn build_state_cookie(provider: OAuthProvider, state_token: &str) -> String {
    format!(
        "{}={}; Max-Age={}; Path={}; HttpOnly; SameSite=Lax",
        state_cookie_name(provider),
        state_token.trim(),
        OAUTH_STATE_COOKIE_MAX_AGE_SECONDS,
        state_cookie_path(provider),
    )
}

pub fn clear_state_cookie(provider: OAuthProvider) -> String {
    format!(
        "{}=; Max-Age=0; Path={}; HttpOnly; SameSite=Lax",
        state_cookie_name(provider),
        state_cookie_path(provider),
    )
}

pub fn validate_state_cookie(
    headers: &HeaderMap,
    provider: OAuthProvider,
    expected_state: &str,
) -> Result<(), String> {
    let cookie_name = state_cookie_name(provider);
    let cookie_state = read_cookie_value(headers, cookie_name).ok_or_else(|| {
        format!(
            "missing {} state cookie; start login again",
            provider.route_segment()
        )
    })?;
    if cookie_state.trim() != expected_state.trim() {
        return Err(format!(
            "{} state cookie mismatch; start login again",
            provider.route_segment()
        ));
    }
    Ok(())
}

pub async fn login_callback_route(
    state: crate::AppState,
    provider_slug: String,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(provider) = OAuthProvider::from_route_segment(&provider_slug) else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            format!("unknown oauth provider '{}'", provider_slug),
        )
            .into_response();
    };

    match provider {
        OAuthProvider::Qwen => {
            super::qwen::admin::login_callback_from_uri(state, method, headers, uri).await
        }
        _ => (
            axum::http::StatusCode::NOT_FOUND,
            format!(
                "{} OAuth callback is not handled by the gateway callback route; use /login/{}/submit instead",
                provider.route_segment(),
                provider.route_segment()
            ),
        )
            .into_response(),
    }
}

pub fn provider_config(
    cfg: Option<&crate::Config>,
    provider: OAuthProvider,
) -> OAuthProviderConfig {
    let mut resolved = default_provider_config(provider);
    if let Some(cfg) = cfg {
        apply_override(&mut resolved, provider_override(cfg, provider));
    }
    apply_env_overrides(&mut resolved, provider);
    resolved
}

fn provider_override<'a>(
    cfg: &'a crate::Config,
    provider: OAuthProvider,
) -> &'a OAuthProviderOverride {
    match provider {
        OAuthProvider::Codex => &cfg.oauth.providers.codex,
        OAuthProvider::Antigravity => &cfg.oauth.providers.antigravity,
        OAuthProvider::Gemini => &cfg.oauth.providers.gemini,
        OAuthProvider::Qwen => &cfg.oauth.providers.qwen,
    }
}

fn default_provider_config(provider: OAuthProvider) -> OAuthProviderConfig {
    match provider {
        OAuthProvider::Codex => OAuthProviderConfig {
            client_id: Some("app_EMoamEEZ73f0CkXaXp7hrann".to_string()),
            client_secret: None,
            redirect_uri: Some("http://localhost:1455/auth/callback".to_string()),
            authorize_url: Some("https://auth.openai.com/oauth/authorize".to_string()),
            token_url: Some("https://auth.openai.com/oauth/token".to_string()),
            device_code_url: None,
            validate_url: None,
            refresh_url: None,
            session_url: None,
            base_url: None,
            scopes: CODEX_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect(),
        },
        OAuthProvider::Antigravity => OAuthProviderConfig {
            client_id: None,
            client_secret: None,
            redirect_uri: Some("http://localhost:51121/oauth-callback".to_string()),
            authorize_url: Some("https://accounts.google.com/o/oauth2/v2/auth".to_string()),
            token_url: Some("https://oauth2.googleapis.com/token".to_string()),
            device_code_url: None,
            validate_url: None,
            refresh_url: None,
            session_url: None,
            base_url: Some("https://cloudcode-pa.googleapis.com".to_string()),
            scopes: ANTIGRAVITY_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect(),
        },
        OAuthProvider::Gemini => OAuthProviderConfig {
            client_id: None,
            client_secret: None,
            redirect_uri: Some("http://localhost:8085/oauth2callback".to_string()),
            authorize_url: Some("https://accounts.google.com/o/oauth2/v2/auth".to_string()),
            token_url: Some("https://oauth2.googleapis.com/token".to_string()),
            device_code_url: None,
            validate_url: None,
            refresh_url: None,
            session_url: None,
            base_url: Some("https://cloudcode-pa.googleapis.com".to_string()),
            scopes: GEMINI_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect(),
        },
        OAuthProvider::Qwen => OAuthProviderConfig {
            client_id: Some("f0304373b74a44d2b584a3fb70ca9e56".to_string()),
            client_secret: None,
            redirect_uri: None,
            authorize_url: Some("https://chat.qwen.ai/oauth/authorize".to_string()),
            token_url: Some("https://chat.qwen.ai/api/v1/oauth2/token".to_string()),
            device_code_url: Some("https://chat.qwen.ai/api/v1/oauth2/device/code".to_string()),
            validate_url: Some("https://chat.qwen.ai/api/v1/auths/".to_string()),
            refresh_url: Some("https://chat.qwen.ai/api/v1/auths/".to_string()),
            session_url: Some("https://chat.qwen.ai/api/v1/auths/".to_string()),
            base_url: Some("https://chat.qwen.ai/api/v1".to_string()),
            scopes: QWEN_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect(),
        },
    }
}

fn apply_override(resolved: &mut OAuthProviderConfig, override_cfg: &OAuthProviderOverride) {
    apply_string_override(&mut resolved.client_id, override_cfg.client_id.as_deref());
    apply_string_override(
        &mut resolved.client_secret,
        override_cfg.client_secret.as_deref(),
    );
    apply_string_override(
        &mut resolved.redirect_uri,
        override_cfg.redirect_uri.as_deref(),
    );
    apply_string_override(
        &mut resolved.authorize_url,
        override_cfg.authorize_url.as_deref(),
    );
    apply_string_override(&mut resolved.token_url, override_cfg.token_url.as_deref());
    apply_string_override(
        &mut resolved.device_code_url,
        override_cfg.device_code_url.as_deref(),
    );
    apply_string_override(
        &mut resolved.validate_url,
        override_cfg.validate_url.as_deref(),
    );
    apply_string_override(
        &mut resolved.refresh_url,
        override_cfg.refresh_url.as_deref(),
    );
    apply_string_override(
        &mut resolved.session_url,
        override_cfg.session_url.as_deref(),
    );
    apply_string_override(&mut resolved.base_url, override_cfg.base_url.as_deref());
    if let Some(scopes) = override_cfg.scopes.as_ref() {
        let parsed = normalize_scopes(scopes.iter().map(|value| value.as_str()).collect());
        if !parsed.is_empty() {
            resolved.scopes = parsed;
        }
    }
}

fn apply_env_overrides(resolved: &mut OAuthProviderConfig, provider: OAuthProvider) {
    let prefix = provider.env_prefix();
    apply_env_string_override(
        &mut resolved.client_id,
        &format!("{}_CLIENT_ID", prefix),
        provider_client_id_fallbacks(provider),
    );
    apply_env_string_override(
        &mut resolved.client_secret,
        &format!("{}_CLIENT_SECRET", prefix),
        provider_client_secret_fallbacks(provider),
    );
    apply_env_string_override(
        &mut resolved.redirect_uri,
        &format!("{}_REDIRECT_URI", prefix),
        &[],
    );
    apply_env_string_override(
        &mut resolved.authorize_url,
        &format!("{}_AUTHORIZE_URL", prefix),
        &[],
    );
    apply_env_string_override(
        &mut resolved.token_url,
        &format!("{}_TOKEN_URL", prefix),
        &[],
    );
    apply_env_string_override(
        &mut resolved.device_code_url,
        &format!("{}_DEVICE_CODE_URL", prefix),
        &[],
    );
    apply_env_string_override(
        &mut resolved.validate_url,
        &format!("{}_VALIDATE_URL", prefix),
        &[],
    );
    apply_env_string_override(
        &mut resolved.refresh_url,
        &format!("{}_REFRESH_URL", prefix),
        &[],
    );
    apply_env_string_override(
        &mut resolved.session_url,
        &format!("{}_SESSION_URL", prefix),
        &[],
    );
    apply_env_string_override(&mut resolved.base_url, &format!("{}_BASE_URL", prefix), &[]);

    if let Some(scopes) = env_value(&format!("{}_SCOPES", prefix), &[]) {
        let parsed = normalize_scopes(scopes.split(',').collect());
        if !parsed.is_empty() {
            resolved.scopes = parsed;
        }
    }
}

fn provider_client_id_fallbacks(provider: OAuthProvider) -> &'static [&'static str] {
    match provider {
        OAuthProvider::Antigravity => &["ANTIGRAVITY_GOOGLE_CLIENT_ID"],
        OAuthProvider::Gemini => &["GEMINI_GOOGLE_CLIENT_ID"],
        _ => &[],
    }
}

fn provider_client_secret_fallbacks(provider: OAuthProvider) -> &'static [&'static str] {
    match provider {
        OAuthProvider::Antigravity => &["ANTIGRAVITY_GOOGLE_CLIENT_SECRET"],
        OAuthProvider::Gemini => &["GEMINI_GOOGLE_CLIENT_SECRET"],
        _ => &[],
    }
}

fn apply_env_string_override(
    target: &mut Option<String>,
    primary_key: &str,
    fallback_keys: &[&str],
) {
    if let Some(value) = env_value(primary_key, fallback_keys) {
        *target = Some(value);
    }
}

fn apply_string_override(target: &mut Option<String>, override_value: Option<&str>) {
    let Some(value) = override_value.map(str::trim) else {
        return;
    };
    if !value.is_empty() {
        *target = Some(value.to_string());
    }
}

fn env_value(primary_key: &str, fallback_keys: &[&str]) -> Option<String> {
    std::iter::once(primary_key)
        .chain(fallback_keys.iter().copied())
        .find_map(|key| {
            std::env::var(key).ok().and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
        })
}

fn normalize_scopes(items: Vec<&str>) -> Vec<String> {
    items
        .into_iter()
        .flat_map(|value| value.split_whitespace())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect()
}

fn state_cookie_name(provider: OAuthProvider) -> &'static str {
    match provider {
        OAuthProvider::Codex => "codex_gateway_oauth_state_codex",
        OAuthProvider::Antigravity => "codex_gateway_oauth_state_antigravity",
        OAuthProvider::Gemini => "codex_gateway_oauth_state_gemini",
        OAuthProvider::Qwen => "codex_gateway_oauth_state_qwen",
    }
}

fn state_cookie_path(provider: OAuthProvider) -> Cow<'static, str> {
    Cow::Owned(format!("/login/{}", provider.route_segment()))
}

fn read_cookie_value(headers: &HeaderMap, target_name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| {
            let mut pieces = part.trim().splitn(2, '=');
            let name = pieces.next()?.trim();
            let value = pieces.next()?.trim();
            if name == target_name {
                Some(value.to_string())
            } else {
                None
            }
        })
        .next()
}
