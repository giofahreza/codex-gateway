use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Default, Serialize)]
pub struct GeminiAccount {
    pub email: String,
    pub label: String,
    pub refresh_token: String,
    pub access_token: Option<String>,
    pub token_type: Option<String>,
    pub expiry: Option<String>,
    pub oauth_client_id: Option<String>,
    pub oauth_client_secret: Option<String>,
    pub project_id: Option<String>,
    pub auto: bool,
    pub checked: bool,
    pub file_name: Option<String>,
    pub enabled: bool,
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<GeminiAccount> {
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

                if value.get("type").and_then(|v| v.as_str()) != Some("gemini") {
                    continue;
                }

                let token_value = value.get("token");
                let refresh_token = token_value
                    .and_then(|token| token.get("refresh_token"))
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("refresh_token").and_then(|v| v.as_str()))
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

                let email = value
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown@gemini")
                    .trim()
                    .to_string();
                let label = value
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .unwrap_or(&email)
                    .to_string();

                accounts.push(GeminiAccount {
                    email,
                    label,
                    refresh_token,
                    access_token: token_value
                        .and_then(|token| token.get("access_token"))
                        .and_then(|v| v.as_str())
                        .or_else(|| value.get("access_token").and_then(|v| v.as_str()))
                        .map(|value| value.to_string()),
                    token_type: token_value
                        .and_then(|token| token.get("token_type"))
                        .and_then(|v| v.as_str())
                        .or_else(|| value.get("token_type").and_then(|v| v.as_str()))
                        .map(|value| value.to_string()),
                    expiry: token_value
                        .and_then(|token| token.get("expiry"))
                        .and_then(|v| v.as_str())
                        .or_else(|| value.get("expiry").and_then(|v| v.as_str()))
                        .map(|value| value.to_string()),
                    oauth_client_id: token_value
                        .and_then(|token| token.get("client_id"))
                        .and_then(|v| v.as_str())
                        .or_else(|| value.get("client_id").and_then(|v| v.as_str()))
                        .map(|value| value.to_string()),
                    oauth_client_secret: token_value
                        .and_then(|token| token.get("client_secret"))
                        .and_then(|v| v.as_str())
                        .or_else(|| value.get("client_secret").and_then(|v| v.as_str()))
                        .map(|value| value.to_string()),
                    project_id: value
                        .get("project_id")
                        .and_then(|v| v.as_str())
                        .map(|value| value.to_string()),
                    auto: value.get("auto").and_then(|v| v.as_bool()).unwrap_or(false),
                    checked: value
                        .get("checked")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
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
        let mut lock = state.gemini_accounts.lock().unwrap();
        *lock = accounts.clone();
    }
    let mut quota_cache = state.gemini_quota_cache.lock().unwrap();
    super::quota::prune_cache(&mut quota_cache, &accounts);
    drop(quota_cache);
    crate::sync_usage_stats(state);
}

pub fn pick_account(state: &crate::AppState) -> Option<GeminiAccount> {
    let mut idx = state.gemini_rr.lock().unwrap();
    let accounts = state.gemini_accounts.lock().unwrap().clone();
    if accounts.is_empty() {
        return None;
    }

    let len = accounts.len();
    let picked_idx = crate::select_best_account_index(
        len,
        *idx,
        |candidate_idx| accounts[candidate_idx].enabled,
        |candidate_idx| crate::gemini_account_selection_score(state, &accounts[candidate_idx]),
    )?;
    *idx = (picked_idx + 1) % len;
    Some(accounts[picked_idx].clone())
}

pub fn first_enabled(state: &crate::AppState) -> Option<GeminiAccount> {
    state
        .gemini_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|account| account.enabled)
        .cloned()
}
