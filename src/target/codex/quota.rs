use serde::Serialize;
use std::time::Duration;

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
                "code_review": entry.summary.code_review
            }));
        }
    }
    results
}

async fn fetch_codex_quota(
    state: &crate::AppState,
    token: &super::tokens::UpstreamToken,
) -> QuotaCacheEntry {
    let mut req = state
        .client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {}", token.token))
        .header("Content-Type", "application/json")
        .header(
            "User-Agent",
            "codex_cli_rs/0.76.0 (Debian 13.0.0; x86_64) WindowsTerminal",
        );
    if let Some(account_id) = token.account_id.as_ref() {
        if !account_id.trim().is_empty() {
            req = req.header("Chatgpt-Account-Id", account_id);
        }
    }

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

    let summary = QuotaSummary {
        label: token.label.clone(),
        account_id: token.account_id.clone().unwrap_or_default(),
        plan_type,
        code_generation: code_gen,
        code_review,
    };
    QuotaCacheEntry {
        fetched_at: std::time::Instant::now(),
        summary,
        error: None,
    }
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
