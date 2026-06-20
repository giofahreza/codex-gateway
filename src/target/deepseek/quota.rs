//! DeepSeek balance fetcher.
//!
//! DeepSeek exposes a single `GET /user/balance` endpoint on its public
//! API at `https://api.deepseek.com` (or the account's `base_url` if
//! configured). The response is a small JSON object with the user's
//! current balance in USD:
//!
//! ```json
//! {
//!   "is_available": true,
//!   "balance_infos": [
//!     { "currency": "USD", "total_balance": "5.76",
//!       "granted_balance": "0.00", "topped_up_balance": "5.76" }
//!   ]
//! }
//! ```
//!
//! We use the same Bearer-token auth the chat API uses, cache each
//! account's response for 60 seconds, and surface a friendly per-currency
//! summary on the admin dashboard.

use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Cache TTL in seconds.
const CACHE_TTL_SECS: u64 = 60;
const REQUEST_TIMEOUT_SECS: u64 = 20;
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const BALANCE_PATH: &str = "/user/balance";

#[derive(Clone, Debug)]
pub struct QuotaCacheEntry {
    pub fetched_at: std::time::Instant,
    pub summary: QuotaSummary,
    pub error: Option<String>,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct QuotaSummary {
    pub label: String,
    pub file_name: String,
    pub is_available: bool,
    /// `true` if the upstream returned at least one balance entry we
    /// could parse.
    pub has_balance: bool,
    pub balances: Vec<BalanceEntry>,
    /// Raw response, useful for debugging.
    pub raw: Value,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct BalanceEntry {
    pub currency: String,
    pub total_balance: String,
    pub granted_balance: String,
    pub topped_up_balance: String,
}

pub async fn get_quota_summaries(state: &crate::AppState) -> Vec<Value> {
    let accounts = state.deepseek_accounts.lock().unwrap().clone();
    let now = std::time::Instant::now();
    let mut results = Vec::with_capacity(accounts.len());

    for account in &accounts {
        let key = cache_key(account);
        let cached = {
            let cache = state.deepseek_quota_cache.lock().unwrap();
            cache.get(&key).cloned()
        };

        let entry = if let Some(cached) = cached {
            if now.duration_since(cached.fetched_at).as_secs() < CACHE_TTL_SECS {
                cached
            } else {
                let fetched = fetch_account_quota(&state.client, account).await;
                let mut cache = state.deepseek_quota_cache.lock().unwrap();
                cache.insert(key.clone(), fetched.clone());
                fetched
            }
        } else {
            let fetched = fetch_account_quota(&state.client, account).await;
            let mut cache = state.deepseek_quota_cache.lock().unwrap();
            cache.insert(key.clone(), fetched.clone());
            fetched
        };

        if let Some(err) = entry.error {
            results.push(json!({
                "label": account.label,
                "file_name": account.file_name.clone().unwrap_or_default(),
                "error": err
            }));
        } else {
            results.push(json!({
                "label": entry.summary.label,
                "file_name": entry.summary.file_name,
                "is_available": entry.summary.is_available,
                "has_balance": entry.summary.has_balance,
                "balances": entry.summary.balances,
                "raw": entry.summary.raw,
            }));
        }
    }

    results
}

fn cache_key(account: &super::accounts::DeepSeekAccount) -> String {
    account
        .file_name
        .clone()
        .unwrap_or_else(|| account.label.clone())
}

async fn fetch_account_quota(
    client: &reqwest::Client,
    account: &super::accounts::DeepSeekAccount,
) -> QuotaCacheEntry {
    let mut summary = QuotaSummary {
        label: account.label.clone(),
        file_name: account.file_name.clone().unwrap_or_default(),
        ..Default::default()
    };
    match fetch_balance(client, account).await {
        Ok((is_available, balances, raw)) => {
            summary.is_available = is_available;
            summary.has_balance = !balances.is_empty();
            summary.balances = balances;
            summary.raw = raw;
            QuotaCacheEntry {
                fetched_at: std::time::Instant::now(),
                summary,
                error: None,
            }
        }
        Err(err) => QuotaCacheEntry {
            fetched_at: std::time::Instant::now(),
            summary,
            error: Some(err),
        },
    }
}

async fn fetch_balance(
    client: &reqwest::Client,
    account: &super::accounts::DeepSeekAccount,
) -> Result<(bool, Vec<BalanceEntry>, Value), String> {
    let base = normalize_base_url(account.base_url.as_deref());
    let url = format!("{}{}", base.trim_end_matches('/'), BALANCE_PATH);

    let resp = client
        .get(&url)
        .header(
            "Authorization",
            format!("Bearer {}", account.api_key.trim()),
        )
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| format!("DeepSeek balance request to {} failed: {}", url, e))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("DeepSeek balance body read failed: {}", e))?;
    if !status.is_success() {
        return Err(format!(
            "DeepSeek balance at {} returned {}: {}",
            url, status, text
        ));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("DeepSeek balance JSON parse failed: {}", e))?;
    let (is_available, balances) = parse_balance_response(&value);
    Ok((is_available, balances, value))
}

fn parse_balance_response(value: &Value) -> (bool, Vec<BalanceEntry>) {
    let is_available = value
        .get("is_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut balances: Vec<BalanceEntry> = Vec::new();
    if let Some(arr) = value.get("balance_infos").and_then(|v| v.as_array()) {
        for item in arr {
            balances.push(BalanceEntry {
                currency: string_field(item, "currency"),
                total_balance: string_field(item, "total_balance"),
                granted_balance: string_field(item, "granted_balance"),
                topped_up_balance: string_field(item, "topped_up_balance"),
            });
        }
    }
    (is_available, balances)
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Normalize the configured `base_url` the same way the chat path does,
/// but with a default of `https://api.deepseek.com` since that's where
/// the balance endpoint lives.
fn normalize_base_url(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_balance_response() {
        let raw = json!({
            "is_available": true,
            "balance_infos": [
                {
                    "currency": "USD",
                    "total_balance": "5.76",
                    "granted_balance": "0.00",
                    "topped_up_balance": "5.76"
                }
            ]
        });
        let (is_available, balances) = parse_balance_response(&raw);
        assert!(is_available);
        assert_eq!(balances.len(), 1);
        let b = &balances[0];
        assert_eq!(b.currency, "USD");
        assert_eq!(b.total_balance, "5.76");
        assert_eq!(b.granted_balance, "0.00");
        assert_eq!(b.topped_up_balance, "5.76");
    }

    #[test]
    fn parses_response_with_empty_balance_infos() {
        let raw = json!({ "is_available": false, "balance_infos": [] });
        let (is_available, balances) = parse_balance_response(&raw);
        assert!(!is_available);
        assert!(balances.is_empty());
    }

    #[test]
    fn handles_missing_fields_gracefully() {
        let raw = json!({
            "is_available": true,
            "balance_infos": [
                { "currency": "CNY" }
            ]
        });
        let (is_available, balances) = parse_balance_response(&raw);
        assert!(is_available);
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].currency, "CNY");
        assert_eq!(balances[0].total_balance, "");
        assert_eq!(balances[0].granted_balance, "");
    }

    #[test]
    fn parses_multiple_currencies() {
        let raw = json!({
            "is_available": true,
            "balance_infos": [
                { "currency": "USD", "total_balance": "5.76",
                  "granted_balance": "0.00", "topped_up_balance": "5.76" },
                { "currency": "CNY", "total_balance": "100.00",
                  "granted_balance": "50.00", "topped_up_balance": "50.00" }
            ]
        });
        let (_, balances) = parse_balance_response(&raw);
        assert_eq!(balances.len(), 2);
        assert_eq!(balances[0].currency, "USD");
        assert_eq!(balances[1].currency, "CNY");
    }

    #[test]
    fn normalize_uses_default_for_missing() {
        assert_eq!(normalize_base_url(None), "https://api.deepseek.com");
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(
            normalize_base_url(Some("https://api.deepseek.com/")),
            "https://api.deepseek.com"
        );
    }

    #[test]
    fn normalize_uses_custom_host() {
        assert_eq!(
            normalize_base_url(Some("https://proxy.example.com/v1")),
            "https://proxy.example.com/v1"
        );
    }
}
