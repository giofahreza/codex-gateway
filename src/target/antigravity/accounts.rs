use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Default, Serialize)]
pub struct AntigravityAccount {
    pub email: String,
    pub label: String,
    pub refresh_token: String,
    pub access_token: Option<String>,
    pub access_token_expires_at: Option<String>,
    pub project_id: Option<String>,
    pub file_name: Option<String>,
    pub enabled: bool,
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<AntigravityAccount> {
    let mut accounts = Vec::new();

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
                    Ok(data) => data,
                    Err(_) => continue,
                };
                let value: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(value) => value,
                    Err(_) => continue,
                };

                if value.get("type").and_then(|v| v.as_str()) != Some("antigravity") {
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
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
                let enabled = file_name
                    .as_ref()
                    .map(|name| !disabled.contains(name))
                    .unwrap_or(true);

                let email = value
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown@antigravity")
                    .to_string();
                let label = value
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&email)
                    .to_string();

                accounts.push(AntigravityAccount {
                    email,
                    label,
                    refresh_token,
                    access_token: value
                        .get("access_token")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    access_token_expires_at: value
                        .get("access_token_expires_at")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    project_id: value
                        .get("project_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
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
        let mut lock = state.agw_accounts.lock().unwrap();
        *lock = accounts.clone();
    }
    let mut quota_cache = state.agw_quota_cache.lock().unwrap();
    super::quota::prune_cache(&mut quota_cache, &accounts);
    drop(quota_cache);
    crate::sync_usage_stats(state);
}

pub fn pick_account(state: &crate::AppState) -> Option<AntigravityAccount> {
    let mut idx = state.agw_rr.lock().unwrap();
    let accounts = state.agw_accounts.lock().unwrap().clone();
    if accounts.is_empty() {
        return None;
    }

    let len = accounts.len();
    let picked_idx = crate::select_best_account_index(
        len,
        *idx,
        |candidate_idx| accounts[candidate_idx].enabled,
        |candidate_idx| crate::antigravity_account_selection_score(state, &accounts[candidate_idx]),
    )?;
    *idx = (picked_idx + 1) % len;
    Some(accounts[picked_idx].clone())
}

pub fn first_enabled(state: &crate::AppState) -> Option<AntigravityAccount> {
    state
        .agw_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|account| account.enabled)
        .cloned()
}
