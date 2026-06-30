use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use std::time::Duration;

use super::accounts::{CopilotAccount, CopilotModelInfo};

const REQUEST_TIMEOUT_SECS: u64 = 180;
const MODEL_FALLBACKS: &[(&str, &str)] = &[
    ("gpt-5.1", "GPT-5.1"),
    ("gpt-5", "GPT-5"),
    ("gpt-4.1", "GPT-4.1"),
    ("claude-sonnet-4.5", "Claude Sonnet 4.5"),
    ("claude-opus-4.6-1m", "Claude Opus 4.6 1M"),
];

pub async fn models(State(state): State<crate::AppState>, headers: HeaderMap) -> impl IntoResponse {
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

    let account = match super::accounts::first_enabled(&state) {
        Some(account) => account,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    "No GitHub Copilot accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response()
        }
    };

    let copilot_token = match super::auth::ensure_copilot_token(&state, &account).await {
        Ok(token) => token,
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "server_error", None),
            )
                .into_response()
        }
    };

    let models = match fetch_models(&state.client, &account.account_type, &copilot_token).await {
        Ok(models) if !models.is_empty() => models,
        _ if !account.models.is_empty() => account.models.clone(),
        Err(err) => {
            let data = fallback_models(Some(err));
            let body = serde_json::to_vec(&json!({
                "object": "list",
                "data": data,
                "models": data
            }))
            .unwrap_or_default();
            return (StatusCode::OK, [("Content-Type", "application/json")], body).into_response();
        }
        _ => MODEL_FALLBACKS
            .iter()
            .map(|(id, name)| CopilotModelInfo {
                id: (*id).to_string(),
                name: Some((*name).to_string()),
                vendor: Some("copilot".to_string()),
                preview: None,
                model_picker_category: None,
                policy_state: None,
            })
            .collect(),
    };

    let data = models_to_openai_entries(&models);
    let body = serde_json::to_vec(&json!({
        "object": "list",
        "data": data,
        "models": data
    }))
    .unwrap_or_default();
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
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

    let mut raw: Value = match serde_json::from_slice(&body) {
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
                .into_response()
        }
    };
    let model = match raw.get("model").and_then(|value| value.as_str()) {
        Some(model) if !model.trim().is_empty() => model.trim().to_string(),
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
                .into_response()
        }
    };
    sanitize_responses_payload(&mut raw);
    let account = match super::accounts::pick_account(&state) {
        Some(account) => account,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    "No GitHub Copilot accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response()
        }
    };
    let context = crate::copilot_usage_context(
        &account,
        Some(model.clone()),
        "/copilot/v1/responses",
        crate::prompt_metrics_from_request_value(&raw),
    );
    crate::record_copilot_request(&state, &context);

    let wants_stream = crate::source::wants_stream(&headers, &body);
    let copilot_token = match super::auth::ensure_copilot_token(&state, &account).await {
        Ok(token) => token,
        Err(err) => {
            crate::record_copilot_error(&state, &context, &err);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "server_error", None),
            )
                .into_response();
        }
    };

    let body = match serde_json::to_vec(&raw) {
        Ok(value) => Bytes::from(value),
        Err(err) => {
            let message = format!("Copilot request serialize failed: {}", err);
            crate::record_copilot_error(&state, &context, &message);
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &message,
                    "invalid_request_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let resp = match post_copilot_responses(
        &state.client,
        &account,
        &copilot_token,
        &body,
        wants_stream,
        responses_payload_has_images(&raw),
    )
    .await
    {
        Ok(resp) => resp,
        Err(message) => {
            crate::record_copilot_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let message = format!("Copilot returned {}: {}", status, text);
        crate::record_copilot_error(&state, &context, &message);
        return (
            status,
            [("Content-Type", "application/json")],
            crate::source::v1::response::openai_error_body(
                &message,
                if status.is_client_error() {
                    "invalid_request_error"
                } else {
                    "server_error"
                },
                None,
            ),
        )
            .into_response();
    }

    if wants_stream {
        return stream_native_responses(&state, &context, resp).await;
    }

    let text = match resp.text().await {
        Ok(text) => text,
        Err(err) => {
            let message = format!("Copilot body read failed: {}", err);
            crate::record_copilot_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("invalid Copilot response: {}", err);
            crate::record_copilot_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };
    let usage = crate::usage_metrics_from_response_value(&value);
    crate::record_copilot_success(&state, &context, &usage);
    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        Bytes::from(text),
    )
        .into_response()
}

pub async fn messages(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !crate::check_api_key(&headers, &state.cfg.proxy_api_key) {
        return (
            StatusCode::UNAUTHORIZED,
            [("Content-Type", "application/json")],
            anthropic_error_body("authentication_error", "Invalid proxy API key"),
        )
            .into_response();
    }

    let raw: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                anthropic_error_body("invalid_request_error", "Invalid request body"),
            )
                .into_response()
        }
    };
    let model = match raw.get("model").and_then(|value| value.as_str()) {
        Some(model) if !model.trim().is_empty() => model.trim().to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                anthropic_error_body("invalid_request_error", "model is required"),
            )
                .into_response()
        }
    };
    let account = match super::accounts::pick_account(&state) {
        Some(account) => account,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Content-Type", "application/json")],
                anthropic_error_body("api_error", "No GitHub Copilot accounts configured"),
            )
                .into_response()
        }
    };

    let responses_payload = match anthropic_messages_to_responses(&raw, &model) {
        Ok(payload) => payload,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                anthropic_error_body("invalid_request_error", &err),
            )
                .into_response()
        }
    };
    let context = crate::copilot_usage_context(
        &account,
        Some(model.clone()),
        "/copilot/anthropic/v1/messages",
        crate::prompt_metrics_from_request_value(&raw),
    );
    crate::record_copilot_request(&state, &context);

    let copilot_token = match super::auth::ensure_copilot_token(&state, &account).await {
        Ok(token) => token,
        Err(err) => {
            crate::record_copilot_error(&state, &context, &err);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                anthropic_error_body("api_error", &err),
            )
                .into_response();
        }
    };
    let body = Bytes::from(serde_json::to_vec(&responses_payload).unwrap_or_default());
    let resp = match post_copilot_responses(
        &state.client,
        &account,
        &copilot_token,
        &body,
        false,
        responses_payload_has_images(&responses_payload),
    )
    .await
    {
        Ok(resp) => resp,
        Err(message) => {
            crate::record_copilot_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                anthropic_error_body("api_error", &message),
            )
                .into_response();
        }
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let message = format!("Copilot returned {}: {}", status, text);
        crate::record_copilot_error(&state, &context, &message);
        return (
            status,
            [("Content-Type", "application/json")],
            anthropic_error_body(
                if status.is_client_error() {
                    "invalid_request_error"
                } else {
                    "api_error"
                },
                &message,
            ),
        )
            .into_response();
    }
    let response_value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("invalid Copilot response: {}", err);
            crate::record_copilot_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                anthropic_error_body("api_error", &message),
            )
                .into_response();
        }
    };
    let usage = crate::usage_metrics_from_response_value(&response_value);
    crate::record_copilot_success(&state, &context, &usage);
    let anthropic = responses_to_anthropic_message(&response_value, &model);
    if raw
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return anthropic_stream_response(&anthropic);
    }

    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        serde_json::to_vec(&anthropic).unwrap_or_default(),
    )
        .into_response()
}

pub async fn fetch_models(
    client: &reqwest::Client,
    account_type: &str,
    copilot_token: &str,
) -> Result<Vec<CopilotModelInfo>, String> {
    let url = format!("{}/models", super::auth::copilot_base_url(account_type));
    let resp = client
        .get(url)
        .headers(super::auth::copilot_headers(copilot_token, false, "user"))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| format!("Copilot models request failed: {}", err))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|err| format!("Copilot models body read failed: {}", err))?;
    if !status.is_success() {
        return Err(format!("Copilot models returned {}: {}", status, text));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|err| format!("Copilot models JSON parse failed: {}", err))?;
    Ok(value
        .get("data")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("id").and_then(|value| value.as_str())?.trim();
            if id.is_empty() {
                return None;
            }
            Some(CopilotModelInfo {
                id: id.to_string(),
                name: item
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
                vendor: item
                    .get("vendor")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
                preview: item.get("preview").and_then(|value| value.as_bool()),
                model_picker_category: item
                    .get("model_picker_category")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
                policy_state: item
                    .get("policy")
                    .and_then(|value| value.get("state"))
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
            })
        })
        .collect())
}

fn models_to_openai_entries(models: &[CopilotModelInfo]) -> Vec<Value> {
    models
        .iter()
        .map(|model| {
            json!({
                "id": prefixed_model_id(&model.id),
                "object": "model",
                "created": 0,
                "owned_by": "copilot",
                "display_name": model.name.as_deref().unwrap_or(&model.id),
                "upstream_model": model.id,
                "vendor": model.vendor,
                "preview": model.preview,
                "model_picker_category": model.model_picker_category,
                "policy_state": model.policy_state,
                "billing_tier": super::accounts::model_billing_tier(&model.id, model.model_picker_category.as_deref()),
                "premium": super::accounts::model_is_premium(&model.id, model.model_picker_category.as_deref()),
                "utility_model": super::accounts::is_utility_model(&model.id)
            })
        })
        .collect()
}

fn fallback_models(warning: Option<String>) -> Vec<Value> {
    MODEL_FALLBACKS
        .iter()
        .map(|(id, name)| {
            let mut value = json!({
                "id": prefixed_model_id(id),
                "object": "model",
                "created": 0,
                "owned_by": "copilot",
                "display_name": name,
                "upstream_model": id
            });
            if let Some(warning) = warning.as_ref() {
                value["warning"] = Value::String(warning.clone());
            }
            value
        })
        .collect()
}

fn prefixed_model_id(model: &str) -> String {
    if model.starts_with("cop:") {
        model.to_string()
    } else {
        format!("cop:{}", model)
    }
}

async fn post_copilot_responses(
    client: &reqwest::Client,
    account: &CopilotAccount,
    copilot_token: &str,
    body: &Bytes,
    stream: bool,
    vision: bool,
) -> Result<reqwest::Response, String> {
    let url = format!(
        "{}/responses",
        super::auth::copilot_base_url(&account.account_type)
    );
    let mut request = client
        .post(url)
        .headers(super::auth::copilot_headers(copilot_token, vision, "agent"))
        .header(
            "Accept",
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .body(body.clone());

    request = request.header("Accept-Encoding", "identity");
    request
        .send()
        .await
        .map_err(|err| format!("Copilot responses request failed: {}", err))
}

async fn stream_native_responses(
    state: &crate::AppState,
    context: &crate::UsageContext,
    resp: reqwest::Response,
) -> axum::response::Response {
    let usage_state = state.clone();
    let usage_context = context.clone();
    let stream = async_stream::stream! {
        let mut upstream = resp.bytes_stream();
        let mut parser = NativeResponsesSseUsageTracker::default();
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    parser.push(&bytes);
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
                Err(err) => {
                    let message = format!("Copilot stream read failed: {}", err);
                    crate::record_copilot_error(&usage_state, &usage_context, &message);
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, "stream"));
                    return;
                }
            }
        }
        if let Some(response) = parser.finish() {
            let usage = crate::usage_metrics_from_response_value(&response);
            crate::record_copilot_success(&usage_state, &usage_context, &usage);
        } else {
            crate::record_copilot_success(&usage_state, &usage_context, &crate::UsageMetrics::default());
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

#[derive(Default)]
struct NativeResponsesSseUsageTracker {
    buffer: Vec<u8>,
    last_response: Option<Value>,
}

impl NativeResponsesSseUsageTracker {
    fn push(&mut self, bytes: &Bytes) {
        self.buffer.extend_from_slice(bytes);
        while let Some((event_end, delimiter_len)) = find_sse_boundary(&self.buffer) {
            let raw_event: Vec<u8> = self.buffer.drain(..event_end + delimiter_len).collect();
            self.absorb_event(&raw_event[..event_end]);
        }
    }

    fn finish(mut self) -> Option<Value> {
        if !self.buffer.is_empty() {
            let raw = std::mem::take(&mut self.buffer);
            self.absorb_event(&raw);
        }
        self.last_response
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
        if value.get("type").and_then(|value| value.as_str()) == Some("response.completed") {
            if let Some(response) = value.get("response").cloned() {
                self.last_response = Some(response);
            }
        }
    }
}

fn anthropic_messages_to_responses(payload: &Value, model: &str) -> Result<Value, String> {
    let messages = payload
        .get("messages")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "messages is required".to_string())?;
    let mut input = Vec::new();
    for message in messages {
        input.extend(translate_anthropic_message(message)?);
    }

    let mut out = Map::new();
    out.insert("model".to_string(), Value::String(model.to_string()));
    out.insert("input".to_string(), Value::Array(input));
    if let Some(instructions) = translate_anthropic_system(payload.get("system")) {
        out.insert("instructions".to_string(), Value::String(instructions));
    }
    if let Some(max_tokens) = payload.get("max_tokens").and_then(|value| value.as_u64()) {
        out.insert("max_output_tokens".to_string(), json!(max_tokens));
    }
    copy_if_present(payload, &mut out, "temperature");
    copy_if_present(payload, &mut out, "top_p");
    if let Some(tools) = translate_anthropic_tools(payload.get("tools")) {
        out.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(tool_choice) = translate_anthropic_tool_choice(payload.get("tool_choice")) {
        out.insert("tool_choice".to_string(), tool_choice);
    }
    out.insert("stream".to_string(), Value::Bool(false));
    Ok(Value::Object(out))
}

fn translate_anthropic_message(message: &Value) -> Result<Vec<Value>, String> {
    let role = message
        .get("role")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "message role is required".to_string())?;
    let content = message
        .get("content")
        .ok_or_else(|| "message content is required".to_string())?;
    if role == "user" {
        translate_anthropic_user_content(content)
    } else if role == "assistant" {
        translate_anthropic_assistant_content(content)
    } else {
        Ok(Vec::new())
    }
}

fn translate_anthropic_user_content(content: &Value) -> Result<Vec<Value>, String> {
    if let Some(text) = content.as_str() {
        return Ok(vec![json!({"role": "user", "content": text})]);
    }
    let Some(parts) = content.as_array() else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    let mut message_parts = Vec::new();
    for part in parts {
        match part.get("type").and_then(|value| value.as_str()) {
            Some("tool_result") => {
                let call_id = part
                    .get("tool_use_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": stringify_tool_result(part.get("content"))
                }));
            }
            Some("text") => {
                if let Some(text) = part.get("text").and_then(|value| value.as_str()) {
                    message_parts.push(json!({"type": "input_text", "text": text}));
                }
            }
            Some("image") => {
                if let Some(image) = translate_anthropic_image(part) {
                    message_parts.push(image);
                }
            }
            _ => {}
        }
    }
    if !message_parts.is_empty() {
        items.push(json!({"role": "user", "content": message_parts}));
    }
    Ok(items)
}

fn translate_anthropic_assistant_content(content: &Value) -> Result<Vec<Value>, String> {
    if let Some(text) = content.as_str() {
        return Ok(vec![json!({"role": "assistant", "content": text})]);
    }
    let Some(parts) = content.as_array() else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    let text = parts
        .iter()
        .filter(|part| part.get("type").and_then(|value| value.as_str()) == Some("text"))
        .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n");
    if !text.is_empty() {
        items.push(json!({"role": "assistant", "content": text}));
    }
    for part in parts {
        if part.get("type").and_then(|value| value.as_str()) != Some("tool_use") {
            continue;
        }
        let id = part
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let name = part
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let arguments = part
            .get("input")
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        items.push(json!({
            "type": "function_call",
            "call_id": id,
            "name": name,
            "arguments": arguments.to_string()
        }));
    }
    Ok(items)
}

fn translate_anthropic_system(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let joined = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn translate_anthropic_image(part: &Value) -> Option<Value> {
    let source = part.get("source")?;
    let source_type = source.get("type").and_then(|value| value.as_str())?;
    if source_type == "base64" {
        let media_type = source.get("media_type").and_then(|value| value.as_str())?;
        let data = source.get("data").and_then(|value| value.as_str())?;
        return Some(json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", media_type, data)
        }));
    }
    None
}

fn stringify_tool_result(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(|value| value.as_str()) == Some("text") {
                    part.get("text")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string())
                } else {
                    Some(part.to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn translate_anthropic_tools(value: Option<&Value>) -> Option<Vec<Value>> {
    let tools = value?.as_array()?;
    let out = tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(|value| value.as_str())?;
            Some(json!({
                "type": "function",
                "name": name,
                "description": tool.get("description").and_then(|value| value.as_str()).unwrap_or(""),
                "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type":"object"}))
            }))
        })
        .collect::<Vec<_>>();
    (!out.is_empty()).then_some(out)
}

fn translate_anthropic_tool_choice(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    match value.get("type").and_then(|value| value.as_str()) {
        Some("auto") => Some(Value::String("auto".to_string())),
        Some("none") => Some(Value::String("none".to_string())),
        Some("any") => Some(Value::String("required".to_string())),
        Some("tool") => value
            .get("name")
            .and_then(|name| name.as_str())
            .map(|name| json!({"type": "function", "name": name})),
        _ => None,
    }
}

fn responses_to_anthropic_message(response: &Value, client_model: &str) -> Value {
    let content = response_to_anthropic_content(response);
    let has_tool = content
        .iter()
        .any(|block| block.get("type").and_then(|value| value.as_str()) == Some("tool_use"));
    let usage = response.get("usage").cloned().unwrap_or_else(|| json!({}));
    json!({
        "id": response.get("id").and_then(|value| value.as_str()).unwrap_or("msg_copilot"),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": client_model,
        "stop_reason": if has_tool { "tool_use" } else { "end_turn" },
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": usage.get("input_tokens").and_then(|value| value.as_u64()).unwrap_or(0),
            "output_tokens": usage.get("output_tokens").and_then(|value| value.as_u64()).unwrap_or(0)
        }
    })
}

fn response_to_anthropic_content(response: &Value) -> Vec<Value> {
    let mut content = Vec::new();
    if let Some(output) = response.get("output").and_then(|value| value.as_array()) {
        for item in output {
            match item.get("type").and_then(|value| value.as_str()) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(|value| value.as_array()) {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|value| value.as_str()) {
                                if !text.is_empty() {
                                    content.push(json!({"type": "text", "text": text}));
                                }
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let arguments = item
                        .get("arguments")
                        .and_then(|value| value.as_str())
                        .and_then(|value| serde_json::from_str::<Value>(value).ok())
                        .unwrap_or_else(|| json!({}));
                    content.push(json!({
                        "type": "tool_use",
                        "id": item.get("call_id").or_else(|| item.get("id")).and_then(|value| value.as_str()).unwrap_or("call_copilot"),
                        "name": item.get("name").and_then(|value| value.as_str()).unwrap_or("tool"),
                        "input": arguments
                    }));
                }
                _ => {}
            }
        }
    }
    if content.is_empty() {
        if let Some(text) = response.get("output_text").and_then(|value| value.as_str()) {
            if !text.is_empty() {
                content.push(json!({"type": "text", "text": text}));
            }
        }
    }
    content
}

fn anthropic_stream_response(message: &Value) -> axum::response::Response {
    let events = anthropic_sse_events(message);
    let stream = async_stream::stream! {
        for event in events {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(event));
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

fn anthropic_sse_events(message: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = message.clone();
    if let Some(object) = start.as_object_mut() {
        object.insert("content".to_string(), Value::Array(Vec::new()));
        object.insert("stop_reason".to_string(), Value::Null);
        object.insert("stop_sequence".to_string(), Value::Null);
    }
    out.push(sse_event(
        "message_start",
        &json!({
            "type": "message_start",
            "message": start
        }),
    ));

    let mut index = 0usize;
    if let Some(content) = message.get("content").and_then(|value| value.as_array()) {
        for block in content {
            out.push(sse_event("content_block_start", &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": if block.get("type").and_then(|value| value.as_str()) == Some("text") {
                    json!({"type":"text","text":""})
                } else {
                    block.clone()
                }
            })));
            if block.get("type").and_then(|value| value.as_str()) == Some("text") {
                out.push(sse_event("content_block_delta", &json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "text_delta", "text": block.get("text").and_then(|value| value.as_str()).unwrap_or("")}
                })));
            }
            out.push(sse_event(
                "content_block_stop",
                &json!({
                    "type": "content_block_stop",
                    "index": index
                }),
            ));
            index += 1;
        }
    }
    out.push(sse_event("message_delta", &json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": message.get("stop_reason").and_then(|value| value.as_str()).unwrap_or("end_turn"),
            "stop_sequence": Value::Null
        },
        "usage": {
            "output_tokens": message.get("usage").and_then(|usage| usage.get("output_tokens")).and_then(|value| value.as_u64()).unwrap_or(0)
        }
    })));
    out.push(sse_event("message_stop", &json!({"type": "message_stop"})));
    out
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {}\ndata: {}\n\n", event, data)
}

fn anthropic_error_body(kind: &str, message: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "type": "error",
            "error": {
                "type": kind,
                "message": message
            }
        }))
        .unwrap_or_default(),
    )
}

fn responses_payload_has_images(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            key == "image_url"
                || key == "input_image"
                || key == "image"
                || responses_payload_has_images(value)
        }),
        Value::Array(items) => items.iter().any(responses_payload_has_images),
        Value::String(text) => text.starts_with("data:image/"),
        _ => false,
    }
}

fn sanitize_responses_payload(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("service_tier");

    if let Some(tools) = object
        .get_mut("tools")
        .and_then(|value| value.as_array_mut())
    {
        tools.retain(|tool| {
            tool.get("type").and_then(|value| value.as_str()) != Some("image_generation")
        });
    }
}

fn copy_if_present(input: &Value, output: &mut Map<String, Value>, key: &str) {
    if let Some(value) = input.get(key) {
        output.insert(key.to_string(), value.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_exposed_with_copilot_prefix() {
        let models = vec![CopilotModelInfo {
            id: "gpt-5.1".to_string(),
            name: Some("GPT-5.1".to_string()),
            vendor: Some("openai".to_string()),
            preview: Some(false),
            model_picker_category: Some("powerful".to_string()),
            policy_state: Some("enabled".to_string()),
        }];

        let entries = models_to_openai_entries(&models);
        assert_eq!(entries[0]["id"], "cop:gpt-5.1");
        assert_eq!(entries[0]["upstream_model"], "gpt-5.1");
        assert_eq!(entries[0]["billing_tier"], "premium");
        assert_eq!(entries[0]["premium"], true);
        assert_eq!(entries[0]["utility_model"], false);
    }

    #[test]
    fn translates_anthropic_messages_to_responses_payload() {
        let payload = json!({
            "model": "claude-sonnet-4.5",
            "system": "You are concise.",
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "Say hi"}
            ],
            "tools": [
                {"name": "lookup", "description": "Lookup", "input_schema": {"type": "object"}}
            ]
        });

        let out = anthropic_messages_to_responses(&payload, "claude-sonnet-4.5").unwrap();
        assert_eq!(out["model"], "claude-sonnet-4.5");
        assert_eq!(out["instructions"], "You are concise.");
        assert_eq!(out["max_output_tokens"], 64);
        assert_eq!(out["input"][0]["role"], "user");
        assert_eq!(out["tools"][0]["type"], "function");
    }

    #[test]
    fn converts_responses_output_to_anthropic_message() {
        let response = json!({
            "id": "resp_1",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "hello"}]
            }],
            "usage": {"input_tokens": 2, "output_tokens": 1}
        });

        let msg = responses_to_anthropic_message(&response, "claude-sonnet-4.5");
        assert_eq!(msg["type"], "message");
        assert_eq!(msg["content"][0]["type"], "text");
        assert_eq!(msg["content"][0]["text"], "hello");
        assert_eq!(msg["usage"]["input_tokens"], 2);
    }

    #[test]
    fn sanitizes_codex_only_responses_fields() {
        let mut payload = json!({
            "model": "gpt-5.1",
            "input": "hi",
            "service_tier": "fast",
            "tools": [
                {"type": "function", "name": "lookup"},
                {"type": "image_generation"}
            ]
        });

        sanitize_responses_payload(&mut payload);
        assert!(payload.get("service_tier").is_none());
        assert_eq!(payload["tools"].as_array().unwrap().len(), 1);
        assert_eq!(payload["tools"][0]["type"], "function");
    }
}
