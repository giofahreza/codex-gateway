use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Default, Serialize)]
pub struct GrokAccount {
    pub label: String,
    pub email: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_at: Option<String>,
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

        let label = value
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .or_else(|| {
                value
                    .get("email")
                    .and_then(|v| v.as_str())
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
            email,
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
    crate::sync_usage_stats(state);
}

pub fn pick_account(state: &crate::AppState) -> Option<GrokAccount> {
    let mut idx = state.grok_rr.lock().unwrap();
    let accounts = state.grok_accounts.lock().unwrap();
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

pub fn first_enabled(state: &crate::AppState) -> Option<GrokAccount> {
    state
        .grok_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.enabled)
        .cloned()
}
