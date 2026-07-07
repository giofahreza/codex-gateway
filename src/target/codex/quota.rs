use serde::{Deserialize, Serialize};
use std::time::Duration;

const CHATGPT_BACKEND_API_BASE: &str = "https://chatgpt.com/backend-api";
const CODEX_USER_AGENT: &str = "codex_cli_rs/0.76.0 (Debian 13.0.0; x86_64) WindowsTerminal";

#[derive(Clone)]
pub struct QuotaCacheEntry {
    pub fetched_at: std::time::Instant,
    pub summary: QuotaSummary,
    pub error: Option<String>,
}

#[derive(Default, Clone, Serialize)]
pub struct QuotaSummary {
    pub label: String,
    pub account_id: String,
    pub plan_type: String,
    pub code_generation: QuotaRateSummary,
    pub code_review: QuotaRateSummary,
    pub additional_rate_limits: Vec<AdditionalRateLimitSummary>,
    pub rate_limit_reset_credits: Option<RateLimitResetCreditsSummary>,
    pub models: Vec<ModelSummary>,
}

#[derive(Default, Clone, Serialize)]
pub struct QuotaRateSummary {
    pub five_hour: Option<QuotaWindowSummary>,
    pub weekly: Option<QuotaWindowSummary>,
}

#[derive(Default, Clone, Serialize)]
pub struct QuotaWindowSummary {
    pub used_percent: Option<f64>,
    pub reset_label: String,
}

#[derive(Default, Clone, Serialize)]
pub struct AdditionalRateLimitSummary {
    pub display_name: String,
    pub five_hour: Option<QuotaWindowSummary>,
    pub weekly: Option<QuotaWindowSummary>,
}

#[derive(Default, Clone, Serialize)]
pub struct ModelSummary {
    pub model_id: String,
    pub display_name: String,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct RateLimitResetCreditsSummary {
    pub available_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<Vec<RateLimitResetCredit>>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct RateLimitResetCredit {
    pub id: String,
    pub reset_type: String,
    pub status: String,
    pub granted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Deserialize)]
struct RateLimitResetCreditsDetails {
    #[serde(default)]
    credits: Vec<RateLimitResetCredit>,
    available_count: i64,
}

#[derive(Deserialize)]
struct ConsumeRateLimitResetCreditResponse {
    code: String,
    #[serde(default)]
    windows_reset: i64,
}

#[derive(Serialize)]
struct ConsumeRateLimitResetCreditRequest<'a> {
    redeem_request_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    credit_id: Option<&'a str>,
}

#[derive(Default, Deserialize)]
pub struct ConsumeRateLimitResetForm {
    pub file_name: Option<String>,
    pub label: Option<String>,
    pub account_id: Option<String>,
    pub credit_id: Option<String>,
    pub idempotency_key: Option<String>,
}

pub async fn get_quota_summaries(state: &crate::AppState) -> Vec<serde_json::Value> {
    let tokens = state.tokens.lock().unwrap().clone();
    {
        let mut cache = state.quota_cache.lock().unwrap();
        if cache.len() != tokens.len() {
            *cache = vec![None; tokens.len()];
        }
    }
    let now = std::time::Instant::now();
    let mut results = Vec::with_capacity(tokens.len());
    for (idx, token) in tokens.iter().enumerate() {
        let cached = {
            let cache = state.quota_cache.lock().unwrap();
            cache.get(idx).cloned().flatten()
        };
        let entry = if let Some(c) = cached {
            if now.duration_since(c.fetched_at).as_secs() < 60 {
                c
            } else {
                let fetched = fetch_codex_quota(state, token).await;
                let mut cache = state.quota_cache.lock().unwrap();
                if cache.len() <= idx {
                    cache.resize(idx + 1, None);
                }
                cache[idx] = Some(fetched.clone());
                fetched
            }
        } else {
            let fetched = fetch_codex_quota(state, token).await;
            let mut cache = state.quota_cache.lock().unwrap();
            if cache.len() <= idx {
                cache.resize(idx + 1, None);
            }
            cache[idx] = Some(fetched.clone());
            fetched
        };
        if let Some(err) = entry.error {
            results.push(serde_json::json!({
                "label": token.label,
                "account_id": token.account_id.clone().unwrap_or_default(),
                "file_name": token.file_name.clone().unwrap_or_default(),
                "error": err
            }));
        } else {
            results.push(serde_json::json!({
                "label": entry.summary.label,
                "account_id": entry.summary.account_id,
                "file_name": token.file_name.clone().unwrap_or_default(),
                "plan_type": entry.summary.plan_type,
                "code_generation": entry.summary.code_generation,
                "code_review": entry.summary.code_review,
                "additional_rate_limits": entry.summary.additional_rate_limits,
                "rate_limit_reset_credits": entry.summary.rate_limit_reset_credits,
                "models": entry.summary.models
            }));
        }
    }
    results
}

async fn fetch_codex_quota(
    state: &crate::AppState,
    token: &super::tokens::UpstreamToken,
) -> QuotaCacheEntry {
    let req = authenticated_codex_request(
        state
            .client
            .get(wham_url(&state.cfg.upstream_base, "usage")),
        token,
    );

    let resp = match req.timeout(Duration::from_secs(30)).send().await {
        Ok(r) => r,
        Err(err) => {
            return QuotaCacheEntry {
                fetched_at: std::time::Instant::now(),
                summary: QuotaSummary::default(),
                error: Some(err.to_string()),
            }
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return QuotaCacheEntry {
            fetched_at: std::time::Instant::now(),
            summary: QuotaSummary::default(),
            error: Some(format!("status {}: {}", status.as_u16(), body)),
        };
    }
    let body = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return QuotaCacheEntry {
                fetched_at: std::time::Instant::now(),
                summary: QuotaSummary::default(),
                error: Some("failed to parse quota response".to_string()),
            }
        }
    };

    let plan_type = v
        .get("plan_type")
        .or_else(|| v.get("planType"))
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();

    let mut code_gen = extract_rate_summary(v.get("rate_limit"));
    let mut code_review = extract_rate_summary(v.get("code_review_rate_limit"));
    let additional_rate_limits = extract_additional_rate_limits(v.get("additional_rate_limits"));

    // Fallback for alternate response shape that sends usage nodes as arrays.
    if code_gen.five_hour.is_none()
        && code_gen.weekly.is_none()
        && code_review.five_hour.is_none()
        && code_review.weekly.is_none()
    {
        let usage_nodes = v
            .get("usage")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        let (fallback_gen, fallback_review) = extract_from_usage_nodes(&usage_nodes);
        code_gen = fallback_gen;
        code_review = fallback_review;
    }

    let rate_limit_reset_credits = fetch_rate_limit_reset_credits(state, token)
        .await
        .ok()
        .or_else(|| extract_rate_limit_reset_credits_summary(v.get("rate_limit_reset_credits")));

    let summary = QuotaSummary {
        label: token.label.clone(),
        account_id: token.account_id.clone().unwrap_or_default(),
        plan_type,
        code_generation: code_gen,
        code_review,
        additional_rate_limits,
        rate_limit_reset_credits,
        models: fetch_codex_models(state, token).await.unwrap_or_default(),
    };
    QuotaCacheEntry {
        fetched_at: std::time::Instant::now(),
        summary,
        error: None,
    }
}

async fn fetch_codex_models(
    state: &crate::AppState,
    token: &super::tokens::UpstreamToken,
) -> Result<Vec<ModelSummary>, String> {
    let req = authenticated_codex_request(
        state.client.get(super::gateway::build_upstream_url(
            &state.cfg.upstream_base,
            "models",
            Some("client_version=1.0.0"),
        )),
        token,
    );

    let resp = req
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("status {}: {}", status.as_u16(), body));
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| "failed to parse models response".to_string())?;
    Ok(parse_models_response(&value))
}

pub async fn consume_rate_limit_reset_credit(
    state: &crate::AppState,
    form: ConsumeRateLimitResetForm,
    fallback_idempotency_key: String,
) -> Result<serde_json::Value, String> {
    let token = select_token_for_reset(state, &form)?;
    let idempotency_key = form
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or(fallback_idempotency_key);
    let credit_id = form
        .credit_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let url = wham_url(&state.cfg.upstream_base, "rate-limit-reset-credits/consume");
    let req = authenticated_codex_request(state.client.post(&url), &token).json(
        &ConsumeRateLimitResetCreditRequest {
            redeem_request_id: &idempotency_key,
            credit_id,
        },
    );
    let response: ConsumeRateLimitResetCreditResponse = execute_json(req, "consume reset credit")
        .await
        .map_err(|err| format!("failed to consume rate limit reset: {}", err))?;

    {
        let mut cache = state.quota_cache.lock().unwrap();
        for entry in cache.iter_mut() {
            *entry = None;
        }
    }

    let remaining = fetch_rate_limit_reset_credits(state, &token).await.ok();
    let outcome = reset_outcome(&response.code);
    let message = reset_outcome_message(&response.code, response.windows_reset);
    Ok(serde_json::json!({
        "ok": true,
        "outcome": outcome,
        "code": response.code,
        "windows_reset": response.windows_reset,
        "idempotency_key": idempotency_key,
        "rate_limit_reset_credits": remaining,
        "message": message
    }))
}

async fn fetch_rate_limit_reset_credits(
    state: &crate::AppState,
    token: &super::tokens::UpstreamToken,
) -> Result<RateLimitResetCreditsSummary, String> {
    let url = wham_url(&state.cfg.upstream_base, "rate-limit-reset-credits");
    let req = authenticated_codex_request(state.client.get(&url), token);
    let details: RateLimitResetCreditsDetails = execute_json(req, "fetch reset credits").await?;
    Ok(RateLimitResetCreditsSummary {
        available_count: details.available_count,
        credits: Some(details.credits),
    })
}

async fn execute_json<T: for<'de> Deserialize<'de>>(
    req: reqwest::RequestBuilder,
    context: &str,
) -> Result<T, String> {
    let resp = req
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("{} request failed: {}", context, err))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|err| format!("{} body read failed: {}", context, err))?;
    if !status.is_success() {
        return Err(format!(
            "{} returned {}: {}",
            context,
            status.as_u16(),
            body
        ));
    }
    serde_json::from_str(&body).map_err(|err| format!("{} JSON parse failed: {}", context, err))
}

fn select_token_for_reset(
    state: &crate::AppState,
    form: &ConsumeRateLimitResetForm,
) -> Result<super::tokens::UpstreamToken, String> {
    let tokens = state.tokens.lock().unwrap().clone();
    let file_name = form
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let account_id = form
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let label = form
        .label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut matches = tokens.into_iter().filter(|token| {
        if let Some(file_name) = file_name {
            return token.file_name.as_deref() == Some(file_name);
        }
        if let Some(account_id) = account_id {
            return token.account_id.as_deref() == Some(account_id);
        }
        if let Some(label) = label {
            return token.label == label;
        }
        false
    });

    let token = matches
        .next()
        .ok_or_else(|| "matching Codex account was not found".to_string())?;
    if matches.next().is_some() {
        return Err("multiple Codex accounts matched; include file_name or account_id".to_string());
    }
    Ok(token)
}

fn authenticated_codex_request(
    req: reqwest::RequestBuilder,
    token: &super::tokens::UpstreamToken,
) -> reqwest::RequestBuilder {
    let mut req = req
        .header("Authorization", format!("Bearer {}", token.token))
        .header("Content-Type", "application/json")
        .header("User-Agent", CODEX_USER_AGENT);
    if let Some(account_id) = token.account_id.as_ref() {
        if !account_id.trim().is_empty() {
            req = req.header("Chatgpt-Account-Id", account_id);
        }
    }
    req
}

fn wham_url(upstream_base: &str, path: &str) -> String {
    let base = chatgpt_backend_base(upstream_base);
    format!("{}/wham/{}", base, path.trim_start_matches('/'))
}

fn chatgpt_backend_base(upstream_base: &str) -> String {
    let trimmed = upstream_base.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return CHATGPT_BACKEND_API_BASE.to_string();
    }
    if let Some(base) = trimmed.strip_suffix("/codex") {
        return base.to_string();
    }
    if trimmed.ends_with("/backend-api") {
        return trimmed.to_string();
    }
    CHATGPT_BACKEND_API_BASE.to_string()
}

fn reset_outcome(code: &str) -> &'static str {
    match code {
        "reset" => "reset",
        "nothing_to_reset" => "nothing_to_reset",
        "no_credit" => "no_credit",
        "already_redeemed" => "already_redeemed",
        _ => "unknown",
    }
}

fn reset_outcome_message(code: &str, windows_reset: i64) -> String {
    match code {
        "reset" => {
            if windows_reset > 0 {
                format!("Usage reset; {} rate-limit window(s) reset.", windows_reset)
            } else {
                "Usage reset.".to_string()
            }
        }
        "nothing_to_reset" => "Your usage does not need a reset right now.".to_string(),
        "no_credit" => "No usage limit reset credits are available.".to_string(),
        "already_redeemed" => "This reset request was already redeemed.".to_string(),
        other => format!("Reset request finished with outcome: {}", other),
    }
}

fn parse_models_response(value: &serde_json::Value) -> Vec<ModelSummary> {
    value
        .get("models")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let model_id = model
                .get("slug")
                .or_else(|| model.get("id"))
                .or_else(|| model.get("model_id"))
                .and_then(|value| value.as_str())?
                .trim();
            if model_id.is_empty() {
                return None;
            }
            let display_name = model
                .get("display_name")
                .or_else(|| model.get("name"))
                .and_then(|value| value.as_str())
                .unwrap_or(model_id)
                .to_string();
            Some(ModelSummary {
                model_id: model_id.to_string(),
                display_name,
            })
        })
        .collect()
}

fn extract_rate_limit_reset_credits_summary(
    value: Option<&serde_json::Value>,
) -> Option<RateLimitResetCreditsSummary> {
    let value = value?;
    let available_count = value
        .get("available_count")
        .or_else(|| value.get("availableCount"))
        .and_then(|value| value.as_i64())?;
    Some(RateLimitResetCreditsSummary {
        available_count,
        credits: None,
    })
}

fn extract_rate_summary(rate_limit: Option<&serde_json::Value>) -> QuotaRateSummary {
    let Some(serde_json::Value::Object(obj)) = rate_limit else {
        return QuotaRateSummary::default();
    };
    let five_hour = obj
        .get("primary_window")
        .and_then(|w| extract_window_summary(w, Some("5h")));
    let weekly = obj
        .get("secondary_window")
        .and_then(|w| extract_window_summary(w, Some("weekly")));
    QuotaRateSummary { five_hour, weekly }
}

fn extract_additional_rate_limits(
    value: Option<&serde_json::Value>,
) -> Vec<AdditionalRateLimitSummary> {
    let Some(items) = value.and_then(|value| value.as_array()) else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            let display_name = item
                .get("display_name")
                .or_else(|| item.get("displayName"))
                .or_else(|| item.get("limit_name"))
                .or_else(|| item.get("limitName"))
                .or_else(|| item.get("name"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_string();
            let rate_limit = item.get("rate_limit").or_else(|| item.get("rateLimit"))?;
            let summary = extract_rate_summary(Some(rate_limit));
            Some(AdditionalRateLimitSummary {
                display_name,
                five_hour: summary.five_hour,
                weekly: summary.weekly,
            })
        })
        .collect()
}

fn extract_window_summary(
    window: &serde_json::Value,
    default_bucket: Option<&str>,
) -> Option<QuotaWindowSummary> {
    let used_percent = window
        .get("used_percent")
        .or_else(|| window.get("usedPercent"))
        .and_then(|x| x.as_f64())
        .or_else(|| {
            let used = window.get("used").and_then(|x| x.as_f64())?;
            let limit = window.get("limit").and_then(|x| x.as_f64())?;
            if limit > 0.0 {
                Some((used / limit) * 100.0)
            } else {
                None
            }
        });

    let reset_label = window
        .get("reset_label")
        .or_else(|| window.get("resetAtLabel"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            let seconds = window
                .get("reset_after_seconds")
                .or_else(|| window.get("resetAfterSeconds"))
                .and_then(|x| x.as_i64())?;
            Some(format_reset_after(seconds, default_bucket))
        })
        .unwrap_or_default();

    Some(QuotaWindowSummary {
        used_percent,
        reset_label,
    })
}

fn format_reset_after(seconds: i64, bucket: Option<&str>) -> String {
    if seconds <= 0 {
        return "reset now".to_string();
    }
    let d = Duration::from_secs(seconds as u64);
    let days = d.as_secs() / 86_400;
    let hours = (d.as_secs() % 86_400) / 3_600;
    let mins = (d.as_secs() % 3_600) / 60;
    match bucket {
        Some("weekly") => {
            if days > 0 {
                format!("resets in {}d {}h", days, hours)
            } else if hours > 0 {
                format!("resets in {}h {}m", hours, mins)
            } else {
                format!("resets in {}m", mins)
            }
        }
        _ => {
            if hours > 0 {
                format!("resets in {}h {}m", hours, mins)
            } else {
                format!("resets in {}m", mins)
            }
        }
    }
}

fn extract_from_usage_nodes(nodes: &[serde_json::Value]) -> (QuotaRateSummary, QuotaRateSummary) {
    let mut code_gen = QuotaRateSummary::default();
    let mut code_review = QuotaRateSummary::default();
    for node in nodes {
        let cat = node
            .get("category")
            .or_else(|| node.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let period = node
            .get("period")
            .or_else(|| node.get("window"))
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let window = extract_window_summary(node, None).unwrap_or_default();
        let is_weekly = period.contains("week");
        let is_code_review = cat.contains("review");
        let is_code_gen = cat.contains("generation") || cat.contains("gen");

        if is_code_gen || (!is_code_review && !is_code_gen) {
            if is_weekly {
                code_gen.weekly = Some(window.clone());
            } else {
                code_gen.five_hour = Some(window.clone());
            }
        }
        if is_code_review {
            if is_weekly {
                code_review.weekly = Some(window.clone());
            } else {
                code_review.five_hour = Some(window);
            }
        }
    }
    (code_gen, code_review)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_additional_codex_spark_rate_limit() {
        let limits = extract_additional_rate_limits(Some(&json!([
            {
                "limit_name": "GPT-5.3-Codex-Spark",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 12.5,
                        "reset_after_seconds": 1800
                    },
                    "secondary_window": {
                        "used_percent": 34.0,
                        "reset_after_seconds": 86400
                    }
                }
            }
        ])));

        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].display_name, "GPT-5.3-Codex-Spark");
        assert_eq!(
            limits[0]
                .five_hour
                .as_ref()
                .and_then(|window| window.used_percent),
            Some(12.5)
        );
        assert_eq!(
            limits[0]
                .weekly
                .as_ref()
                .and_then(|window| window.used_percent),
            Some(34.0)
        );
    }

    #[test]
    fn builds_wham_reset_credit_urls_from_codex_upstream_base() {
        assert_eq!(
            wham_url(
                "https://chatgpt.com/backend-api/codex",
                "rate-limit-reset-credits/consume"
            ),
            "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume"
        );
        assert_eq!(
            wham_url(
                "https://chatgpt.com/backend-api",
                "rate-limit-reset-credits"
            ),
            "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits"
        );
    }

    #[test]
    fn extracts_reset_credit_summary_from_usage_payload() {
        let summary = extract_rate_limit_reset_credits_summary(Some(&json!({
            "available_count": 3
        })))
        .expect("reset summary");

        assert_eq!(summary.available_count, 3);
        assert!(summary.credits.is_none());
    }
}
