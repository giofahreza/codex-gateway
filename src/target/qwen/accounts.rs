use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Default, Serialize)]
pub struct QwenAccount {
    pub email: String,
    pub subject: Option<String>,
    pub label: String,
    pub refresh_token: String,
    pub access_token: Option<String>,
    pub resource_url: Option<String>,
    pub expired_at: Option<String>,
    pub file_name: Option<String>,
    pub enabled: bool,
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<QwenAccount> {
    let mut accounts = Vec::new();

    if let Some(dir) = cfg.auth_dir.as_ref() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut files: Vec<_> = entries.filter_map(|entry| entry.ok()).collect();
            files.sort_by_key(|entry| entry.path());

            for entry in files {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }

                let data = match std::fs::read_to_string(&path) {
                    Ok(data) => data,
                    Err(_) => continue,
                };
                let value: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(value) => value,
                    Err(_) => continue,
                };

                if value.get("type").and_then(|v| v.as_str()) != Some("qwen") {
                    continue;
                }

                let refresh_token = value
                    .get("refresh_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if refresh_token.is_empty() {
                    continue;
                }

                let file_name = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_string());
                let enabled = file_name
                    .as_ref()
                    .map(|value| !disabled.contains(value))
                    .unwrap_or(true);

                let access_token = value
                    .get("access_token")
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string());
                let fallback_email = value
                    .get("email")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("label").and_then(|v| v.as_str()))
                    .unwrap_or("qwen-account")
                    .to_string();
                let identity = access_token
                    .as_deref()
                    .map(|token| super::auth::identity_from_access_token(token, &fallback_email));
                let subject = value
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string())
                    .or_else(|| {
                        identity
                            .as_ref()
                            .and_then(|identity| identity.subject.clone())
                    });
                let email = value
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string())
                    .or_else(|| {
                        identity
                            .as_ref()
                            .and_then(|identity| identity.email.clone())
                    })
                    .unwrap_or(fallback_email);
                let label = value
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let label = if label.trim().is_empty() {
                    identity
                        .as_ref()
                        .map(|identity| identity.label.clone())
                        .unwrap_or_else(|| email.clone())
                } else {
                    label
                };

                accounts.push(QwenAccount {
                    email,
                    subject,
                    label,
                    refresh_token,
                    access_token,
                    resource_url: value
                        .get("resource_url")
                        .and_then(|v| v.as_str())
                        .map(|value| value.to_string()),
                    expired_at: value
                        .get("expired")
                        .and_then(|v| v.as_str())
                        .map(|value| value.to_string()),
                    file_name,
                    enabled,
                });
            }
        }
    }

    accounts
}

pub fn reload_state(state: &crate::AppState) {
    let disabled = state.disabled.lock().unwrap().clone();
    let accounts = load_accounts(&state.cfg, &disabled);
    {
        let mut lock = state.qwen_accounts.lock().unwrap();
        *lock = accounts.clone();
    }
    let mut quota_cache = state.qwen_quota_cache.lock().unwrap();
    super::quota::prune_cache(&mut quota_cache, &accounts);
    drop(quota_cache);
    crate::migrate_qwen_usage_keys(state);
    crate::sync_usage_stats(state);
}

pub fn pick_account(state: &crate::AppState) -> Option<QwenAccount> {
    let mut idx = state.qwen_rr.lock().unwrap();
    let accounts = state.qwen_accounts.lock().unwrap();
    if accounts.is_empty() {
        return None;
    }

    let len = accounts.len();
    for _ in 0..len {
        let picked_idx = *idx % len;
        *idx = (*idx + 1) % len;
        if accounts[picked_idx].enabled {
            return Some(accounts[picked_idx].clone());
        }
    }
    None
}

pub fn first_enabled(state: &crate::AppState) -> Option<QwenAccount> {
    state
        .qwen_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|account| account.enabled)
        .cloned()
}
