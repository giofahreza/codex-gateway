use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Default, Serialize)]
pub struct DeepSeekAccount {
    pub account_id: String,
    pub label: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub file_name: Option<String>,
    pub enabled: bool,
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<DeepSeekAccount> {
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

                if value.get("type").and_then(|v| v.as_str()) != Some("deepseek") {
                    continue;
                }

                let api_key = value
                    .get("api_key")
                    .or_else(|| value.get("access_token"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if api_key.is_empty() {
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

                let label = value
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .map(|label| label.to_string())
                    .or_else(|| {
                        path.file_stem()
                            .and_then(|value| value.to_str())
                            .map(|value| value.to_string())
                    })
                    .unwrap_or_else(|| "deepseek".to_string());
                let account_id = value
                    .get("account_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| label.clone());

                accounts.push(DeepSeekAccount {
                    account_id,
                    label,
                    api_key,
                    base_url: value
                        .get("base_url")
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
        let mut lock = state.deepseek_accounts.lock().unwrap();
        *lock = accounts;
    }
    crate::sync_usage_stats(state);
}

pub fn candidate_accounts(state: &crate::AppState) -> Vec<DeepSeekAccount> {
    let mut idx = state.deepseek_rr.lock().unwrap();
    let accounts = state.deepseek_accounts.lock().unwrap().clone();
    if accounts.is_empty() {
        return Vec::new();
    }

    let len = accounts.len();
    let picked_indices = crate::select_ordered_account_indices(
        len,
        *idx,
        |candidate_idx| accounts[candidate_idx].enabled,
        |candidate_idx| crate::deepseek_account_selection_score(state, &accounts[candidate_idx]),
    );
    if let Some(picked_idx) = picked_indices.first() {
        *idx = (picked_idx + 1) % len;
    }
    picked_indices
        .into_iter()
        .map(|candidate_idx| accounts[candidate_idx].clone())
        .collect()
}

pub fn first_enabled(state: &crate::AppState) -> Option<DeepSeekAccount> {
    state
        .deepseek_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|account| account.enabled)
        .cloned()
}
