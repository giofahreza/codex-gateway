use axum::http::{header, HeaderMap, HeaderValue};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const ADMIN_SESSION_COOKIE: &str = "codex_gateway_admin_session";
const TOTP_STEP_SECONDS: u64 = 30;
const TOTP_WINDOW_STEPS: i64 = 1;
const TOTP_DIGITS: u32 = 6;
const DEFAULT_SESSION_TTL_SECONDS: u64 = 12 * 60 * 60;
const MIN_SESSION_TTL_SECONDS: u64 = 300;
const MAX_SESSION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const LOGIN_FAILURE_WINDOW_SECONDS: u64 = 5 * 60;
const LOGIN_FAILURE_THRESHOLD: usize = 3;
const LOGIN_BASE_LOCKOUT_SECONDS: u64 = 5 * 60;
const LOGIN_MAX_BACKOFF_SHIFT: u32 = 10;

type HmacSha1 = Hmac<Sha1>;

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct AdminAuthConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub totp_secret: Option<String>,
    #[serde(default)]
    pub session_ttl_seconds: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AdminSession {
    pub expires_at_unix: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LoginAttemptState {
    recent_failures_unix: VecDeque<u64>,
    lockout_level: u32,
    locked_until_unix: Option<u64>,
}

#[derive(Deserialize)]
pub(crate) struct LoginForm {
    pub otp: String,
}

pub(crate) fn apply_env_overrides(cfg: &mut AdminAuthConfig) {
    if let Some(value) = env_value(&["ADMIN_AUTH_API_KEY", "ADMIN_API_KEY"]) {
        cfg.api_key = Some(value);
    }
    if let Some(value) = env_value(&["ADMIN_AUTH_TOTP_SECRET", "ADMIN_TOTP_SECRET"]) {
        cfg.totp_secret = Some(value);
    }
    if let Some(value) = env_value(&["ADMIN_AUTH_ENABLED"]) {
        cfg.enabled = parse_bool(&value).unwrap_or(cfg.enabled);
    }
    if let Some(value) = env_value(&["ADMIN_AUTH_SESSION_TTL_SECONDS"]) {
        if let Ok(parsed) = value.parse::<u64>() {
            cfg.session_ttl_seconds = Some(parsed);
        }
    }
}

pub(crate) fn is_enabled(cfg: &AdminAuthConfig) -> bool {
    cfg.enabled
        || cfg
            .totp_secret
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
}

pub(crate) fn is_configured(cfg: &AdminAuthConfig) -> bool {
    cfg.totp_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
}

pub(crate) fn session_ttl_seconds(cfg: &AdminAuthConfig) -> u64 {
    cfg.session_ttl_seconds
        .unwrap_or(DEFAULT_SESSION_TTL_SECONDS)
        .clamp(MIN_SESSION_TTL_SECONDS, MAX_SESSION_TTL_SECONDS)
}

pub(crate) fn verify_login(
    cfg: &AdminAuthConfig,
    otp: &str,
    now: SystemTime,
) -> Result<(), String> {
    if !is_enabled(cfg) {
        return Err("admin login is not enabled".to_string());
    }
    if !is_configured(cfg) {
        return Err(
            "admin login is not configured: set admin_auth.totp_secret or ADMIN_AUTH_TOTP_SECRET"
                .to_string(),
        );
    }
    let secret = cfg
        .totp_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "admin login is not configured: set admin_auth.totp_secret or ADMIN_AUTH_TOTP_SECRET"
                .to_string()
        })?;
    if !verify_totp(secret, otp, now) {
        return Err("invalid OTP".to_string());
    }
    Ok(())
}

pub(crate) fn login_client_key(headers: &HeaderMap) -> String {
    let forwarded_ip = [
        "cf-connecting-ip",
        "true-client-ip",
        "x-real-ip",
        "x-forwarded-for",
    ]
    .iter()
    .find_map(|header_name| {
        headers.get(*header_name).and_then(|value| {
            value.to_str().ok().and_then(|raw| {
                raw.split(',')
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        })
    });
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");

    match forwarded_ip {
        Some(ip) => format!("ip:{}|ua:{}", ip, user_agent),
        None => format!("ua:{}", user_agent),
    }
}

pub(crate) fn current_lockout_message(
    attempts: &mut HashMap<String, LoginAttemptState>,
    client_key: &str,
    now: SystemTime,
) -> Option<String> {
    let now_unix = unix_seconds(now);
    let Some(state) = attempts.get_mut(client_key) else {
        return None;
    };
    prune_attempt_state(state, now_unix);
    let locked_until = state.locked_until_unix?;
    if locked_until <= now_unix {
        state.locked_until_unix = None;
        prune_attempt_state(state, now_unix);
        return None;
    }
    Some(lockout_message(locked_until.saturating_sub(now_unix)))
}

pub(crate) fn record_failed_login(
    attempts: &mut HashMap<String, LoginAttemptState>,
    client_key: &str,
    now: SystemTime,
) -> Option<String> {
    let now_unix = unix_seconds(now);
    let state = attempts.entry(client_key.to_string()).or_default();
    prune_attempt_state(state, now_unix);
    if let Some(locked_until) = state.locked_until_unix {
        if locked_until > now_unix {
            return Some(lockout_message(locked_until.saturating_sub(now_unix)));
        }
        state.locked_until_unix = None;
    }
    state.recent_failures_unix.push_back(now_unix);
    prune_attempt_state(state, now_unix);
    if state.recent_failures_unix.len() < LOGIN_FAILURE_THRESHOLD {
        return None;
    }
    state.recent_failures_unix.clear();
    let shift = state.lockout_level.min(LOGIN_MAX_BACKOFF_SHIFT);
    let lockout_seconds = LOGIN_BASE_LOCKOUT_SECONDS.saturating_mul(1u64 << shift);
    state.lockout_level = state.lockout_level.saturating_add(1);
    state.locked_until_unix = Some(now_unix.saturating_add(lockout_seconds));
    Some(lockout_message(lockout_seconds))
}

pub(crate) fn clear_login_attempts(
    attempts: &mut HashMap<String, LoginAttemptState>,
    client_key: &str,
) {
    attempts.remove(client_key);
}

pub(crate) fn create_session(
    sessions: &mut HashMap<String, AdminSession>,
    ttl_seconds: u64,
) -> String {
    prune_expired_sessions(sessions);
    let session_id = Uuid::new_v4().simple().to_string();
    let expires_at_unix = now_unix_seconds().saturating_add(ttl_seconds);
    sessions.insert(session_id.clone(), AdminSession { expires_at_unix });
    session_id
}

pub(crate) fn validate_session(
    headers: &HeaderMap,
    sessions: &mut HashMap<String, AdminSession>,
) -> bool {
    prune_expired_sessions(sessions);
    let Some(session_id) = read_cookie_value(headers, ADMIN_SESSION_COOKIE) else {
        return false;
    };
    match sessions.get(&session_id) {
        Some(session) if session.expires_at_unix > now_unix_seconds() => true,
        _ => {
            sessions.remove(&session_id);
            false
        }
    }
}

pub(crate) fn remove_session(headers: &HeaderMap, sessions: &mut HashMap<String, AdminSession>) {
    if let Some(session_id) = read_cookie_value(headers, ADMIN_SESSION_COOKIE) {
        sessions.remove(&session_id);
    }
    prune_expired_sessions(sessions);
}

pub(crate) fn build_session_cookie(session_id: &str, ttl_seconds: u64) -> String {
    format!(
        "{}={}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax",
        ADMIN_SESSION_COOKIE, session_id, ttl_seconds
    )
}

pub(crate) fn clear_session_cookie() -> String {
    format!(
        "{}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
        ADMIN_SESSION_COOKIE
    )
}

pub(crate) fn append_set_cookie(headers: &mut axum::http::HeaderMap, cookie: &str) {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        headers.append(header::SET_COOKIE, value);
    }
}

pub(crate) fn load_sessions(path: &Path) -> HashMap<String, AdminSession> {
    let Ok(data) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(mut sessions) = serde_json::from_str::<HashMap<String, AdminSession>>(&data) else {
        return HashMap::new();
    };
    prune_expired_sessions(&mut sessions);
    sessions
}

pub(crate) fn save_sessions(path: &Path, sessions: &HashMap<String, AdminSession>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(data) = serde_json::to_vec_pretty(sessions) else {
        return;
    };
    let tmp_path = path.with_extension("json.tmp");
    if std::fs::write(&tmp_path, data).is_ok() && std::fs::rename(&tmp_path, path).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    } else {
        let _ = std::fs::remove_file(&tmp_path);
    }
}

fn verify_totp(secret: &str, otp: &str, now: SystemTime) -> bool {
    let otp = otp.trim();
    if otp.len() != TOTP_DIGITS as usize || !otp.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some(secret_bytes) = decode_totp_secret(secret) else {
        return false;
    };
    let Ok(duration) = now.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let step = duration.as_secs() / TOTP_STEP_SECONDS;
    for delta in -TOTP_WINDOW_STEPS..=TOTP_WINDOW_STEPS {
        let candidate_step = step as i64 + delta;
        if candidate_step < 0 {
            continue;
        }
        if timing_safe_eq(otp, &hotp(&secret_bytes, candidate_step as u64)) {
            return true;
        }
    }
    false
}

fn hotp(secret: &[u8], counter: u64) -> String {
    let mut mac = HmacSha1::new_from_slice(secret).expect("valid hmac key length");
    mac.update(&counter.to_be_bytes());
    let result = mac.finalize().into_bytes();
    let offset = (result[19] & 0x0f) as usize;
    let binary = ((u32::from(result[offset]) & 0x7f) << 24)
        | (u32::from(result[offset + 1]) << 16)
        | (u32::from(result[offset + 2]) << 8)
        | u32::from(result[offset + 3]);
    format!("{:06}", binary % 10u32.pow(TOTP_DIGITS))
}

fn decode_totp_secret(secret: &str) -> Option<Vec<u8>> {
    let normalized = secret
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '-' | '='))
        .flat_map(|ch| ch.to_uppercase())
        .collect::<String>();
    if normalized.is_empty() {
        return None;
    }
    BASE32_NOPAD.decode(normalized.as_bytes()).ok()
}

fn read_cookie_value(headers: &HeaderMap, target_name: &str) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|cookie| {
        let (name, value) = cookie.trim().split_once('=')?;
        if name.trim() == target_name {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

fn prune_expired_sessions(sessions: &mut HashMap<String, AdminSession>) {
    let now = now_unix_seconds();
    sessions.retain(|_, session| session.expires_at_unix > now);
}

fn prune_attempt_state(state: &mut LoginAttemptState, now_unix: u64) {
    while let Some(oldest) = state.recent_failures_unix.front().copied() {
        if now_unix.saturating_sub(oldest) > LOGIN_FAILURE_WINDOW_SECONDS {
            state.recent_failures_unix.pop_front();
        } else {
            break;
        }
    }
    if state
        .locked_until_unix
        .is_some_and(|locked_until| locked_until <= now_unix)
    {
        state.locked_until_unix = None;
    }
}

fn now_unix_seconds() -> u64 {
    unix_seconds(SystemTime::now())
}

fn unix_seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn timing_safe_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for idx in 0..max_len {
        diff |= usize::from(*left.get(idx).unwrap_or(&0) ^ *right.get(idx).unwrap_or(&0));
    }
    diff == 0
}

fn env_value(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
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

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn lockout_message(remaining_seconds: u64) -> String {
    format!(
        "Too many failed login attempts. Try again in {}.",
        human_duration(remaining_seconds)
    )
}

fn human_duration(seconds: u64) -> String {
    let rounded = seconds.max(1);
    let hours = rounded / 3600;
    let minutes = (rounded % 3600) / 60;
    let secs = rounded % 60;
    if hours > 0 {
        if minutes > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}h", hours)
        }
    } else if minutes > 1 {
        if secs > 0 {
            format!("{}m {}s", minutes, secs)
        } else {
            format!("{} minutes", minutes)
        }
    } else if minutes == 1 {
        if secs > 0 {
            format!("1m {}s", secs)
        } else {
            "1 minute".to_string()
        }
    } else {
        format!("{}s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_login_accepts_rfc_totp_vector() {
        let cfg = AdminAuthConfig {
            enabled: true,
            api_key: Some("admin-key".to_string()),
            totp_secret: Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_string()),
            session_ttl_seconds: None,
        };

        let now = UNIX_EPOCH + std::time::Duration::from_secs(59);
        assert!(verify_login(&cfg, "287082", now).is_ok());
    }

    #[test]
    fn verify_login_rejects_wrong_totp() {
        let cfg = AdminAuthConfig {
            enabled: true,
            api_key: Some("admin-key".to_string()),
            totp_secret: Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_string()),
            session_ttl_seconds: None,
        };

        let now = UNIX_EPOCH + std::time::Duration::from_secs(59);
        assert!(verify_login(&cfg, "000000", now).is_err());
    }

    #[test]
    fn session_cookie_round_trip_validates_and_clears() {
        let mut sessions = HashMap::new();
        let session_id = create_session(&mut sessions, 600);
        let cookie = build_session_cookie(&session_id, 600);
        let cookie_pair = cookie.split(';').next().unwrap_or_default();

        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(cookie_pair).unwrap());

        assert!(validate_session(&headers, &mut sessions));
        remove_session(&headers, &mut sessions);
        assert!(!validate_session(&headers, &mut sessions));
    }

    #[test]
    fn failed_login_lockout_escalates() {
        let mut attempts = HashMap::new();
        let client_key = "ip:127.0.0.1|ua:test";

        assert!(record_failed_login(&mut attempts, client_key, UNIX_EPOCH).is_none());
        assert!(record_failed_login(
            &mut attempts,
            client_key,
            UNIX_EPOCH + std::time::Duration::from_secs(60)
        )
        .is_none());
        let first_lockout = record_failed_login(
            &mut attempts,
            client_key,
            UNIX_EPOCH + std::time::Duration::from_secs(120),
        )
        .unwrap();
        assert!(first_lockout.contains("5 minutes"));
        assert!(current_lockout_message(
            &mut attempts,
            client_key,
            UNIX_EPOCH + std::time::Duration::from_secs(121)
        )
        .is_some());
        assert!(current_lockout_message(
            &mut attempts,
            client_key,
            UNIX_EPOCH + std::time::Duration::from_secs(421)
        )
        .is_none());

        assert!(record_failed_login(
            &mut attempts,
            client_key,
            UNIX_EPOCH + std::time::Duration::from_secs(430)
        )
        .is_none());
        assert!(record_failed_login(
            &mut attempts,
            client_key,
            UNIX_EPOCH + std::time::Duration::from_secs(460)
        )
        .is_none());
        let second_lockout = record_failed_login(
            &mut attempts,
            client_key,
            UNIX_EPOCH + std::time::Duration::from_secs(490),
        )
        .unwrap();
        assert!(second_lockout.contains("10 minutes"));
    }
}
