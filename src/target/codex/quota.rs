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
        .header("User-Agent", "codex_cli_rs/0.76.0 (Debian 13.0.0; x86_64) WindowsTerminal");
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

    let usage_nodes = v
        .get("usage")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let mut code_gen = QuotaRateSummary::default();
    let mut code_review = QuotaRateSummary::default();

    for node in usage_nodes {
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
        let used = node.get("used_percent").and_then(|x| x.as_f64()).or_else(|| {
            let used = node.get("used").and_then(|x| x.as_f64())?;
            let limit = node.get("limit").and_then(|x| x.as_f64())?;
            if limit > 0.0 {
                Some((used / limit) * 100.0)
            } else {
                None
            }
        });
        let reset_label = node
            .get("reset_label")
            .or_else(|| node.get("resetAtLabel"))
            .or_else(|| node.get("resets_at"))
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        let window = QuotaWindowSummary {
            used_percent: used,
            reset_label,
        };
        let is_weekly = period.contains("week");
        let is_five_hour = period.contains("5h") || period.contains("five") || period.contains("hour");
        let is_code_review = cat.contains("review");
        let is_code_gen = cat.contains("generation") || cat.contains("gen");

        if is_code_gen || (!is_code_review && !is_code_gen) {
            if is_weekly {
                code_gen.weekly = Some(window.clone());
            } else if is_five_hour || !is_weekly {
                code_gen.five_hour = Some(window.clone());
            }
        }
        if is_code_review {
            if is_weekly {
                code_review.weekly = Some(window.clone());
            } else if is_five_hour || !is_weekly {
                code_review.five_hour = Some(window);
            }
        }
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
