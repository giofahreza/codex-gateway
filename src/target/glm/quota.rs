use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

const REQUEST_TIMEOUT_SECS: u64 = 20;

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
    pub account_type: String,
    pub status_msg: String,
    pub available_models: Vec<ModelInfo>,
    /// Balance entries reported by Z.AI for `api_usage` keys (pay-as-you-go).
    /// Empty for `subscription` keys.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub balances: Vec<BalanceEntry>,
    /// Free-form note from the balance fetch (e.g. the precise reason the
    /// endpoint returned no usable data). Useful for the dashboard card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance_note: Option<String>,
    pub raw: Value,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct BalanceEntry {
    pub currency: String,
    pub total_balance: String,
    pub granted_balance: String,
    pub topped_up_balance: String,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct ModelInfo {
    pub model_id: String,
    pub display_name: String,
    pub owned_by: String,
}

pub async fn get_quota_summaries(state: &crate::AppState) -> Vec<Value> {
    let accounts = state.glm_accounts.lock().unwrap().clone();
    let now = std::time::Instant::now();
    let mut results = Vec::with_capacity(accounts.len());

    for account in &accounts {
        let key = cache_key(account);
        let cached = {
            let cache = state.glm_quota_cache.lock().unwrap();
            cache.get(&key).cloned()
        };

        let entry = if let Some(cached) = cached {
            if crate::quota_cache_entry_is_fresh(now, cached.fetched_at, &cached.summary) {
                cached
            } else {
                let fetched = fetch_account_quota(&state.client, account).await;
                let mut cache = state.glm_quota_cache.lock().unwrap();
                cache.insert(key.clone(), fetched.clone());
                fetched
            }
        } else {
            let fetched = fetch_account_quota(&state.client, account).await;
            let mut cache = state.glm_quota_cache.lock().unwrap();
            cache.insert(key.clone(), fetched.clone());
            fetched
        };

        if let Some(err) = entry.error {
            results.push(json!({
                "label": account.label,
                "file_name": account.file_name.clone().unwrap_or_default(),
                "account_type": account.normalized_account_type(),
                "error": err
            }));
        } else {
            let mut payload = json!({
                "label": entry.summary.label,
                "file_name": entry.summary.file_name,
                "account_type": entry.summary.account_type,
                "status_msg": entry.summary.status_msg,
                "available_models": entry.summary.available_models,
                "balances": entry.summary.balances,
                "raw": entry.summary.raw,
            });
            if let Some(note) = entry.summary.balance_note.as_ref() {
                if let serde_json::Value::Object(map) = &mut payload {
                    map.insert(
                        "balance_note".to_string(),
                        serde_json::Value::String(note.clone()),
                    );
                }
            }
            results.push(payload);
        }
    }

    results
}

fn cache_key(account: &super::accounts::GlmAccount) -> String {
    account
        .file_name
        .clone()
        .unwrap_or_else(|| account.label.clone())
}

async fn fetch_account_quota(
    client: &reqwest::Client,
    account: &super::accounts::GlmAccount,
) -> QuotaCacheEntry {
    let is_api_usage = !account.is_subscription();
    let models_result = fetch_models(client, account).await;
    let (models, raw, error) = match models_result {
        Ok((models, raw)) => (models, raw, None),
        Err(err) => (Vec::new(), Value::Null, Some(err)),
    };

    let (balances, balance_note, balance_status) = if is_api_usage {
        match fetch_balance(client, account).await {
            Ok(BalanceResult::Found(entries)) => (
                entries,
                None,
                Some("Live balance from Z.AI billing endpoint".to_string()),
            ),
            Ok(BalanceResult::NotAvailable(reason)) => (Vec::new(), Some(reason.clone()), None),
            Err(err) => (Vec::new(), Some(err.clone()), None),
        }
    } else {
        (Vec::new(), None, None)
    };

    let status_msg = balance_status.unwrap_or_else(|| {
        let note = balance_note.as_deref().unwrap_or(
            "This key is a GLM Coding Plan subscription; balance is not applicable here.",
        );
        format!("{note} The card shows the live model catalog and gateway-recorded usage.")
    });

    let summary = QuotaSummary {
        label: account.label.clone(),
        file_name: account.file_name.clone().unwrap_or_default(),
        account_type: account.normalized_account_type(),
        status_msg,
        available_models: models,
        balances,
        balance_note: if is_api_usage { balance_note } else { None },
        raw,
    };

    QuotaCacheEntry {
        fetched_at: std::time::Instant::now(),
        summary,
        error,
    }
}

async fn fetch_models(
    client: &reqwest::Client,
    account: &super::accounts::GlmAccount,
) -> Result<(Vec<ModelInfo>, Value), String> {
    let base = account.openai_base_url();
    let url = models_url(&base);
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
        .map_err(|e| format!("GLM models request to {} failed: {}", url, e))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("GLM models body read failed: {}", e))?;
    if !status.is_success() {
        return Err(format!(
            "GLM models at {} returned {}: {}",
            url, status, text
        ));
    }
    let raw: Value =
        serde_json::from_str(&text).map_err(|e| format!("GLM models JSON parse failed: {}", e))?;
    Ok((extract_models(&raw), raw))
}

fn models_url(base_url: &str) -> String {
    let base = super::api::normalize_base_url(Some(base_url));
    if base.ends_with("/models") {
        return base;
    }
    if let Some(stripped) = base.strip_suffix("/chat/completions") {
        return format!("{}/models", stripped.trim_end_matches('/'));
    }
    format!("{}/models", base)
}

/// Outcome of attempting to fetch a Z.AI `api_usage` balance.
///
/// `api_usage` keys on Z.AI do not currently expose a public balance endpoint on
/// `api.z.ai` — the Spring proxy returns `{"success":false,"msg":"404 NOT_FOUND"}`
/// for every `/api/finance/...` path we probe with a Bearer token. We surface a
/// precise reason on the dashboard card so it is obvious whether the gateway
/// reached Z.AI and got rejected, or whether the endpoint exists but returned
/// no balance data.
enum BalanceResult {
    Found(Vec<BalanceEntry>),
    NotAvailable(String),
}

const BALANCE_PATHS: &[&str] = &[
    "/api/finance/balance",
    "/api/account/balance",
    "/api/billing/balance",
    "/api/balance",
];

async fn fetch_balance(
    client: &reqwest::Client,
    account: &super::accounts::GlmAccount,
) -> Result<BalanceResult, String> {
    // Derive candidate host list from the chat base URL. Default `api.z.ai` is
    // what `api_usage` keys ship with; legacy `open.bigmodel.cn` is accepted as
    // a fallback for older credentials.
    let mut hosts: Vec<String> = Vec::new();
    let configured_base = account.openai_base_url();
    if let Some(host) = primary_host(&configured_base) {
        hosts.push(host);
    }
    hosts.push("z.ai".to_string());
    hosts.push("open.bigmodel.cn".to_string());
    hosts.dedup();

    let mut last_reason = String::new();
    for host in &hosts {
        for path in BALANCE_PATHS {
            let url = format!("https://{}{}", host, path);
            let resp = match client
                .get(&url)
                .header(
                    "Authorization",
                    format!("Bearer {}", account.api_key.trim()),
                )
                .header("Accept", "application/json")
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(err) => {
                    last_reason = format!("request to https://{}{} failed: {}", host, path, err);
                    continue;
                }
            };
            let status = resp.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                last_reason = format!(
                    "https://{}{} returned 404 (no public balance endpoint)",
                    host, path
                );
                continue;
            }
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                last_reason = format!(
                    "https://{}{} rejected the key ({}); check that the API key has billing read access",
                    host, path, status
                );
                // The endpoint exists; stop probing and surface the auth error.
                return Ok(BalanceResult::NotAvailable(last_reason));
            }
            let text = match resp.text().await {
                Ok(text) => text,
                Err(err) => {
                    last_reason = format!("https://{}{} body read failed: {}", host, path, err);
                    continue;
                }
            };
            if !status.is_success() {
                last_reason = format!(
                    "https://{}{} returned {}: {}",
                    host,
                    path,
                    status,
                    truncate(&text, 120)
                );
                continue;
            }
            let value: Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(err) => {
                    last_reason =
                        format!("https://{}{} returned invalid JSON: {}", host, path, err);
                    continue;
                }
            };
            // Spring gateway sometimes wraps 404 in a 200 with
            // {"success":false,"msg":"404 NOT_FOUND"}; treat that as missing.
            if looks_like_zai_not_found(&value) {
                last_reason = format!(
                    "https://{}{} reached Z.AI but reported 404 NOT_FOUND",
                    host, path
                );
                continue;
            }
            let entries = parse_balance_response(&value);
            if entries.is_empty() {
                last_reason = format!(
                    "https://{}{} returned 200 but no balance entries could be parsed ({} bytes)",
                    host,
                    path,
                    text.len()
                );
                continue;
            }
            return Ok(BalanceResult::Found(entries));
        }
    }
    Ok(BalanceResult::NotAvailable(last_reason))
}

fn looks_like_zai_not_found(value: &Value) -> bool {
    let success = value.get("success").and_then(|v| v.as_bool());
    if matches!(success, Some(false)) {
        if let Some(msg) = value.get("msg").and_then(|v| v.as_str()) {
            if msg.contains("404") || msg.to_ascii_lowercase().contains("not_found") {
                return true;
            }
        }
        if let Some(code) = value.get("code") {
            if let Some(num) = code.as_u64() {
                if num == 404 || num == 500 {
                    return true;
                }
            }
            if let Some(text) = code.as_str() {
                if text == "500" || text == "404" {
                    return true;
                }
            }
        }
    }
    false
}

fn primary_host(base_url: &str) -> Option<String> {
    let trimmed = base_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = trimmed.split('/').next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn parse_balance_response(value: &Value) -> Vec<BalanceEntry> {
    // OpenAI-style: balance_infos array of {currency,total_balance,granted_balance,topped_up_balance}
    // Z.AI-style: {"data":{"balance":"…","currency":"…",...}} or a top-level fields map.
    if let Some(arr) = value.get("balance_infos").and_then(|v| v.as_array()) {
        let mut out = Vec::new();
        for item in arr {
            out.push(BalanceEntry {
                currency: string_field(item, "currency"),
                total_balance: string_field(item, "total_balance"),
                granted_balance: string_field(item, "granted_balance"),
                topped_up_balance: string_field(item, "topped_up_balance"),
            });
        }
        return out;
    }
    if let Some(map) = value.get("data").and_then(|v| v.as_object()) {
        if map.contains_key("balance_infos") || map.contains_key("total_balance") {
            return parse_balance_response(&Value::Object(map.clone()));
        }
    }
    // Single-balance map response
    if value.get("total_balance").is_some() || value.get("balance").is_some() {
        return vec![BalanceEntry {
            currency: string_field(value, "currency"),
            total_balance: first_string(value, &["total_balance", "balance", "available"]),
            granted_balance: first_string(value, &["granted_balance", "granted", "free"]),
            topped_up_balance: first_string(value, &["topped_up_balance", "topped_up", "paid"]),
        }];
    }
    Vec::new()
}

fn first_string(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(s) = value.get(*key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
        if let Some(n) = value.get(*key).and_then(|v| v.as_f64()) {
            return n.to_string();
        }
        if let Some(n) = value.get(*key).and_then(|v| v.as_i64()) {
            return n.to_string();
        }
    }
    String::new()
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn extract_models(value: &Value) -> Vec<ModelInfo> {
    let models = value
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| value.get("models").and_then(|v| v.as_array()))
        .or_else(|| value.as_array());

    models
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let model_id = model
                .get("id")
                .or_else(|| model.get("model"))
                .and_then(|v| v.as_str())?;
            Some(ModelInfo {
                model_id: model_id.to_string(),
                display_name: model
                    .get("display_name")
                    .or_else(|| model.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(model_id)
                    .to_string(),
                owned_by: model
                    .get("owned_by")
                    .and_then(|v| v.as_str())
                    .unwrap_or("glm")
                    .to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_models_reads_openai_list() {
        let models = extract_models(&json!({
            "object": "list",
            "data": [
                { "id": "glm-5.2", "owned_by": "zai" },
                { "id": "glm-4.6" }
            ]
        }));

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model_id, "glm-5.2");
        assert_eq!(models[1].display_name, "glm-4.6");
    }

    #[test]
    fn models_url_uses_coding_plan_base() {
        assert_eq!(
            models_url("https://api.z.ai/api/coding/paas/v4"),
            "https://api.z.ai/api/coding/paas/v4/models"
        );
        assert_eq!(
            models_url("https://api.z.ai/api/coding/paas/v4/chat/completions"),
            "https://api.z.ai/api/coding/paas/v4/models"
        );
    }

    #[test]
    fn parse_balance_response_handles_openai_style_array() {
        let raw = json!({
            "is_available": true,
            "balance_infos": [
                {
                    "currency": "CNY",
                    "total_balance": "12.34",
                    "granted_balance": "5.00",
                    "topped_up_balance": "7.34"
                },
                {
                    "currency": "USD",
                    "total_balance": "0",
                    "granted_balance": "0",
                    "topped_up_balance": "0"
                }
            ]
        });
        let entries = parse_balance_response(&raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].currency, "CNY");
        assert_eq!(entries[0].total_balance, "12.34");
        assert_eq!(entries[0].granted_balance, "5.00");
        assert_eq!(entries[1].currency, "USD");
    }

    #[test]
    fn parse_balance_response_handles_flat_top_level_balance() {
        let raw = json!({
            "balance": "8.50",
            "currency": "USD",
            "granted": "1.00",
            "topped_up": "7.50"
        });
        let entries = parse_balance_response(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].currency, "USD");
        assert_eq!(entries[0].total_balance, "8.50");
        assert_eq!(entries[0].granted_balance, "1.00");
        assert_eq!(entries[0].topped_up_balance, "7.50");
    }

    #[test]
    fn parse_balance_response_handles_data_wrapper() {
        let raw = json!({
            "code": 200,
            "data": {
                "balance_infos": [
                    {
                        "currency": "CNY",
                        "total_balance": "100.00",
                        "granted_balance": "0.00",
                        "topped_up_balance": "100.00"
                    }
                ]
            }
        });
        let entries = parse_balance_response(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].total_balance, "100.00");
    }

    #[test]
    fn parse_balance_response_returns_empty_when_nothing_usable() {
        assert!(parse_balance_response(&json!({"success": false})).is_empty());
        assert!(parse_balance_response(&json!({"balance_infos": []})).is_empty());
    }

    #[test]
    fn looks_like_zai_not_found_catches_spring_proxy_404s() {
        assert!(looks_like_zai_not_found(&json!({
            "code": 500,
            "msg": "404 NOT_FOUND",
            "success": false
        })));
        assert!(looks_like_zai_not_found(&json!({
            "code": "500",
            "msg": "404 NOT_FOUND",
            "success": false
        })));
        assert!(!looks_like_zai_not_found(&json!({
            "success": true,
            "data": {"balance_infos": []}
        })));
    }

    #[test]
    fn primary_host_strips_scheme_and_path() {
        assert_eq!(
            primary_host("https://api.z.ai/api/paas/v4"),
            Some("api.z.ai".to_string())
        );
        assert_eq!(primary_host("https://z.ai"), Some("z.ai".to_string()));
        assert_eq!(primary_host("not-a-url"), Some("not-a-url".to_string()));
    }
}
