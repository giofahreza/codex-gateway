use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CopilotModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub preview: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CopilotAccount {
    pub account_id: String,
    pub label: String,
    pub login: String,
    pub github_token: String,
    pub copilot_token: Option<String>,
    pub copilot_expires_at: Option<i64>,
    pub copilot_refresh_in: Option<i64>,
    pub account_type: String,
    pub file_name: Option<String>,
    pub enabled: bool,
    pub models: Vec<CopilotModelInfo>,
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<CopilotAccount> {
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

                if value.get("type").and_then(|value| value.as_str()) != Some("copilot") {
                    continue;
                }

                let github_token = value
                    .get("github_token")
                    .or_else(|| value.get("access_token"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if github_token.is_empty() {
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

                let login = value
                    .get("login")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                let label = value
                    .get("label")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .or_else(|| {
                        if login.is_empty() {
                            None
                        } else {
                            Some(login.clone())
                        }
                    })
                    .or_else(|| {
                        path.file_stem()
                            .and_then(|value| value.to_str())
                            .map(|value| value.to_string())
                    })
                    .unwrap_or_else(|| "copilot".to_string());
                let account_id = value
                    .get("account_id")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| {
                        if login.is_empty() {
                            label.clone()
                        } else {
                            login.clone()
                        }
                    });
                let account_type = super::auth::normalize_account_type(
                    value.get("account_type").and_then(|value| value.as_str()),
                );
                let models = value
                    .get("models")
                    .and_then(|value| value.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| serde_json::from_value(item.clone()).ok())
                            .collect::<Vec<CopilotModelInfo>>()
                    })
                    .unwrap_or_default();

                accounts.push(CopilotAccount {
                    account_id,
                    label,
                    login,
                    github_token,
                    copilot_token: value
                        .get("copilot_token")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string()),
                    copilot_expires_at: value
                        .get("copilot_expires_at")
                        .or_else(|| value.get("expires_at"))
                        .and_then(|value| value.as_i64()),
                    copilot_refresh_in: value
                        .get("copilot_refresh_in")
                        .or_else(|| value.get("refresh_in"))
                        .and_then(|value| value.as_i64()),
                    account_type,
                    file_name,
                    enabled,
                    models,
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
        let mut lock = state.copilot_accounts.lock().unwrap();
        *lock = accounts;
    }
    crate::sync_usage_stats(state);
}

pub fn pick_account(state: &crate::AppState) -> Option<CopilotAccount> {
    let mut idx = state.copilot_rr.lock().unwrap();
    let accounts = state.copilot_accounts.lock().unwrap().clone();
    if accounts.is_empty() {
        return None;
    }

    let len = accounts.len();
    let picked_idx = crate::select_best_account_index(
        len,
        *idx,
        |candidate_idx| accounts[candidate_idx].enabled,
        |candidate_idx| crate::copilot_account_selection_score(state, &accounts[candidate_idx]),
    )?;
    *idx = (picked_idx + 1) % len;
    Some(accounts[picked_idx].clone())
}

pub fn first_enabled(state: &crate::AppState) -> Option<CopilotAccount> {
    state
        .copilot_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|account| account.enabled)
        .cloned()
}
