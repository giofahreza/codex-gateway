use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use std::time::Duration;

use super::DEFAULT_BASE_URL;

const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

pub fn anthropic_messages_url(base_url: &str) -> String {
    let base = upstream_root(base_url);
    if base.ends_with("/anthropic/v1/messages")
        || base.ends_with("/v1/messages")
        || base.ends_with("/messages")
    {
        return base;
    }
    if base.ends_with("/anthropic/v1") {
        return format!("{}/messages", base);
    }
    if base.ends_with("/anthropic") {
        return format!("{}/v1/messages", base);
    }
    format!("{}/anthropic/v1/messages", base)
}

fn upstream_root(base_url: &str) -> String {
    let mut base = super::api::normalize_base_url(Some(base_url));
    for suffix in [
        "/v1/chat/completions",
        "/chat/completions",
        "/v1/responses",
        "/responses",
        "/v1",
    ] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped.trim_end_matches('/').to_string();
            break;
        }
    }
    if base.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        base
    }
}

pub async fn messages(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !crate::check_api_key(&state, &headers) {
        return anthropic_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Invalid proxy API key",
        );
    }

    let raw: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Invalid request body",
            );
        }
    };

    let model = match raw.get("model").and_then(|v| v.as_str()) {
        Some(model) if !model.trim().is_empty() => model.trim().to_string(),
        _ => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "model is required",
            );
        }
    };

    let accounts = super::accounts::candidate_accounts(&state);
    if accounts.is_empty() {
        return anthropic_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            "No MiniMax accounts configured",
        );
    }

    let wants_stream = crate::source::wants_stream(&headers, &body);
    let prompt_metrics = crate::prompt_metrics_from_request_value(&raw);
    let mut last_error: Option<(StatusCode, String)> = None;

    for (attempt_idx, account) in accounts.iter().enumerate() {
        let context = crate::minimax_usage_context(
            account,
            Some(model.clone()),
            "/minimax/anthropic/v1/messages",
            prompt_metrics.clone(),
        );
        crate::record_minimax_request(&state, &context);

        let base_url = super::api::normalize_base_url(account.base_url.as_deref());
        let url = anthropic_messages_url(&base_url);
        let mut request = state
            .client
            .post(url)
            .body(body.clone())
            .timeout(Duration::from_secs(180));

        for (key, value) in headers.iter() {
            if should_drop_anthropic_incoming_header(key.as_str()) {
                continue;
            }
            request = request.header(key, value);
        }

        request = request
            .header(
                "Authorization",
                format!("Bearer {}", account.api_key.trim()),
            )
            .header("x-api-key", account.api_key.trim())
            .header("Content-Type", "application/json")
            .header(
                "Accept",
                if wants_stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            );
        if !headers.contains_key("anthropic-version") {
            request = request.header("anthropic-version", DEFAULT_ANTHROPIC_VERSION);
        }

        let resp = match request.send().await {
            Ok(resp) => resp,
            Err(err) => {
                let message = format!("MiniMax Anthropic request failed: {}", err);
                crate::record_minimax_error(&state, &context, &message);
                last_error = Some((StatusCode::BAD_GATEWAY, message));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };

        let status = resp.status();
        let out_headers = response_headers(
            resp.headers(),
            if wants_stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        );

        if wants_stream && status.is_success() {
            return stream_messages(state, context, resp, out_headers).await;
        }

        let bytes = match resp.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                let message = format!("MiniMax Anthropic body read failed: {}", err);
                crate::record_minimax_error(&state, &context, &message);
                last_error = Some((StatusCode::BAD_GATEWAY, message));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };

        if !status.is_success() {
            let message = format!(
                "MiniMax Anthropic returned {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            );
            crate::record_minimax_error(&state, &context, &message);
            if attempt_idx + 1 < accounts.len()
                && crate::should_retry_account_error(status, &message)
            {
                last_error = Some((status, message));
                continue;
            }
            return (status, out_headers, bytes).into_response();
        }

        let usage = serde_json::from_slice::<Value>(&bytes)
            .map(|value| crate::usage_metrics_from_response_value(&value))
            .unwrap_or_default();
        crate::record_minimax_success(&state, &context, &usage);
        return (status, out_headers, bytes).into_response();
    }

    let (status, message) = last_error.unwrap_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "All MiniMax accounts failed".to_string(),
        )
    });
    anthropic_error(
        status,
        "api_error",
        &format!("All MiniMax accounts failed; last error: {}", message),
    )
}

async fn stream_messages(
    state: crate::AppState,
    context: crate::UsageContext,
    resp: reqwest::Response,
    headers: HeaderMap,
) -> axum::response::Response {
    let usage_state = state.clone();
    let usage_context = context.clone();
    let stream = async_stream::stream! {
        let mut lifecycle = crate::StreamRequestGuard::new(&usage_state, &usage_context);
        let mut upstream = resp.bytes_stream();
        let mut parser = AnthropicSseUsageTracker::default();
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    parser.push(&bytes);
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
                Err(err) => {
                    let message = format!("MiniMax Anthropic stream read failed: {}", err);
                    crate::record_minimax_error(&usage_state, &usage_context, &message);
                    lifecycle.finish();
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, "stream"));
                    return;
                }
            }
        }
        if let Some(usage) = parser.finish() {
            crate::record_minimax_success(&usage_state, &usage_context, &usage);
        } else {
            crate::record_minimax_success(&usage_state, &usage_context, &crate::UsageMetrics::default());
        }
        lifecycle.finish();
    };

    (StatusCode::OK, headers, Body::from_stream(stream)).into_response()
}

fn should_drop_anthropic_incoming_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    crate::should_drop_incoming_header(&lower) || lower == "x-api-key"
}

fn response_headers(headers: &HeaderMap, fallback_content_type: &'static str) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (key, value) in headers.iter() {
        let lower = key.as_str().to_ascii_lowercase();
        if lower == "content-length" || lower == "content-encoding" || is_hop_header(&lower) {
            continue;
        }
        out.insert(key, value.clone());
    }
    if !out.contains_key(axum::http::header::CONTENT_TYPE) {
        out.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static(fallback_content_type),
        );
    }
    out
}

fn is_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn anthropic_error(
    status: StatusCode,
    error_type: &str,
    message: &str,
) -> axum::response::Response {
    let body = serde_json::to_vec(&serde_json::json!({
        "type": "error",
        "error": {
            "type": error_type,
            "message": message
        }
    }))
    .unwrap_or_default();
    (status, [("Content-Type", "application/json")], body).into_response()
}

#[derive(Default)]
struct AnthropicSseUsageTracker {
    buffer: Vec<u8>,
    last_usage: Option<crate::UsageMetrics>,
}

impl AnthropicSseUsageTracker {
    fn push(&mut self, bytes: &Bytes) {
        self.buffer.extend_from_slice(bytes);
        while let Some((event_end, delimiter_len)) = find_sse_boundary(&self.buffer) {
            let raw = self
                .buffer
                .drain(..event_end + delimiter_len)
                .collect::<Vec<_>>();
            self.absorb_event(&raw[..event_end]);
        }
    }

    fn finish(mut self) -> Option<crate::UsageMetrics> {
        if !self.buffer.is_empty() {
            let raw = std::mem::take(&mut self.buffer);
            self.absorb_event(&raw);
        }
        self.last_usage
    }

    fn absorb_event(&mut self, raw_event: &[u8]) {
        let Some(data) = parse_sse_data(raw_event) else {
            return;
        };
        if data.trim() == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        let usage = crate::usage_metrics_from_response_value(&value);
        if usage.input_tokens > 0 || usage.output_tokens > 0 || usage.total_tokens > 0 {
            self.last_usage = Some(usage);
        }
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| (idx, 4))
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|idx| (idx, 2))
        })
}

fn parse_sse_data(raw_event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw_event);
    let mut data_lines = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_messages_url_uses_official_anthropic_route() {
        assert_eq!(
            anthropic_messages_url("https://api.minimax.io"),
            "https://api.minimax.io/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.minimax.io/v1"),
            "https://api.minimax.io/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.minimax.io/anthropic"),
            "https://api.minimax.io/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.minimax.io/anthropic/v1"),
            "https://api.minimax.io/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.minimaxi.com/anthropic"),
            "https://api.minimaxi.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn anthropic_messages_url_converts_codex_endpoint_base() {
        assert_eq!(
            anthropic_messages_url("https://api.minimax.io/v1/responses"),
            "https://api.minimax.io/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.minimax.io/v1/chat/completions"),
            "https://api.minimax.io/anthropic/v1/messages"
        );
    }
}
