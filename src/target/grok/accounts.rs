use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Default, Serialize)]
pub struct GrokAccount {
    pub label: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub user_id: Option<String>,
    pub team_id: Option<String>,
    pub team_blocked: Option<bool>,
    pub zdr_status: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_at: Option<String>,
    pub api_base_url: Option<String>,
    pub models: Vec<super::auth::GrokModelInfo>,
    pub rate_limits: Vec<super::auth::GrokRateLimitInfo>,
    pub last_effective_model: Option<String>,
    pub file_name: Option<String>,
    pub enabled: bool,
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<GrokAccount> {
    let mut accounts = Vec::new();

    let dir = match cfg.auth_dir.as_ref() {
        Some(dir) => dir,
        None => return accounts,
    };

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return accounts,
    };

    let mut files: Vec<_> = entries.filter_map(|entry| entry.ok()).collect();
    files.sort_by_key(|entry| entry.path());

    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
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

        if value.get("type").and_then(|v| v.as_str()) != Some("grok") {
            continue;
        }

        let access_token = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if access_token.is_empty() {
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(|v| v.to_str())
            .map(|v| v.to_string());
        let enabled = file_name
            .as_ref()
            .map(|v| !disabled.contains(v))
            .unwrap_or(true);

        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string());

        let label = name
            .clone()
            .or_else(|| {
                value
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string())
            })
            .or_else(|| {
                value
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string())
            })
            .or_else(|| {
                value
                    .get("user_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string())
            })
            .or_else(|| {
                path.file_stem()
                    .and_then(|v| v.to_str())
                    .map(|v| v.to_string())
            })
            .unwrap_or_else(|| "grok".to_string());

        let email = value
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string());

        accounts.push(GrokAccount {
            label,
            name,
            email,
            email_verified: value.get("email_verified").and_then(|v| v.as_bool()),
            user_id: value
                .get("user_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string()),
            team_id: value
                .get("team_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string()),
            team_blocked: value.get("team_blocked").and_then(|v| v.as_bool()),
            zdr_status: value
                .get("zdr_status")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string()),
            access_token,
            refresh_token: value
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|v| v.to_string()),
            token_type: value
                .get("token_type")
                .and_then(|v| v.as_str())
                .unwrap_or("Bearer")
                .to_string(),
            expires_at: value
                .get("expires_at")
                .and_then(|v| v.as_str())
                .map(|v| v.to_string()),
            api_base_url: value
                .get("api_base_url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string()),
            models: value
                .get("models")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            rate_limits: value
                .get("rate_limits")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            last_effective_model: value
                .get("last_effective_model")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string()),
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
        let mut lock = state.grok_accounts.lock().unwrap();
        *lock = accounts;
    }
    crate::migrate_grok_usage_keys(state);
    crate::sync_usage_stats(state);
}

pub fn update_runtime_metadata(
    state: &crate::AppState,
    file_name: Option<&str>,
    effective_model: Option<&str>,
    rate_limits: &[super::auth::GrokRateLimitInfo],
) {
    let Some(file_name) = file_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };

    let mut accounts = state.grok_accounts.lock().unwrap();
    let Some(account) = accounts
        .iter_mut()
        .find(|account| account.file_name.as_deref() == Some(file_name))
    else {
        return;
    };

    if let Some(model) = effective_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        account.last_effective_model = Some(model.to_string());
    }
    if !rate_limits.is_empty() {
        account.rate_limits = rate_limits.to_vec();
    }
}

pub fn candidate_accounts(state: &crate::AppState) -> Vec<GrokAccount> {
    let mut idx = state.grok_rr.lock().unwrap();
    let accounts = state.grok_accounts.lock().unwrap().clone();
    if accounts.is_empty() {
        return Vec::new();
    }

    let len = accounts.len();
    let priority_accounts = crate::routing_priority_accounts_for_provider(state, "grok");
    let picked_indices = crate::select_ordered_account_indices_with_priority(
        len,
        *idx,
        |candidate_idx| {
            accounts[candidate_idx].enabled
                && crate::router_account_eligible(
                    state,
                    "grok",
                    &crate::grok_stats_key(&accounts[candidate_idx]),
                )
        },
        |candidate_idx| {
            priority_accounts.contains(&crate::grok_stats_key(&accounts[candidate_idx]))
        },
        |candidate_idx| crate::grok_account_selection_score(state, &accounts[candidate_idx]),
    );
    if let Some(picked_idx) = picked_indices.first() {
        *idx = (picked_idx + 1) % len;
        crate::router_reserve_account(
            state,
            "grok",
            &crate::grok_stats_key(&accounts[*picked_idx]),
        );
    }
    picked_indices
        .into_iter()
        .map(|candidate_idx| accounts[candidate_idx].clone())
        .collect()
}

pub fn first_enabled(state: &crate::AppState) -> Option<GrokAccount> {
    state
        .grok_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.enabled)
        .cloned()
}
