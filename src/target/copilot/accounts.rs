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
    #[serde(default)]
    pub model_picker_category: Option<String>,
    #[serde(default)]
    pub policy_state: Option<String>,
}

pub fn model_billing_tier(model_id: &str, model_picker_category: Option<&str>) -> &'static str {
    if is_observed_non_premium_model(model_id) {
        return "non_premium";
    }

    match model_picker_category
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "powerful" | "versatile" => "premium",
        "lightweight" => "unknown",
        _ => "unknown",
    }
}

pub fn model_is_premium(model_id: &str, model_picker_category: Option<&str>) -> Option<bool> {
    match model_billing_tier(model_id, model_picker_category) {
        "premium" => Some(true),
        "non_premium" => Some(false),
        _ => None,
    }
}

pub fn is_utility_model(model_id: &str) -> bool {
    is_observed_non_premium_model(model_id)
}

pub fn is_app_accessible_model(model_id: &str) -> bool {
    is_observed_non_premium_model(model_id)
}

pub fn is_observed_non_premium_model(model_id: &str) -> bool {
    let id = model_id
        .trim()
        .strip_prefix("cop:")
        .unwrap_or(model_id.trim())
        .to_ascii_lowercase();
    id == "gpt-3.5-turbo"
        || id == "gpt-3.5-turbo-0613"
        || id == "gpt-4-o-preview"
        || id == "gpt-4.1"
        || id.starts_with("gpt-4.1-")
        || id == "gpt-4o"
        || id.starts_with("gpt-4o-")
        || id == "gpt-4o-mini"
        || id.starts_with("gpt-4o-mini-")
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

pub fn candidate_accounts(state: &crate::AppState) -> Vec<CopilotAccount> {
    let mut idx = state.copilot_rr.lock().unwrap();
    let accounts = state.copilot_accounts.lock().unwrap().clone();
    if accounts.is_empty() {
        return Vec::new();
    }

    let len = accounts.len();
    let picked_indices = crate::select_ordered_account_indices(
        len,
        *idx,
        |candidate_idx| {
            accounts[candidate_idx].enabled
                && crate::router_account_eligible(
                    state,
                    "copilot",
                    &crate::copilot_stats_key(&accounts[candidate_idx]),
                )
        },
        |candidate_idx| crate::copilot_account_selection_score(state, &accounts[candidate_idx]),
    );
    if let Some(picked_idx) = picked_indices.first() {
        *idx = (picked_idx + 1) % len;
        crate::router_reserve_account(
            state,
            "copilot",
            &crate::copilot_stats_key(&accounts[*picked_idx]),
        );
    }
    picked_indices
        .into_iter()
        .map(|candidate_idx| accounts[candidate_idx].clone())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_billing_tier_follows_copilot_picker_category() {
        assert_eq!(
            model_billing_tier("claude-opus-4.7", Some("powerful")),
            "premium"
        );
        assert_eq!(
            model_billing_tier("claude-sonnet-4.5", Some("versatile")),
            "premium"
        );
        assert_eq!(
            model_billing_tier("gpt-5-mini", Some("lightweight")),
            "unknown"
        );
        assert_eq!(model_billing_tier("trajectory-compaction", None), "unknown");
        assert_eq!(
            model_is_premium("claude-opus-4.7", Some("powerful")),
            Some(true)
        );
        assert_eq!(model_is_premium("gpt-5-mini", Some("lightweight")), None);
        assert_eq!(model_is_premium("trajectory-compaction", None), None);
    }

    #[test]
    fn model_billing_tier_marks_utility_models_non_premium() {
        for model in [
            "gpt-3.5-turbo",
            "gpt-3.5-turbo-0613",
            "gpt-4-o-preview",
            "gpt-4.1",
            "gpt-4.1-2025-04-14",
            "gpt-4o",
            "gpt-4o-2024-11-20",
            "gpt-4o-mini",
            "gpt-4o-mini-2024-07-18",
            "cop:gpt-4.1",
        ] {
            assert_eq!(model_billing_tier(model, Some("versatile")), "non_premium");
            assert_eq!(model_is_premium(model, Some("versatile")), Some(false));
            assert!(is_app_accessible_model(model));
        }
        assert!(!is_app_accessible_model("gpt-41-copilot"));
        assert!(!is_app_accessible_model("trajectory-compaction"));
    }
}
