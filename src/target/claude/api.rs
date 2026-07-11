use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use std::time::Duration;

const REQUEST_TIMEOUT_SECS: u64 = 180;
const DEFAULT_MAX_TOKENS: u64 = 4096;

pub async fn models(State(state): State<crate::AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !crate::check_api_key(&state, &headers) {
        return openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            "Invalid proxy API key",
        );
    }

    let account = match super::accounts::first_enabled(&state) {
        Some(account) => account,
        None => {
            return (
                StatusCode::OK,
                [("Content-Type", "application/json")],
                serde_json::to_vec(&json!({
                    "object": "list",
                    "data": fallback_models()
                }))
                .unwrap_or_default(),
            )
                .into_response();
        }
    };
    let access_token = match super::auth::ensure_access_token(&state, &account).await {
        Ok(token) => token,
        Err(err) => {
            return (
                StatusCode::OK,
                [("Content-Type", "application/json")],
                serde_json::to_vec(&json!({
                    "object": "list",
                    "warning": err,
                    "data": models_to_openai_entries(&account.models)
                }))
                .unwrap_or_default(),
            )
                .into_response();
        }
    };
    let base_url = super::auth::api_base_url(account.api_base_url.as_deref());
    let models = super::auth::fetch_models(&state.client, &access_token, &base_url)
        .await
        .unwrap_or_else(|_| {
            if account.models.is_empty() {
                fallback_model_infos()
            } else {
                account.models.clone()
            }
        });
    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        serde_json::to_vec(&json!({
            "object": "list",
            "data": models_to_openai_entries(&models)
        }))
        .unwrap_or_default(),
    )
        .into_response()
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
    let raw = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(_) => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Invalid request body",
            );
        }
    };
    let model = match raw.get("model").and_then(|value| value.as_str()) {
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
            "No Claude accounts configured",
        );
    }
    let wants_stream = crate::source::wants_stream(&headers, &body);
    let prompt_metrics = crate::prompt_metrics_from_request_value(&raw);
    let mut last_error: Option<(StatusCode, String)> = None;

    for (attempt_idx, account) in accounts.iter().enumerate() {
        let context = crate::claude_usage_context(
            account,
            Some(model.clone()),
            "/claude/v1/messages",
            prompt_metrics.clone(),
        );
        crate::record_claude_request(&state, &context);
        let access_token = match super::auth::ensure_access_token(&state, account).await {
            Ok(token) => token,
            Err(err) => {
                crate::record_claude_error(&state, &context, &err);
                last_error = Some((StatusCode::UNAUTHORIZED, err));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };

        let resp = match post_anthropic_messages(
            &state.client,
            account,
            &access_token,
            &headers,
            body.clone(),
            wants_stream,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                crate::record_claude_error(&state, &context, &err);
                last_error = Some((StatusCode::BAD_GATEWAY, err));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };

        let status = resp.status();
        super::quota::observe_response_headers(&state, account, resp.headers());
        let out_headers = response_headers(
            resp.headers(),
            if wants_stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        );
        if wants_stream && status.is_success() {
            return stream_anthropic_passthrough(state, context, resp, out_headers).await;
        }
        let bytes = match resp.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                let message = format!("Claude body read failed: {}", err);
                crate::record_claude_error(&state, &context, &message);
                last_error = Some((StatusCode::BAD_GATEWAY, message));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };
        if !status.is_success() {
            let message = format!(
                "Claude returned {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            );
            crate::record_claude_error(&state, &context, &message);
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
        crate::record_claude_success(&state, &context, &usage);
        return (status, out_headers, bytes).into_response();
    }

    let (status, message) = last_error.unwrap_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "All Claude accounts failed".to_string(),
        )
    });
    anthropic_error(
        status,
        "api_error",
        &format!("All Claude accounts failed; last error: {}", message),
    )
}

pub async fn responses(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !crate::check_api_key(&state, &headers) {
        return openai_error(
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            "Invalid proxy API key",
        );
    }
    let raw = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(_) => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Invalid request body",
            );
        }
    };
    let model = match raw.get("model").and_then(|value| value.as_str()) {
        Some(model) if !model.trim().is_empty() => model.trim().to_string(),
        _ => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "model is required",
            );
        }
    };
    let wants_stream = crate::source::wants_stream(&headers, &body);
    let anthropic_payload = match responses_to_anthropic_messages(&raw, &model, false) {
        Ok(payload) => payload,
        Err(err) => {
            return openai_error(StatusCode::BAD_REQUEST, "invalid_request_error", &err);
        }
    };
    let anthropic_body = Bytes::from(serde_json::to_vec(&anthropic_payload).unwrap_or_default());
    let accounts = super::accounts::candidate_accounts(&state);
    if accounts.is_empty() {
        return openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "No Claude accounts configured",
        );
    }
    let prompt_metrics = crate::prompt_metrics_from_request_value(&raw);
    let mut last_error: Option<(StatusCode, String)> = None;

    for (attempt_idx, account) in accounts.iter().enumerate() {
        let context = crate::claude_usage_context(
            account,
            Some(model.clone()),
            "/claude/v1/responses",
            prompt_metrics.clone(),
        );
        crate::record_claude_request(&state, &context);
        let access_token = match super::auth::ensure_access_token(&state, account).await {
            Ok(token) => token,
            Err(err) => {
                crate::record_claude_error(&state, &context, &err);
                last_error = Some((StatusCode::UNAUTHORIZED, err));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };
        let resp = match post_anthropic_messages(
            &state.client,
            account,
            &access_token,
            &headers,
            anthropic_body.clone(),
            false,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                crate::record_claude_error(&state, &context, &err);
                last_error = Some((StatusCode::BAD_GATEWAY, err));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };
        let status = resp.status();
        super::quota::observe_response_headers(&state, account, resp.headers());
        let text = match resp.text().await {
            Ok(text) => text,
            Err(err) => {
                let message = format!("Claude body read failed: {}", err);
                crate::record_claude_error(&state, &context, &message);
                last_error = Some((StatusCode::BAD_GATEWAY, message));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        };
        if !status.is_success() {
            let message = format!("Claude returned {}: {}", status, text);
            crate::record_claude_error(&state, &context, &message);
            if attempt_idx + 1 < accounts.len()
                && crate::should_retry_account_error(status, &message)
            {
                last_error = Some((status, message));
                continue;
            }
            return (
                status,
                [("Content-Type", "application/json")],
                openai_error_body_from_anthropic(&text),
            )
                .into_response();
        }
        let anthropic: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                let message = format!("Claude response JSON parse failed: {}", err);
                crate::record_claude_error(&state, &context, &message);
                return openai_error(StatusCode::BAD_GATEWAY, "server_error", &message);
            }
        };
        let response = anthropic_message_to_response(&anthropic, &model);
        let mut usage = crate::usage_metrics_from_response_value(&response);
        if usage.total_tokens == 0 {
            usage = crate::usage_metrics_from_response_value(&anthropic);
        }
        crate::record_claude_success(&state, &context, &usage);
        if wants_stream {
            return responses_sse(response).into_response();
        }
        return (
            StatusCode::OK,
            [("Content-Type", "application/json")],
            serde_json::to_vec(&response).unwrap_or_default(),
        )
            .into_response();
    }

    let (status, message) = last_error.unwrap_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "All Claude accounts failed".to_string(),
        )
    });
    openai_error(
        status,
        "server_error",
        &format!("All Claude accounts failed; last error: {}", message),
    )
}

async fn post_anthropic_messages(
    client: &reqwest::Client,
    account: &super::accounts::ClaudeAccount,
    access_token: &str,
    incoming_headers: &HeaderMap,
    body: Bytes,
    stream: bool,
) -> Result<reqwest::Response, String> {
    let base_url = super::auth::api_base_url(account.api_base_url.as_deref());
    let beta = incoming_headers
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok());
    let mut request = client
        .post(format!("{}/v1/messages", base_url.trim_end_matches('/')))
        .headers(super::auth::anthropic_headers(access_token, beta))
        .header(
            "Accept",
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .header("Accept-Encoding", "identity")
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .body(body);
    for (key, value) in incoming_headers.iter() {
        let lower = key.as_str().to_ascii_lowercase();
        if should_drop_anthropic_incoming_header(&lower) {
            continue;
        }
        if matches!(
            lower.as_str(),
            "anthropic-version" | "anthropic-beta" | "accept" | "content-type"
        ) {
            continue;
        }
        request = request.header(key, value);
    }
    request
        .send()
        .await
        .map_err(|err| format!("Claude request failed: {}", err))
}

fn responses_to_anthropic_messages(
    raw: &Value,
    model: &str,
    stream: bool,
) -> Result<Value, String> {
    let mut object = Map::new();
    object.insert("model".to_string(), Value::String(model.to_string()));
    object.insert(
        "max_tokens".to_string(),
        raw.get("max_tokens")
            .or_else(|| raw.get("max_output_tokens"))
            .and_then(|value| value.as_u64())
            .unwrap_or(DEFAULT_MAX_TOKENS)
            .into(),
    );
    object.insert("stream".to_string(), Value::Bool(stream));
    if let Some(system) = raw
        .get("system")
        .or_else(|| raw.get("instructions"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert("system".to_string(), Value::String(system.to_string()));
    }
    if let Some(temperature) = raw.get("temperature").and_then(|value| value.as_f64()) {
        object.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = raw.get("top_p").and_then(|value| value.as_f64()) {
        object.insert("top_p".to_string(), json!(top_p));
    }
    if let Some(stop) = raw.get("stop_sequences").or_else(|| raw.get("stop")) {
        object.insert("stop_sequences".to_string(), normalize_stop_sequences(stop));
    }

    let messages = if let Some(messages) = raw.get("messages").and_then(|value| value.as_array()) {
        messages
            .iter()
            .filter_map(message_like_to_anthropic)
            .collect::<Vec<_>>()
    } else if let Some(input) = raw.get("input") {
        input_to_anthropic_messages(input)
    } else {
        Vec::new()
    };
    if messages.is_empty() {
        return Err("input or messages is required".to_string());
    }
    object.insert("messages".to_string(), Value::Array(messages));

    if let Some(tools) = normalize_tools(raw.get("tools")) {
        object.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(tool_choice) = normalize_tool_choice(raw.get("tool_choice")) {
        object.insert("tool_choice".to_string(), tool_choice);
    }

    Ok(Value::Object(object))
}

fn input_to_anthropic_messages(input: &Value) -> Vec<Value> {
    match input {
        Value::String(text) => vec![json!({"role": "user", "content": text})],
        Value::Array(items) => items.iter().filter_map(input_item_to_anthropic).collect(),
        Value::Object(_) => input_item_to_anthropic(input).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn input_item_to_anthropic(item: &Value) -> Option<Value> {
    let item_type = item
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match item_type {
        "function_call" => {
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(|value| value.as_str())
                .unwrap_or("call_claude");
            let name = item
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("tool");
            let input = item
                .get("arguments")
                .and_then(|value| value.as_str())
                .and_then(|value| serde_json::from_str::<Value>(value).ok())
                .or_else(|| item.get("input").cloned())
                .unwrap_or_else(|| json!({}));
            Some(json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": call_id, "name": name, "input": input}]
            }))
        }
        "function_call_output" | "tool_result" => {
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("tool_call_id"))
                .or_else(|| item.get("id"))
                .and_then(|value| value.as_str())
                .unwrap_or("call_claude");
            let output = item
                .get("output")
                .or_else(|| item.get("content"))
                .map(content_value_to_text)
                .unwrap_or_default();
            Some(json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": call_id, "content": output}]
            }))
        }
        "message" | "" => message_like_to_anthropic(item),
        _ => None,
    }
}

fn message_like_to_anthropic(message: &Value) -> Option<Value> {
    let role = normalize_anthropic_role(
        message
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("user"),
    );
    let content = message
        .get("content")
        .map(content_to_anthropic_parts)
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| {
            message
                .get("text")
                .and_then(|value| value.as_str())
                .map(|text| vec![json!({"type": "text", "text": text})])
                .unwrap_or_default()
        });
    if content.is_empty() {
        return None;
    }
    Some(json!({"role": role, "content": content}))
}

fn normalize_anthropic_role(role: &str) -> &'static str {
    match role.trim().to_ascii_lowercase().as_str() {
        "assistant" => "assistant",
        _ => "user",
    }
}

fn content_to_anthropic_parts(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![json!({"type": "text", "text": text})],
        Value::Array(parts) => parts.iter().filter_map(content_part_to_anthropic).collect(),
        Value::Object(_) => content_part_to_anthropic(content).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn content_part_to_anthropic(part: &Value) -> Option<Value> {
    let part_type = part
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match part_type {
        "text" | "input_text" | "output_text" | "" => part
            .get("text")
            .or_else(|| part.get("input_text"))
            .or_else(|| part.get("output_text"))
            .or_else(|| part.get("content"))
            .and_then(|value| value.as_str())
            .map(|text| json!({"type": "text", "text": text})),
        "tool_use" => Some(part.clone()),
        "tool_result" => Some(part.clone()),
        "input_image" | "image" => image_part_to_anthropic(part),
        _ => None,
    }
}

fn image_part_to_anthropic(part: &Value) -> Option<Value> {
    let url = part
        .get("image_url")
        .and_then(|value| {
            value.as_str().map(|value| value.to_string()).or_else(|| {
                value
                    .get("url")
                    .and_then(|url| url.as_str())
                    .map(|url| url.to_string())
            })
        })
        .or_else(|| {
            part.get("url")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })?;
    if let Some((media_type, data)) = parse_data_url(&url) {
        return Some(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data
            }
        }));
    }
    Some(json!({
        "type": "image",
        "source": {
            "type": "url",
            "url": url
        }
    }))
}

fn parse_data_url(value: &str) -> Option<(String, String)> {
    let rest = value.strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(";base64,")?;
    Some((media_type.to_string(), data.to_string()))
}

fn content_value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(content_value_to_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => map
            .get("text")
            .or_else(|| map.get("content"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| value.to_string()),
        _ => String::new(),
    }
}

fn normalize_tools(tools: Option<&Value>) -> Option<Vec<Value>> {
    let tools = tools?.as_array()?;
    let out = tools
        .iter()
        .filter_map(|tool| {
            if tool.get("type").and_then(|value| value.as_str()) != Some("function") {
                return None;
            }
            if let Some(function) = tool.get("function") {
                let name = function.get("name").and_then(|value| value.as_str())?;
                return Some(json!({
                    "name": name,
                    "description": function.get("description").and_then(|value| value.as_str()).unwrap_or(""),
                    "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
                }));
            }
            let name = tool.get("name").and_then(|value| value.as_str())?;
            Some(json!({
                "name": name,
                "description": tool.get("description").and_then(|value| value.as_str()).unwrap_or(""),
                "input_schema": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
            }))
        })
        .collect::<Vec<_>>();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn normalize_tool_choice(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::String(text)) => match text.as_str() {
            "auto" => Some(json!({"type": "auto"})),
            "required" => Some(json!({"type": "any"})),
            "none" => Some(json!({"type": "none"})),
            _ => None,
        },
        Some(Value::Object(map)) => {
            if let Some(function) = map.get("function").and_then(|value| value.as_object()) {
                if let Some(name) = function.get("name").and_then(|value| value.as_str()) {
                    return Some(json!({"type": "tool", "name": name}));
                }
            }
            if let Some(name) = map.get("name").and_then(|value| value.as_str()) {
                return Some(json!({"type": "tool", "name": name}));
            }
            None
        }
        _ => None,
    }
}

fn normalize_stop_sequences(value: &Value) -> Value {
    match value {
        Value::String(text) => json!([text]),
        Value::Array(_) => value.clone(),
        _ => json!([]),
    }
}

fn anthropic_message_to_response(message: &Value, model: &str) -> Value {
    let mut output = Vec::new();
    let mut text_parts = Vec::new();
    for block in message
        .get("content")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(|value| value.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|value| value.as_str()) {
                    text_parts.push(json!({"type": "output_text", "text": text}));
                }
            }
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(|value| value.as_str()) {
                    output.push(json!({
                        "type": "reasoning",
                        "summary": [{"type": "summary_text", "text": text}]
                    }));
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("call_claude");
                let name = block
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("tool");
                let arguments = block
                    .get("input")
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                output.push(json!({
                    "type": "function_call",
                    "id": id,
                    "call_id": id,
                    "name": name,
                    "arguments": arguments,
                    "status": "completed"
                }));
            }
            _ => {}
        }
    }
    if !text_parts.is_empty() {
        output.insert(
            0,
            json!({
                "type": "message",
                "role": "assistant",
                "content": text_parts,
                "status": "completed"
            }),
        );
    }

    let usage = message.get("usage").cloned().unwrap_or_else(|| json!({}));
    let input_tokens = usage
        .get("input_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    json!({
        "id": message.get("id").and_then(|value| value.as_str()).unwrap_or("resp_claude"),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": message.get("model").and_then(|value| value.as_str()).unwrap_or(model),
        "status": "completed",
        "output": output,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    })
}

fn responses_sse(response: Value) -> axum::response::Response {
    let mut events = String::new();
    events.push_str(&format!(
        "event: response.created\ndata: {}\n\n",
        json!({"type": "response.created", "response": response}).to_string()
    ));
    for item in response
        .get("output")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        events.push_str(&format!(
            "event: response.output_item.added\ndata: {}\n\n",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": item
            })
            .to_string()
        ));
    }
    events.push_str(&format!(
        "event: response.completed\ndata: {}\n\n",
        json!({"type": "response.completed", "response": response}).to_string()
    ));
    (
        StatusCode::OK,
        [
            ("Content-Type", "text/event-stream"),
            ("Cache-Control", "no-cache"),
        ],
        events,
    )
        .into_response()
}

async fn stream_anthropic_passthrough(
    state: crate::AppState,
    context: crate::UsageContext,
    resp: reqwest::Response,
    headers: HeaderMap,
) -> axum::response::Response {
    let usage_state = state.clone();
    let usage_context = context.clone();
    let stream = async_stream::stream! {
        let mut upstream = resp.bytes_stream();
        let mut parser = AnthropicSseUsageTracker::default();
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    parser.push(&bytes);
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
                Err(err) => {
                    let message = format!("Claude stream read failed: {}", err);
                    crate::record_claude_error(&usage_state, &usage_context, &message);
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, "stream"));
                    return;
                }
            }
        }
        if let Some(usage) = parser.finish() {
            crate::record_claude_success(&usage_state, &usage_context, &usage);
        } else {
            crate::record_claude_success(&usage_state, &usage_context, &crate::UsageMetrics::default());
        }
    };
    (StatusCode::OK, headers, Body::from_stream(stream)).into_response()
}

fn models_to_openai_entries(models: &[super::accounts::ClaudeModelInfo]) -> Vec<Value> {
    let source = if models.is_empty() {
        fallback_model_infos()
    } else {
        models.to_vec()
    };
    source
        .into_iter()
        .map(|model| {
            json!({
                "id": model.id,
                "object": "model",
                "created": 0,
                "owned_by": "anthropic",
                "display_name": model.display_name,
                "provider": super::PROVIDER_NAME,
                "upstream_model": model.id
            })
        })
        .collect()
}

fn fallback_models() -> Vec<Value> {
    models_to_openai_entries(&fallback_model_infos())
}

fn fallback_model_infos() -> Vec<super::accounts::ClaudeModelInfo> {
    [
        "claude-opus-4-1-20250805",
        "claude-opus-4-20250514",
        "claude-sonnet-4-20250514",
        "claude-3-7-sonnet-20250219",
        "claude-3-5-haiku-20241022",
    ]
    .iter()
    .map(|id| super::accounts::ClaudeModelInfo {
        id: (*id).to_string(),
        display_name: None,
        created_at: None,
        model_type: Some("model".to_string()),
    })
    .collect()
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
    (
        status,
        [("Content-Type", "application/json")],
        serde_json::to_vec(&json!({
            "type": "error",
            "error": {
                "type": error_type,
                "message": message
            }
        }))
        .unwrap_or_default(),
    )
        .into_response()
}

fn openai_error(status: StatusCode, error_type: &str, message: &str) -> axum::response::Response {
    (
        status,
        [("Content-Type", "application/json")],
        crate::source::v1::response::openai_error_body(message, error_type, None),
    )
        .into_response()
}

fn openai_error_body_from_anthropic(text: &str) -> Vec<u8> {
    let message = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })
        .unwrap_or_else(|| text.to_string());
    crate::source::v1::response::openai_error_body(&message, "server_error", None).to_vec()
}

#[derive(Default)]
struct AnthropicSseUsageTracker {
    buffer: Vec<u8>,
    usage: crate::UsageMetrics,
    saw_usage: bool,
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
        if self.saw_usage {
            Some(self.usage)
        } else {
            None
        }
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
            merge_usage_metrics(&mut self.usage, usage);
            self.saw_usage = true;
        }
    }
}

fn merge_usage_metrics(current: &mut crate::UsageMetrics, incoming: crate::UsageMetrics) {
    current.input_tokens = current.input_tokens.max(incoming.input_tokens);
    current.output_tokens = current.output_tokens.max(incoming.output_tokens);
    current.cache_tokens = current.cache_tokens.max(incoming.cache_tokens);
    current.reasoning_tokens = current.reasoning_tokens.max(incoming.reasoning_tokens);
    current.total_tokens = current
        .total_tokens
        .max(incoming.total_tokens)
        .max(current.input_tokens.saturating_add(current.output_tokens));
    if incoming.raw_usage.is_some() {
        current.raw_usage = incoming.raw_usage;
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
    fn responses_payload_maps_text_tools_and_tool_history() {
        let payload = json!({
            "model": "claude-sonnet-4-20250514",
            "instructions": "Be brief",
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "lookup alpha"}]},
                {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{\"q\":\"alpha\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "alpha=42"}
            ],
            "tools": [{"type": "function", "name": "lookup", "parameters": {"type": "object"}}],
            "tool_choice": "required",
            "max_output_tokens": 100
        });
        let out =
            responses_to_anthropic_messages(&payload, "claude-sonnet-4-20250514", false).unwrap();
        assert_eq!(out["system"], "Be brief");
        assert_eq!(out["messages"].as_array().unwrap().len(), 3);
        assert_eq!(out["tools"][0]["name"], "lookup");
        assert_eq!(out["tool_choice"]["type"], "any");
    }

    #[test]
    fn anthropic_tool_use_maps_to_responses_function_call() {
        let msg = json!({
            "id": "msg_1",
            "model": "claude-sonnet-4-20250514",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "trace", "input": {"ok": true}}],
            "usage": {"input_tokens": 5, "output_tokens": 2}
        });
        let out = anthropic_message_to_response(&msg, "claude-sonnet-4-20250514");
        assert_eq!(out["output"][0]["type"], "function_call");
        assert_eq!(out["output"][0]["arguments"], "{\"ok\":true}");
        assert_eq!(out["usage"]["total_tokens"], 7);
    }
}
