use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Default, Serialize)]
pub struct GlmAccount {
    pub account_id: String,
    pub label: String,
    pub account_type: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub file_name: Option<String>,
    pub enabled: bool,
}

pub const ACCOUNT_TYPE_API_USAGE: &str = "api_usage";
pub const ACCOUNT_TYPE_SUBSCRIPTION: &str = "subscription";

impl GlmAccount {
    pub fn normalized_account_type(&self) -> String {
        normalize_account_type(Some(&self.account_type))
    }

    pub fn is_subscription(&self) -> bool {
        self.normalized_account_type() == ACCOUNT_TYPE_SUBSCRIPTION
    }

    pub fn openai_base_url(&self) -> String {
        super::api::normalize_base_url_for_account_type(
            self.base_url.as_deref(),
            &self.normalized_account_type(),
        )
    }
}

pub fn normalize_account_type(value: Option<&str>) -> String {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase().replace(['-', ' '], "_"))
        .as_deref()
    {
        Some("subscription") | Some("coding_plan") | Some("coding") | Some("plan") => {
            ACCOUNT_TYPE_SUBSCRIPTION.to_string()
        }
        Some("api") | Some("api_usage") | Some("usage") | Some("pay_as_you_go") | Some("payg") => {
            ACCOUNT_TYPE_API_USAGE.to_string()
        }
        _ => ACCOUNT_TYPE_API_USAGE.to_string(),
    }
}

fn account_type_from_value(value: &serde_json::Value) -> String {
    if let Some(account_type) = value
        .get("account_type")
        .or_else(|| value.get("usage_type"))
        .and_then(|v| v.as_str())
    {
        return normalize_account_type(Some(account_type));
    }

    let has_subscription_route = value
        .get("base_url")
        .or_else(|| value.get("openai_base_url"))
        .and_then(|v| v.as_str())
        .map(|value| value.to_ascii_lowercase().contains("/coding/"))
        .unwrap_or(false)
        || value
            .get("anthropic_base_url")
            .and_then(|v| v.as_str())
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);

    if has_subscription_route {
        ACCOUNT_TYPE_SUBSCRIPTION.to_string()
    } else {
        ACCOUNT_TYPE_API_USAGE.to_string()
    }
}

pub fn load_accounts(cfg: &crate::Config, disabled: &HashSet<String>) -> Vec<GlmAccount> {
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

                if value.get("type").and_then(|v| v.as_str()) != Some(super::PROVIDER_NAME) {
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
                    .unwrap_or_else(|| super::PROVIDER_NAME.to_string());
                let account_id = value
                    .get("account_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| label.clone());
                let account_type = account_type_from_value(&value);

                accounts.push(GlmAccount {
                    account_id,
                    label,
                    account_type,
                    api_key,
                    base_url: value
                        .get("base_url")
                        .or_else(|| value.get("openai_base_url"))
                        .and_then(|v| v.as_str())
                        .map(|value| value.to_string()),
                    anthropic_base_url: value
                        .get("anthropic_base_url")
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
        let mut lock = state.glm_accounts.lock().unwrap();
        *lock = accounts;
    }
    crate::sync_usage_stats(state);
}

pub fn candidate_accounts(state: &crate::AppState) -> Vec<GlmAccount> {
    let mut idx = state.glm_rr.lock().unwrap();
    let accounts = state.glm_accounts.lock().unwrap().clone();
    if accounts.is_empty() {
        return Vec::new();
    }

    let len = accounts.len();
    let picked_indices = crate::select_ordered_account_indices(
        len,
        *idx,
        |candidate_idx| accounts[candidate_idx].enabled,
        |candidate_idx| crate::glm_account_selection_score(state, &accounts[candidate_idx]),
    );
    if let Some(picked_idx) = picked_indices.first() {
        *idx = (picked_idx + 1) % len;
    }
    picked_indices
        .into_iter()
        .map(|candidate_idx| accounts[candidate_idx].clone())
        .collect()
}

pub fn first_enabled(state: &crate::AppState) -> Option<GlmAccount> {
    state
        .glm_accounts
        .lock()
        .unwrap()
        .iter()
        .find(|account| account.enabled)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_account_type_aliases() {
        assert_eq!(normalize_account_type(Some("api")), ACCOUNT_TYPE_API_USAGE);
        assert_eq!(
            normalize_account_type(Some("api usage")),
            ACCOUNT_TYPE_API_USAGE
        );
        assert_eq!(
            normalize_account_type(Some("coding-plan")),
            ACCOUNT_TYPE_SUBSCRIPTION
        );
        assert_eq!(
            normalize_account_type(Some("subscription")),
            ACCOUNT_TYPE_SUBSCRIPTION
        );
    }

    #[test]
    fn infers_old_coding_plan_credentials_from_routes() {
        assert_eq!(
            account_type_from_value(&json!({
                "base_url": "https://api.z.ai/api/coding/paas/v4"
            })),
            ACCOUNT_TYPE_SUBSCRIPTION
        );
        assert_eq!(account_type_from_value(&json!({})), ACCOUNT_TYPE_API_USAGE);
    }
}
