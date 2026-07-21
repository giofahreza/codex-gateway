use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClaudeModelInfo {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub model_type: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClaudeAccount {
    pub organization_uuid: String,
    pub account_id: String,
    pub label: String,
    pub email: Option<String>,
    pub scopes: Vec<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_at: Option<String>,
    pub api_base_url: Option<String>,
    pub models: Vec<ClaudeModelInfo>,
    pub file_name: Option<String>,
    pub enabled: bool,
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<ClaudeAccount> {
    let mut accounts = Vec::new();
    let Some(dir) = cfg.auth_dir.as_ref() else {
        return accounts;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return accounts;
    };

    let mut files: Vec<_> = entries.filter_map(|entry| entry.ok()).collect();
    files.sort_by_key(|entry| entry.path());

    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }

        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) != Some(super::PROVIDER_NAME) {
            continue;
        }

        let access_token = value
            .get("access_token")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if access_token.is_empty() {
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
        let organization_uuid = value
            .get("organization_uuid")
            .or_else(|| value.get("organization_id"))
            .or_else(|| value.get("account_id"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
            .unwrap_or_default();
        let email = value
            .get("email")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        let label = value
            .get("label")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
            .or_else(|| email.clone())
            .or_else(|| {
                if organization_uuid.is_empty() {
                    None
                } else {
                    Some(organization_uuid.clone())
                }
            })
            .or_else(|| {
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_string())
            })
            .unwrap_or_else(|| super::PROVIDER_NAME.to_string());
        let account_id = value
            .get("account_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
            .unwrap_or_else(|| {
                if organization_uuid.is_empty() {
                    label.clone()
                } else {
                    organization_uuid.clone()
                }
            });

        accounts.push(ClaudeAccount {
            organization_uuid,
            account_id,
            label,
            email,
            scopes: value
                .get("scopes")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            access_token,
            refresh_token: value
                .get("refresh_token")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string()),
            token_type: value
                .get("token_type")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("Bearer")
                .to_string(),
            expires_at: value
                .get("expires_at")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string()),
            api_base_url: value
                .get("api_base_url")
                .or_else(|| value.get("base_url"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string()),
            models: value
                .get("models")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| serde_json::from_value(item.clone()).ok())
                        .collect::<Vec<ClaudeModelInfo>>()
                })
                .unwrap_or_default(),
            file_name,
            enabled,
        });
    }

    accounts
}

pub fn reload_state(state: &crate::AppState) {
    let disabled = state.disabled.lock().unwrap().clone();
    let accounts = load_accounts(&state.cfg, &disabled);
    {
        let mut lock = state.claude_accounts.lock().unwrap();
        *lock = accounts;
    }
    crate::sync_usage_stats(state);
}

pub fn candidate_accounts(state: &crate::AppState) -> Vec<ClaudeAccount> {
    let mut idx = state.claude_rr.lock().unwrap();
    let accounts = state.claude_accounts.lock().unwrap().clone();
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
                    "claude",
                    &crate::claude_stats_key(&accounts[candidate_idx]),
                )
        },
        |candidate_idx| crate::claude_account_selection_score(state, &accounts[candidate_idx]),
    );
    if let Some(picked_idx) = picked_indices.first() {
        *idx = (picked_idx + 1) % len;
        crate::router_reserve_account(
            state,
            "claude",
            &crate::claude_stats_key(&accounts[*picked_idx]),
        );
    }
    picked_indices
        .into_iter()
        .map(|candidate_idx| accounts[candidate_idx].clone())
        .collect()
}

pub fn first_enabled(state: &crate::AppState) -> Option<ClaudeAccount> {
    state
        .claude_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|account| account.enabled)
        .cloned()
}

/// Classify the reason `candidate_accounts` returned an empty list, so the
/// caller can surface a precise error instead of the misleading "no accounts
/// configured" message.
pub fn empty_accounts_reason(state: &crate::AppState) -> &'static str {
    let accounts = state.claude_accounts.lock().unwrap();
    if accounts.is_empty() {
        return "No Claude accounts configured";
    }
    if !accounts.iter().any(|account| account.enabled) {
        return "All Claude accounts are disabled";
    }
    let now = std::time::Instant::now();
    let mut in_cooldown = 0usize;
    for account in accounts.iter().filter(|account| account.enabled) {
        let key = crate::account_router_key("claude", &crate::claude_stats_key(account));
        let mut router = state.account_router.lock().unwrap();
        let runtime = router.entry(key).or_default();
        if matches!(
            crate::account_runtime_status(true, runtime, now),
            crate::AccountRuntimeStatus::Cooldown
        ) {
            in_cooldown += 1;
        }
    }
    if in_cooldown == accounts.len() {
        return "All Claude accounts are temporarily in cooldown";
    }
    "No eligible Claude accounts (all disabled or cooling down)"
}
