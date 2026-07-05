use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

const CACHE_TTL_SECS: u64 = 60;
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
    pub raw: Value,
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
            if now.duration_since(cached.fetched_at).as_secs() < CACHE_TTL_SECS {
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
            results.push(json!({
                "label": entry.summary.label,
                "file_name": entry.summary.file_name,
                "account_type": entry.summary.account_type,
                "status_msg": entry.summary.status_msg,
                "available_models": entry.summary.available_models,
                "raw": entry.summary.raw,
            }));
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
    let models_result = fetch_models(client, account).await;
    let (models, raw, error) = match models_result {
        Ok((models, raw)) => (models, raw, None),
        Err(err) => (Vec::new(), Value::Null, Some(err)),
    };

    let summary = QuotaSummary {
        label: account.label.clone(),
        file_name: account.file_name.clone().unwrap_or_default(),
        account_type: account.normalized_account_type(),
        status_msg: "Z.AI does not expose a stable GLM quota endpoint here; this card shows the live model catalog and gateway-recorded usage.".to_string(),
        available_models: models,
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
}
