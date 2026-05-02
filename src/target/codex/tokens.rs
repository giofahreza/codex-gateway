use base64::Engine;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::time::Duration;

#[derive(Clone)]
pub struct UpstreamToken {
    pub token: String,
    pub account_id: Option<String>,
    pub label: String,
    pub file_name: Option<String>,
    pub enabled: bool,
    pub expired_at: Option<String>,
}

pub fn load_tokens(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<UpstreamToken> {
    let mut tokens: Vec<UpstreamToken> = cfg
        .tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.trim().to_string()))
        .filter(|(_, t)| !t.is_empty())
        .map(|(i, t)| UpstreamToken {
            token: t,
            account_id: None,
            label: format!("manual-{}", i + 1),
            file_name: None,
            enabled: true,
            expired_at: None,
        })
        .collect();

    if let Some(dir) = cfg.auth_dir.as_ref() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            files.sort_by_key(|e| e.path());
            for entry in files {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let data = match std::fs::read_to_string(&path) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let value: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(t) = value.get("type").and_then(|v| v.as_str()) {
                    if !t.eq_ignore_ascii_case("codex") {
                        continue;
                    }
                }
                let account_id = value
                    .get("account_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let label = value
                    .get("email")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("label").and_then(|v| v.as_str()))
                    .or_else(|| value.get("account_id").and_then(|v| v.as_str()))
                    .or_else(|| path.file_name().and_then(|s| s.to_str()))
                    .unwrap_or("codex-account")
                    .to_string();
                let expired_at = value
                    .get("id_token")
                    .and_then(|v| v.as_str())
                    .and_then(parse_jwt_subscription_until)
                    .or_else(|| {
                        value
                            .get("expired")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                if let Some(tok) = value
                    .get("access_token")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("api_key").and_then(|v| v.as_str()))
                {
                    let tok = tok.trim();
                    if !tok.is_empty() {
                        let file_name = path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string());
                        let enabled = file_name
                            .as_ref()
                            .map(|f| !disabled.contains(f))
                            .unwrap_or(true);
                        tokens.push(UpstreamToken {
                            token: tok.to_string(),
                            account_id,
                            label,
                            file_name,
                            enabled,
                            expired_at,
                        });
                    }
                }
            }
        }
    }

    let mut seen = HashSet::new();
    tokens.retain(|t| seen.insert(t.token.clone()));
    tokens
}

pub fn reload_state(state: &crate::AppState) {
    let disabled = state.disabled.lock().unwrap().clone();
    let tokens = load_tokens(&state.cfg, &disabled);
    {
        let mut tlock = state.tokens.lock().unwrap();
        *tlock = tokens.clone();
    }
    {
        let mut stats = state.stats.lock().unwrap();
        stats.per_account = tokens
            .iter()
            .map(|t| crate::AccountUsage {
                label: t.label.clone(),
                account_id: t.account_id.clone().unwrap_or_default(),
                requests: 0,
                errors: 0,
            })
            .collect();
        stats.total_requests = 0;
        stats.total_errors = 0;
    }
    {
        let mut cache = state.quota_cache.lock().unwrap();
        *cache = vec![None; tokens.len()];
    }
}

fn parse_jwt_subscription_until(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let raw = v
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_subscription_active_until"))?;
    if let Some(s) = raw.as_str() {
        return Some(s.to_string());
    }
    if let Some(ts) = raw.as_f64() {
        if ts > 0.0 {
            let dt = if ts > 1e12 {
                DateTime::<Utc>::from(std::time::UNIX_EPOCH + Duration::from_millis(ts as u64))
            } else {
                DateTime::<Utc>::from(std::time::UNIX_EPOCH + Duration::from_secs(ts as u64))
            };
            return Some(dt.to_rfc3339());
        }
    }
    None
}
