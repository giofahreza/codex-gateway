use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;

use super::DEFAULT_BASE_URL;

const MODEL_FALLBACKS: &[&str] = &[
    "MiniMax-Text-01",
    "abab6.5s-chat",
    "abab6.5-chat",
    "abab5.5-chat",
];

pub fn normalize_base_url(base_url: Option<&str>) -> String {
    base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/')
        .to_string()
}

fn chat_completions_url(base_url: &str) -> String {
    let base = normalize_base_url(Some(base_url));
    if base.ends_with("/v1/chat/completions") {
        return base;
    }
    if base.ends_with("/chat/completions") {
        return base;
    }
    if base.ends_with("/v1") {
        return format!("{}/chat/completions", base);
    }
    format!("{}/v1/chat/completions", base)
}

fn models_url(base_url: &str) -> String {
    let base = normalize_base_url(Some(base_url));
    if base.ends_with("/models") {
        return base;
    }
    if base.ends_with("/v1") {
        return format!("{}/models", base);
    }
    format!("{}/v1/models", base)
}

pub async fn validate_api_key(
    client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
) -> Result<(), String> {
    let resp = client
        .get(models_url(base_url))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("MiniMax models request failed: {}", e))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("MiniMax models body read failed: {}", e))?;
    if !status.is_success() {
        return Err(format!("MiniMax models returned {}: {}", status, text));
    }
    Ok(())
}

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
                    "No MiniMax accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let api_key = account.api_key.clone();
    let base_url = normalize_base_url(account.base_url.as_deref());
    match fetch_models_json(&state.client, &api_key, &base_url).await {
        Ok(value) => match models_to_openai_json(&value) {
            Ok(body) => {
                (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
            }
            Err(err) => (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "server_error", None),
            )
                .into_response(),
        },
        Err(err) => {
            let data = MODEL_FALLBACKS
                .iter()
                .map(|id| {
                    json!({
                        "id": id,
                        "object": "model",
                        "created": 0,
                        "owned_by": "minimax"
                    })
                })
                .collect::<Vec<_>>();
            let body = serde_json::to_vec(&json!({
                "object": "list",
                "data": data,
                "warning": err
            }))
            .unwrap_or_default();
            (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
        }
    }
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
    let context = crate::minimax_usage_context(
        &account,
        Some(model.clone()),
        "/minimax/v1/chat/completions",
        crate::prompt_metrics_from_request_value(&raw),
    );
    crate::record_minimax_request(&state, &context);

    let base_url = normalize_base_url(account.base_url.as_deref());
    let wants_stream = crate::source::wants_stream(&headers, &body);

    let chat_payload = match build_chat_completions_payload(&raw, &model) {
        Ok(payload) => payload,
        Err(err) => {
            crate::record_minimax_error(&state, &context, &err);
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "invalid_request_error", None),
            )
                .into_response();
        }
    };

    if wants_stream {
        return stream_chat_completions(
            &state,
            &account.api_key,
            &base_url,
            &chat_payload,
            &context,
            &model,
            &headers,
        )
        .await;
    }

    let resp = match state
        .client
        .post(chat_completions_url(&base_url))
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
            crate::record_minimax_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };

    let status = resp.status();
    let text = match resp.text().await {
        Ok(text) => text,
        Err(err) => {
            let message = format!("MiniMax body read failed: {}", err);
            crate::record_minimax_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };

    if !status.is_success() {
        crate::record_minimax_error(
            &state,
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
            crate::record_minimax_error(&state, &context, &message);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };

    let response = chat_completion_to_responses(&chat_response, &model);
    let usage = crate::usage_metrics_from_response_value(&response);
    crate::record_minimax_success(&state, &context, &usage);

    let body = serde_json::to_vec(&response).unwrap_or_default();
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}

async fn stream_chat_completions(
    state: &crate::AppState,
    api_key: &str,
    base_url: &str,
    payload: &Value,
    context: &crate::UsageContext,
    model: &str,
    _headers: &HeaderMap,
) -> axum::response::Response {
    let resp = match state
        .client
        .post(chat_completions_url(base_url))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .body(payload.to_string())
        .timeout(Duration::from_secs(180))
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            let message = format!("MiniMax stream request failed: {}", err);
            crate::record_minimax_error(state, context, &message);
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
        crate::record_minimax_error(
            state,
            context,
            format!("MiniMax stream returned {}: {}", status, text),
        );
        return (
            StatusCode::BAD_GATEWAY,
            [("Content-Type", "application/json")],
            crate::source::v1::response::openai_error_body(
                &format!("MiniMax stream returned {}: {}", status, text),
                "server_error",
                None,
            ),
        )
            .into_response();
    }

    let usage_state = state.clone();
    let usage_context = context.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let mut recorded = false;
    let stream = resp.bytes_stream().map(move |chunk| {
        if let Ok(ref bytes) = chunk {
            if !recorded {
                buffer.extend_from_slice(bytes);
                if let Some(usage) = scan_sse_for_usage(&buffer) {
                    let metrics = chat_usage_to_metrics(&usage);
                    crate::record_minimax_success(&usage_state, &usage_context, &metrics);
                    recorded = true;
                }
            }
        }
        chunk.map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "stream"))
    });
    let converted_stream = stream.map_ok(move |chunk| chunk);

    (
        StatusCode::OK,
        [
            ("Content-Type", "text/event-stream"),
            ("Cache-Control", "no-store"),
        ],
        Body::from_stream(converted_stream),
    )
        .into_response()
}

async fn fetch_models_json(
    client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
) -> Result<Value, String> {
    let resp = client
        .get(models_url(base_url))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("MiniMax models request failed: {}", e))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("MiniMax models body read failed: {}", e))?;
    if !status.is_success() {
        return Err(format!("MiniMax models returned {}: {}", status, text));
    }
    serde_json::from_str(&text).map_err(|e| format!("invalid MiniMax models JSON: {}", e))
}

fn models_to_openai_json(value: &Value) -> Result<Vec<u8>, String> {
    let models = value
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())
        .ok_or_else(|| "MiniMax models response was missing data".to_string())?;

    let data = models
        .iter()
        .filter_map(|model| {
            let id = model
                .get("id")
                .and_then(|v| v.as_str())
                .or_else(|| model.get("model").and_then(|v| v.as_str()))?;
            Some(json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": model.get("owned_by").and_then(|v| v.as_str()).unwrap_or("minimax")
            }))
        })
        .collect::<Vec<_>>();

    serde_json::to_vec(&json!({ "object": "list", "data": data })).map_err(|e| e.to_string())
}

fn build_chat_completions_payload(raw: &Value, model: &str) -> Result<Value, String> {
    let mut out = json!({
        "model": model,
        "stream": false,
    });

    if let Some(messages) = build_chat_messages(raw)? {
        out["messages"] = messages;
    } else {
        return Err("request did not contain any messages or input".to_string());
    }

    if let Some(max_tokens) = raw
        .get("max_output_tokens")
        .or_else(|| raw.get("max_tokens"))
        .and_then(|v| v.as_u64())
    {
        out["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = raw.get("temperature").and_then(|v| v.as_f64()) {
        out["temperature"] = json!(temperature);
    }
    if let Some(top_p) = raw.get("top_p").and_then(|v| v.as_f64()) {
        out["top_p"] = json!(top_p);
    }
    if let Some(tools) = build_chat_tools(raw) {
        out["tools"] = tools;
    }
    if let Some(tool_choice) = build_chat_tool_choice(raw) {
        out["tool_choice"] = tool_choice;
    }
    if let Some(stop) = raw.get("stop").cloned() {
        out["stop"] = stop;
    }
    if let Some(stream) = raw.get("stream").cloned() {
        out["stream"] = stream;
    }
    Ok(out)
}

fn build_chat_messages(raw: &Value) -> Result<Option<Value>, String> {
    if let Some(messages) = raw.get("messages").and_then(|v| v.as_array()) {
        let mut out = Vec::new();
        for message in messages {
            let role = message
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("user")
                .trim()
                .to_ascii_lowercase();
            let role = match role.as_str() {
                "system" | "developer" => "system",
                "assistant" => "assistant",
                "user" => "user",
                "tool" => "tool",
                other => other,
            };
            let content = flatten_message_content(message.get("content"));
            let mut entry = json!({
                "role": role,
                "content": content
            });
            if let Some(name) = message.get("name").and_then(|v| v.as_str()) {
                entry["name"] = json!(name);
            }
            if let Some(tool_call_id) = message.get("tool_call_id").and_then(|v| v.as_str()) {
                entry["tool_call_id"] = json!(tool_call_id);
            }
            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                entry["tool_calls"] = Value::Array(tool_calls.clone());
            }
            out.push(entry);
        }
        return Ok(Some(Value::Array(out)));
    }

    if let Some(prompt) = raw.get("prompt") {
        if let Some(text) = prompt.as_str() {
            return Ok(Some(json!([
                { "role": "user", "content": text }
            ])));
        }
    }

    if let Some(input) = raw.get("input") {
        if let Some(text) = input.as_str() {
            let mut out = Vec::new();
            if let Some(instructions) = raw
                .get("instructions")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                out.push(json!({ "role": "system", "content": instructions }));
            }
            out.push(json!({ "role": "user", "content": text }));
            return Ok(Some(Value::Array(out)));
        }

        if let Some(items) = input.as_array() {
            let mut out = Vec::new();
            if let Some(instructions) = raw
                .get("instructions")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                out.push(json!({ "role": "system", "content": instructions }));
            }
            for item in items {
                if let Some(entry) = input_item_to_chat_message(item)? {
                    out.push(entry);
                }
            }
            if out.is_empty() {
                return Ok(None);
            }
            return Ok(Some(Value::Array(out)));
        }
    }

    Ok(None)
}

fn input_item_to_chat_message(item: &Value) -> Result<Option<Value>, String> {
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let role = item
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match item_type {
        "message" | "" => {
            let role = match role.as_str() {
                "system" | "developer" => "system",
                "assistant" => "assistant",
                "user" | "" => "user",
                "tool" => "tool",
                other => other,
            };
            let content = flatten_message_content(item.get("content"));
            let mut entry = json!({ "role": role, "content": content });
            if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                entry["name"] = json!(name);
            }
            if let Some(tool_call_id) = item.get("tool_call_id").and_then(|v| v.as_str()) {
                entry["tool_call_id"] = json!(tool_call_id);
            }
            if let Some(tool_calls) = item.get("tool_calls").and_then(|v| v.as_array()) {
                entry["tool_calls"] = Value::Array(tool_calls.clone());
            }
            Ok(Some(entry))
        }
        "function_call" => {
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = item
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            if name.is_empty() {
                return Ok(None);
            }
            let tool_call = json!({
                "id": call_id,
                "type": "function",
                "function": { "name": name, "arguments": arguments }
            });
            let assistant_text = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
            let content = if assistant_text.is_empty() {
                Value::String(String::new())
            } else {
                Value::String(assistant_text.to_string())
            };
            Ok(Some(json!({
                "role": "assistant",
                "content": content,
                "tool_calls": [tool_call]
            })))
        }
        "function_call_output" => {
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let output = item
                .get("output")
                .cloned()
                .unwrap_or(Value::String(String::new()));
            let output_text = match output {
                Value::String(s) => s,
                other => serde_json::to_string(&other).unwrap_or_default(),
            };
            Ok(Some(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output_text
            })))
        }
        _ => Ok(None),
    }
}

fn flatten_message_content(value: Option<&Value>) -> Value {
    let Some(value) = value else {
        return Value::String(String::new());
    };
    if let Some(text) = value.as_str() {
        return Value::String(text.to_string());
    }
    if let Some(arr) = value.as_array() {
        let mut parts = Vec::new();
        for part in arr {
            if let Some(text) = part
                .get("text")
                .and_then(|v| v.as_str())
                .or_else(|| part.get("input_text").and_then(|v| v.as_str()))
                .or_else(|| part.get("output_text").and_then(|v| v.as_str()))
            {
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            } else if let Some(text) = part.as_str() {
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
        }
        if parts.is_empty() {
            return Value::String(String::new());
        }
        return Value::String(parts.join("\n"));
    }
    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        return Value::String(text.to_string());
    }
    Value::String(String::new())
}

fn build_chat_tools(raw: &Value) -> Option<Value> {
    let tools = raw.get("tools")?.as_array()?;
    let mut out = Vec::new();
    for tool in tools {
        if let Some(function) = tool.get("function") {
            let mut mapped = json!({
                "type": "function",
                "function": {
                    "name": function.get("name").cloned().unwrap_or(Value::String(String::new())),
                    "description": function.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "parameters": function.get("parameters").cloned().unwrap_or(json!({ "type": "object" }))
                }
            });
            if let Some(strict) = function.get("strict").and_then(|v| v.as_bool()) {
                mapped["function"]["strict"] = json!(strict);
            }
            out.push(mapped);
        } else if let Some(name) = tool.get("name").and_then(|v| v.as_str()) {
            out.push(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": tool.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "parameters": tool.get("parameters").cloned().unwrap_or(json!({ "type": "object" }))
                }
            }));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

fn build_chat_tool_choice(raw: &Value) -> Option<Value> {
    let choice = raw.get("tool_choice")?;
    if let Some(value) = choice.as_str() {
        return match value {
            "auto" | "none" | "required" => Some(json!(value)),
            other => Some(json!(other)),
        };
    }
    Some(choice.clone())
}

fn chat_completion_to_responses(chat: &Value, model: &str) -> Value {
    let id = chat
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("chatcmpl-{}", Uuid::new_v4().simple()));
    let created = chat
        .get("created")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| chrono::Utc::now().timestamp() as u64);

    let mut output_text = String::new();
    let mut output: Vec<Value> = Vec::new();
    if let Some(choices) = chat.get("choices").and_then(|v| v.as_array()) {
        for choice in choices {
            if let Some(message) = choice.get("message") {
                if let Some(reasoning) = message
                    .get("reasoning_content")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    output.push(json!({
                        "id": format!("rs_{}", Uuid::new_v4().simple()),
                        "type": "reasoning",
                        "summary": [{
                            "type": "summary_text",
                            "text": reasoning
                        }],
                        "content": reasoning
                    }));
                }
                let mut had_content = false;
                if let Some(content) = message.get("content") {
                    if let Some(text) = content.as_str() {
                        if !text.is_empty() {
                            output_text.push_str(text);
                            had_content = true;
                        }
                    } else if let Some(parts) = content.as_array() {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    output_text.push_str(text);
                                    had_content = true;
                                }
                            }
                        }
                    }
                }
                if had_content {
                    output.push(json!({
                        "type": "message",
                        "id": format!("msg_{}", Uuid::new_v4().simple()),
                        "status": "completed",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": output_text.clone(),
                            "annotations": []
                        }]
                    }));
                }
                if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                    for call in tool_calls {
                        let call_id = call
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
                        let name = call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let arguments = call
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        output.push(json!({
                            "id": call_id,
                            "type": "function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": arguments
                        }));
                    }
                }
            }
        }
    }

    let usage = chat.get("usage").cloned().unwrap_or(json!({}));
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(input_tokens + output_tokens);

    json!({
        "id": id,
        "object": "response",
        "created": created,
        "model": model,
        "status": "completed",
        "output": output,
        "output_text": output_text,
        "usage": {
            "input_tokens": input_tokens,
            "input_tokens_details": { "cached_tokens": 0 },
            "output_tokens": output_tokens,
            "output_tokens_details": { "reasoning_tokens": 0 },
            "total_tokens": total_tokens,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0
        }
    })
}

fn chat_usage_to_metrics(usage: &Value) -> crate::UsageMetrics {
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(input_tokens + output_tokens);
    let cache_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    crate::UsageMetrics {
        input_tokens,
        output_tokens,
        total_tokens,
        cache_tokens,
        reasoning_tokens: 0,
        raw_usage: Some(usage.clone()),
    }
}

fn scan_sse_for_usage(buffer: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(buffer);
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let data = match line.strip_prefix("data:") {
            Some(data) => data.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            return None;
        }
        let value: Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("object").and_then(|v| v.as_str()) == Some("chat.completion") {
            return value.get("usage").cloned();
        }
        if let Some(choice) = value
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
        {
            if let Some(usage) = choice.get("usage") {
                return Some(usage.clone());
            }
        }
        if let Some(usage) = value.get("usage") {
            return Some(usage.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_url_defaults_to_official() {
        assert_eq!(normalize_base_url(None), DEFAULT_BASE_URL);
        assert_eq!(
            normalize_base_url(Some("https://example.com/")),
            "https://example.com"
        );
    }

    #[test]
    fn chat_completions_url_handles_known_shapes() {
        assert_eq!(
            chat_completions_url("https://api.minimaxi.chat"),
            "https://api.minimaxi.chat/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.minimaxi.chat/v1"),
            "https://api.minimaxi.chat/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://example.com/v1/chat/completions"),
            "https://example.com/v1/chat/completions"
        );
    }

    #[test]
    fn build_chat_completions_payload_maps_messages_and_tools() {
        let raw = json!({
            "model": "MiniMax-Text-01",
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ],
            "max_output_tokens": 64,
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "echo",
                        "parameters": { "type": "object" }
                    }
                }
            ]
        });
        let payload = build_chat_completions_payload(&raw, "MiniMax-Text-01").unwrap();
        assert_eq!(payload["model"], "MiniMax-Text-01");
        assert_eq!(payload["max_tokens"], 64);
        assert_eq!(payload["messages"][0]["role"], "system");
        assert_eq!(payload["messages"][1]["content"], "hi");
        assert_eq!(payload["tools"][0]["function"]["name"], "echo");
    }

    #[test]
    fn build_chat_completions_payload_maps_responses_input() {
        let raw = json!({
            "model": "MiniMax-Text-01",
            "instructions": "be brief",
            "input": [
                { "type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}] },
                { "type": "function_call", "call_id": "call_1", "name": "echo", "arguments": "{\"x\":1}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "ok" }
            ]
        });
        let payload = build_chat_completions_payload(&raw, "MiniMax-Text-01").unwrap();
        assert_eq!(payload["messages"][0]["role"], "system");
        assert_eq!(payload["messages"][1]["role"], "user");
        assert_eq!(payload["messages"][2]["role"], "assistant");
        assert_eq!(payload["messages"][2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(payload["messages"][3]["role"], "tool");
        assert_eq!(payload["messages"][3]["tool_call_id"], "call_1");
        assert_eq!(payload["messages"][3]["content"], "ok");
    }

    #[test]
    fn chat_completion_to_responses_maps_text_thinking_and_tool_use() {
        let chat = json!({
            "id": "chatcmpl-1",
            "created": 1700000000,
            "model": "MiniMax-Text-01",
            "choices": [{
                "message": {
                    "reasoning_content": "think",
                    "content": "hello",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": { "name": "echo", "arguments": "{}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7 }
        });
        let response = chat_completion_to_responses(&chat, "MiniMax-Text-01");
        let output = response["output"].as_array().unwrap();
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["type"], "message");
        assert_eq!(output[2]["type"], "function_call");
        assert_eq!(response["usage"]["input_tokens"], 3);
        assert_eq!(response["usage"]["output_tokens"], 4);
        assert_eq!(response["output_text"], "hello");
    }
}
