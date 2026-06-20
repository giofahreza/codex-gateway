//! Native OpenAI Responses API pass-through to MiniMax.
//!
//! MiniMax exposes an OpenAI Responses-API-compatible endpoint at
//! `POST /v1/responses`
//! (https://platform.minimax.io/docs/api-reference/responses-create). The
//! field names there line up exactly with the Codex SDK's request body,
//! so we can forward the request largely as-is instead of doing the
//! lossy Responses → Chat-Completions translation.
//!
//! Two fields that need special handling:
//!
//!   * `reasoning` — MiniMax-M3 only enters Adaptive Thinking when this
//!     field is set to a non-`none` value. Without it, the model produces
//!     very short answers and the Codex agent loop sees the model
//!     "stop before task done". We default it to `medium` for M3 when
//!     the client did not set it.
//!   * `tools` — the Codex SDK registers an `apply_patch` tool that
//!     MiniMax does not support. We strip it before forwarding.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

use super::accounts::MiniMaxAccount;
use super::api::{
    build_chat_completions_payload, chat_completion_to_responses, chat_completions_url,
    stream_chat_completions,
};
use super::DEFAULT_BASE_URL;

const RESPONSES_PATH: &str = "/v1/responses";

/// Normalize the account's configured base URL to a bare host (no trailing
/// slash).
pub fn normalize_base_url(base_url: Option<&str>) -> String {
    base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/')
        .to_string()
}

fn responses_url(base_url: &str) -> String {
    let base = normalize_base_url(Some(base_url));
    if base.ends_with(RESPONSES_PATH) {
        return base;
    }
    if base.ends_with("/responses") {
        return base;
    }
    if base.ends_with("/v1") {
        return format!("{}/responses", base);
    }
    format!("{}{}", base, RESPONSES_PATH)
}

/// When the configured `base_url` explicitly points at the chat-completions
/// endpoint we keep using that path. Otherwise the request goes to the
/// native `/v1/responses`.
pub fn use_native_responses(account_base_url: Option<&str>) -> bool {
    let normalized = normalize_base_url(account_base_url);
    !normalized.ends_with("/chat/completions")
}

pub async fn responses(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !crate::check_api_key(&headers, &state.cfg.proxy_api_key) {
        return (
            StatusCode::UNAUTHORIZED,
            [("Content-Type", "application/json")],
            crate::source::v1::response::openai_error_body(
                "Invalid proxy API key",
                "authentication_error",
                Some("invalid_api_key"),
            ),
        )
            .into_response();
    }

    let raw: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    "Invalid request body",
                    "invalid_request_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let model = match raw.get("model").and_then(|v| v.as_str()) {
        Some(model) if !model.trim().is_empty() => model.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    "model is required",
                    "invalid_request_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let account = match super::accounts::pick_account(&state) {
        Some(account) => account,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    "No MiniMax accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let base_url = normalize_base_url(account.base_url.as_deref());
    if use_native_responses(Some(&base_url)) {
        native_responses(&state, &account, &base_url, &model, &raw, &headers, &body).await
    } else {
        chat_completions_responses(
            &state, &account, &base_url, &model, &raw, &headers, &body,
        )
        .await
    }
}

async fn native_responses(
    state: &crate::AppState,
    account: &MiniMaxAccount,
    base_url: &str,
    model: &str,
    raw: &Value,
    incoming_headers: &HeaderMap,
    body: &Bytes,
) -> axum::response::Response {
    let context = crate::minimax_usage_context(
        account,
        Some(model.to_string()),
        "/minimax/v1/responses",
        crate::prompt_metrics_from_request_value(raw),
    );
    crate::record_minimax_request(state, &context);

    let payload = match build_native_payload(raw) {
        Ok(payload) => payload,
        Err(err) => {
            crate::record_minimax_error(state, &context, &err);
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &err,
                    "invalid_request_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let wants_stream = crate::source::wants_stream(incoming_headers, body);
    let url = responses_url(base_url);
    let mut request = state
        .client
        .post(&url)
        .header(
            "Authorization",
            format!("Bearer {}", account.api_key.trim()),
        )
        .header("Content-Type", "application/json")
        .header(
            "Accept",
            if wants_stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .timeout(Duration::from_secs(180))
        .body(payload.to_string());

    if !incoming_headers.contains_key("accept-encoding") {
        request = request.header("Accept-Encoding", "identity");
    }
    if !incoming_headers.contains_key("user-agent") {
        request = request.header("User-Agent", "codex-gateway/1.0");
    }

    let resp = match request.send().await {
        Ok(resp) => resp,
        Err(err) => {
            let message = format!("MiniMax responses request failed: {}", err);
            crate::record_minimax_error(state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &message,
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let message = format!("MiniMax returned {}: {}", status, text);
        crate::record_minimax_error(state, &context, &message);
        return (
            StatusCode::BAD_GATEWAY,
            [("Content-Type", "application/json")],
            crate::source::v1::response::openai_error_body(
                &message,
                "server_error",
                None,
            ),
        )
            .into_response();
    }

    if wants_stream {
        return stream_native_responses(state, &context, resp).await;
    }

    let text = match resp.text().await {
        Ok(text) => text,
        Err(err) => {
            let message = format!("MiniMax body read failed: {}", err);
            crate::record_minimax_error(state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &message,
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("invalid MiniMax response: {}", err);
            crate::record_minimax_error(state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &message,
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let usage = crate::usage_metrics_from_response_value(&value);
    crate::record_minimax_success(state, &context, &usage);

    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        Bytes::from(text),
    )
        .into_response()
}

async fn stream_native_responses(
    state: &crate::AppState,
    context: &crate::UsageContext,
    resp: reqwest::Response,
) -> axum::response::Response {
    let usage_state = state.clone();
    let usage_context = context.clone();
    let mut upstream = resp.bytes_stream();
    let mut sse_buffer: Vec<u8> = Vec::new();
    let mut last_response: Option<Value> = None;
    let stream = async_stream::stream! {
        while let Some(chunk) = upstream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(err) => {
                    let message = format!("MiniMax stream read failed: {}", err);
                    crate::record_minimax_error(&usage_state, &usage_context, &message);
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                        "event: error\ndata: {{\"message\":\"{}\"}}\n\n",
                        message.replace('"', "\\\"")
                    )));
                    return;
                }
            };
            sse_buffer.extend_from_slice(&bytes);
            while let Some((event_end, delimiter_len)) = find_sse_boundary(&sse_buffer) {
                let raw_event: Vec<u8> = sse_buffer.drain(..event_end + delimiter_len).collect();
                if let Some(data) = parse_sse_data(&raw_event[..event_end]) {
                    if data.trim() == "[DONE]" {
                        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(&data) {
                        if let Some(obj) = value.as_object() {
                            if obj.get("type").and_then(|v| v.as_str())
                                == Some("response.completed")
                            {
                                if let Some(response) = obj.get("response").cloned() {
                                    last_response = Some(response);
                                }
                            }
                        }
                    }
                    yield Ok(Bytes::from(raw_event));
                }
            }
        }
        if let Some(response) = last_response {
            let usage = crate::usage_metrics_from_response_value(&response);
            crate::record_minimax_success(&usage_state, &usage_context, &usage);
        }
    };

    (
        StatusCode::OK,
        [
            ("Content-Type", "text/event-stream"),
            ("Cache-Control", "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
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
    let mut data_lines: Vec<String> = Vec::new();
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

/// Build the request body we forward to MiniMax `/v1/responses`. Most
/// fields are forwarded verbatim; the transformations are:
///
///   * drop Codex-specific fields (`store`, `include`, `user`,
///     `safety_identifier`, `parallel_tool_calls`, `truncation`).
///   * strip the `apply_patch` tool MiniMax does not support.
///   * default `reasoning` to `medium` for M3 when the client did not
///     set it.
fn build_native_payload(raw: &Value) -> Result<Value, String> {
    let mut out = serde_json::Map::new();

    for key in &[
        "model",
        "input",
        "instructions",
        "max_output_tokens",
        "temperature",
        "top_p",
        "stream",
        "metadata",
        "prompt_cache_key",
        "text",
        "service_tier",
        "tool_choice",
    ] {
        if let Some(value) = raw.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }

    if let Some(tools_value) = filter_tools(raw.get("tools")) {
        out.insert("tools".to_string(), tools_value);
    }

    if let Some(reasoning) = normalize_reasoning(raw) {
        out.insert("reasoning".to_string(), reasoning);
    }

    serde_json::to_value(out).map_err(|e| e.to_string())
}

fn filter_tools(raw: Option<&Value>) -> Option<Value> {
    let tools = raw?.as_array()?;
    let mut out = Vec::new();
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| {
                tool.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");
        if name == "apply_patch" {
            continue;
        }
        out.push(tool.clone());
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

fn normalize_reasoning(raw: &Value) -> Option<Value> {
    if let Some(reasoning) = raw.get("reasoning") {
        if let Some(obj) = reasoning.as_object() {
            let effort = obj
                .get("effort")
                .and_then(|v| v.as_str())
                .unwrap_or("none");
            if matches!(effort, "none") {
                return Some(json!({ "effort": "none" }));
            }
            return Some(reasoning.clone());
        }
    }

    if let Some(effort) = raw.get("reasoning_effort").and_then(|v| v.as_str()) {
        return Some(json!({ "effort": effort }));
    }

    let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if model.eq_ignore_ascii_case("MiniMax-M3") {
        return Some(json!({ "effort": "medium" }));
    }

    None
}

/// Chat-Completions fallback path. Used when the configured account's
/// `base_url` explicitly points at the chat-completions endpoint.
async fn chat_completions_responses(
    state: &crate::AppState,
    account: &MiniMaxAccount,
    base_url: &str,
    model: &str,
    raw: &Value,
    headers: &HeaderMap,
    body: &Bytes,
) -> axum::response::Response {
    let context = crate::minimax_usage_context(
        account,
        Some(model.to_string()),
        "/minimax/v1/chat/completions",
        crate::prompt_metrics_from_request_value(raw),
    );
    crate::record_minimax_request(state, &context);

    let wants_stream = crate::source::wants_stream(headers, body);

    let chat_payload = match build_chat_completions_payload(raw, model) {
        Ok(payload) => payload,
        Err(err) => {
            crate::record_minimax_error(state, &context, &err);
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &err,
                    "invalid_request_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    if wants_stream {
        return stream_chat_completions(
            state,
            &account.api_key,
            base_url,
            &chat_payload,
            &context,
            model,
            headers,
        )
        .await;
    }

    let resp = match state
        .client
        .post(chat_completions_url(base_url))
        .header(
            "Authorization",
            format!("Bearer {}", account.api_key.trim()),
        )
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(chat_payload.to_string())
        .timeout(Duration::from_secs(180))
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            let message = format!("MiniMax request failed: {}", err);
            crate::record_minimax_error(state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &message,
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let status = resp.status();
    let text = match resp.text().await {
        Ok(text) => text,
        Err(err) => {
            let message = format!("MiniMax body read failed: {}", err);
            crate::record_minimax_error(state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &message,
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    if !status.is_success() {
        crate::record_minimax_error(
            state,
            &context,
            format!("MiniMax returned {}: {}", status, text),
        );
        return (
            StatusCode::BAD_GATEWAY,
            [("Content-Type", "application/json")],
            crate::source::v1::response::openai_error_body(
                &format!("MiniMax returned {}: {}", status, text),
                "server_error",
                None,
            ),
        )
            .into_response();
    }

    let chat_response: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("invalid MiniMax response: {}", err);
            crate::record_minimax_error(state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &message,
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let response = chat_completion_to_responses(&chat_response, model);
    let usage = crate::usage_metrics_from_response_value(&response);
    crate::record_minimax_success(state, &context, &usage);

    let body = serde_json::to_vec(&response).unwrap_or_default();
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_base_url_defaults_to_official() {
        assert_eq!(normalize_base_url(None), DEFAULT_BASE_URL);
        assert_eq!(
            normalize_base_url(Some("https://example.com/")),
            "https://example.com"
        );
    }

    #[test]
    fn responses_url_handles_known_shapes() {
        assert_eq!(
            responses_url("https://api.minimaxi.chat"),
            "https://api.minimaxi.chat/v1/responses"
        );
        assert_eq!(
            responses_url("https://api.minimaxi.chat/v1"),
            "https://api.minimaxi.chat/v1/responses"
        );
        assert_eq!(
            responses_url("https://example.com/v1/responses"),
            "https://example.com/v1/responses"
        );
    }

    #[test]
    fn use_native_responses_skips_chat_completions_url() {
        assert!(use_native_responses(None));
        assert!(use_native_responses(Some("https://api.minimaxi.chat")));
        assert!(use_native_responses(Some("https://api.minimaxi.chat/v1")));
        assert!(!use_native_responses(Some(
            "https://example.com/v1/chat/completions"
        )));
    }

    #[test]
    fn build_native_payload_strips_codex_fields() {
        let raw = json!({
            "model": "MiniMax-M3",
            "input": [{"role": "user", "content": "hi"}],
            "store": true,
            "include": ["reasoning.encrypted_content"],
            "parallel_tool_calls": true,
            "truncation": "auto",
            "user": "u1",
            "safety_identifier": "abc",
            "metadata": {"k": "v"}
        });
        let out = build_native_payload(&raw).unwrap();
        assert_eq!(out["model"], "MiniMax-M3");
        assert!(out.get("input").is_some());
        assert!(out.get("metadata").is_some());
        assert!(out.get("store").is_none());
        assert!(out.get("include").is_none());
        assert!(out.get("parallel_tool_calls").is_none());
        assert!(out.get("truncation").is_none());
        assert!(out.get("user").is_none());
        assert!(out.get("safety_identifier").is_none());
    }

    #[test]
    fn build_native_payload_filters_apply_patch() {
        let raw = json!({
            "model": "MiniMax-M3",
            "input": "hi",
            "tools": [
                {"type": "function", "name": "apply_patch", "parameters": {"type": "object"}},
                {"type": "function", "name": "shell", "parameters": {"type": "object"}}
            ]
        });
        let out = build_native_payload(&raw).unwrap();
        let tools = out["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "shell");
    }

    #[test]
    fn build_native_payload_defaults_reasoning_for_m3() {
        let raw = json!({"model": "MiniMax-M3", "input": "hi"});
        let out = build_native_payload(&raw).unwrap();
        assert_eq!(out["reasoning"]["effort"], "medium");
    }

    #[test]
    fn build_native_payload_respects_explicit_none_reasoning() {
        let raw = json!({
            "model": "MiniMax-M3",
            "input": "hi",
            "reasoning": {"effort": "none"}
        });
        let out = build_native_payload(&raw).unwrap();
        assert_eq!(out["reasoning"]["effort"], "none");
    }

    #[test]
    fn build_native_payload_forwards_non_none_reasoning() {
        let raw = json!({
            "model": "MiniMax-M3",
            "input": "hi",
            "reasoning": {"effort": "high"}
        });
        let out = build_native_payload(&raw).unwrap();
        assert_eq!(out["reasoning"]["effort"], "high");
    }

    #[test]
    fn build_native_payload_omits_reasoning_for_m2() {
        let raw = json!({"model": "MiniMax-M2.7", "input": "hi"});
        let out = build_native_payload(&raw).unwrap();
        assert!(out.get("reasoning").is_none());
    }

    #[test]
    fn filter_tools_keeps_supported_tools_only() {
        let raw = json!([
            {"type": "function", "name": "apply_patch"},
            {"type": "function", "name": "shell"},
            {"type": "function", "function": {"name": "apply_patch"}},
            {"type": "function", "function": {"name": "read_file"}}
        ]);
        let out = filter_tools(Some(&raw)).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "shell");
        assert_eq!(arr[1]["function"]["name"], "read_file");
    }

    #[test]
    fn build_native_payload_forwards_text_field() {
        let raw = json!({
            "model": "MiniMax-M3",
            "input": "hi",
            "text": {"format": {"type": "text"}}
        });
        let out = build_native_payload(&raw).unwrap();
        assert_eq!(out["text"]["format"]["type"], "text");
    }

    #[test]
    fn build_native_payload_forwards_metadata_and_prompt_cache_key() {
        let raw = json!({
            "model": "MiniMax-M3",
            "input": "hi",
            "metadata": {"k": "v"},
            "prompt_cache_key": "abc"
        });
        let out = build_native_payload(&raw).unwrap();
        assert_eq!(out["metadata"]["k"], "v");
        assert_eq!(out["prompt_cache_key"], "abc");
    }

    #[test]
    fn build_native_payload_handles_reasoning_effort_alias() {
        let raw = json!({
            "model": "MiniMax-M3",
            "input": "hi",
            "reasoning_effort": "low"
        });
        let out = build_native_payload(&raw).unwrap();
        assert_eq!(out["reasoning"]["effort"], "low");
    }
}
