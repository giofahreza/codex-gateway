use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const MODEL_FALLBACKS: &[(&str, &str)] = &[
    ("deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("deepseek-v4-pro", "DeepSeek V4 Pro"),
    ("deepseek-chat", "DeepSeek Chat"),
    ("deepseek-reasoner", "DeepSeek Reasoner"),
];

pub fn normalize_base_url(base_url: Option<&str>) -> String {
    base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/')
        .to_string()
}

pub async fn validate_api_key(
    client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
) -> Result<(), String> {
    fetch_models_json(client, api_key, base_url)
        .await
        .map(|_| ())
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
                    "No DeepSeek accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    match fetch_models_json(
        &state.client,
        &account.api_key,
        &normalize_base_url(account.base_url.as_deref()),
    )
    .await
    {
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
                .map(|(id, label)| {
                    json!({
                        "id": id,
                        "object": "model",
                        "created": 0,
                        "owned_by": "deepseek",
                        "display_name": label
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

    let request_value: serde_json::Value = match serde_json::from_slice(&body) {
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

    let model = match request_value.get("model").and_then(|v| v.as_str()) {
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

    let payload = match build_chat_payload(&request_value, &model) {
        Ok(payload) => payload,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "invalid_request_error", None),
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
                    "No DeepSeek accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };
    let context = crate::deepseek_usage_context(
        &account,
        Some(model.clone()),
        "/deepseek/v1/responses",
        crate::prompt_metrics_from_request_value(&request_value),
    );
    crate::record_deepseek_request(&state, &context);

    let upstream = match send_chat_request(
        &state.client,
        &account.api_key,
        &normalize_base_url(account.base_url.as_deref()),
        &payload,
    )
    .await
    {
        Ok(value) => value,
        Err((status, message)) => {
            crate::record_deepseek_error(&state, &context, &message);
            return (
                status,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };

    let response = chat_to_openai_response(&upstream, &model);
    let usage = crate::usage_metrics_from_response_value(&response);
    crate::record_deepseek_success(&state, &context, &usage);

    if crate::source::wants_stream(&headers, &body) {
        return (
            StatusCode::OK,
            [
                ("Content-Type", "text/event-stream"),
                ("Cache-Control", "no-store"),
            ],
            Body::from(render_response_sse(&response)),
        )
            .into_response();
    }

    let body = serde_json::to_vec(&response).unwrap_or_default();
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}

async fn fetch_models_json(
    client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
) -> Result<serde_json::Value, String> {
    let resp = client
        .get(format!("{}/models", normalize_base_url(Some(base_url))))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("DeepSeek models request failed: {}", e))?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("DeepSeek models returned {}: {}", status, text));
    }

    serde_json::from_str(&text).map_err(|e| format!("invalid DeepSeek models JSON: {}", e))
}

fn models_to_openai_json(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let Some(models) = value.get("data").and_then(|v| v.as_array()) else {
        return Err("DeepSeek models response was missing data".to_string());
    };

    let data = models
        .iter()
        .filter_map(|model| {
            let id = model.get("id").and_then(|v| v.as_str())?;
            Some(json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": model.get("owned_by").and_then(|v| v.as_str()).unwrap_or("deepseek")
            }))
        })
        .collect::<Vec<_>>();

    serde_json::to_vec(&json!({
        "object": "list",
        "data": data
    }))
    .map_err(|e| e.to_string())
}

async fn send_chat_request(
    client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let resp = client
        .post(format!(
            "{}/chat/completions",
            normalize_base_url(Some(base_url))
        ))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(payload.to_string())
        .timeout(Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("DeepSeek request failed: {}", e),
            )
        })?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    if !status.is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("DeepSeek returned {}: {}", status, text),
        ));
    }

    serde_json::from_str(&text).map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("failed to parse DeepSeek response: {}", e),
        )
    })
}

fn build_chat_payload(
    request_value: &serde_json::Value,
    model: &str,
) -> Result<serde_json::Value, String> {
    let messages = if let Some(messages) = request_value.get("messages").and_then(|v| v.as_array())
    {
        normalize_chat_messages(messages)?
    } else {
        build_messages_from_input(request_value)?
    };

    if messages.is_empty() {
        return Err("messages must not be empty".to_string());
    }

    let mut payload = json!({
        "model": model,
        "messages": messages,
        "stream": false
    });

    if let Some(tools) = build_tools(request_value)? {
        payload["tools"] = tools;
    }

    if let Some(max_output_tokens) = request_value
        .get("max_output_tokens")
        .and_then(|v| v.as_u64())
    {
        payload["max_tokens"] = json!(max_output_tokens);
    }
    if let Some(temperature) = request_value.get("temperature").and_then(|v| v.as_f64()) {
        payload["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request_value.get("top_p").and_then(|v| v.as_f64()) {
        payload["top_p"] = json!(top_p);
    }
    if let Some(stop) = request_value.get("stop") {
        payload["stop"] = stop.clone();
    }
    if let Some(thinking) = build_thinking(request_value) {
        payload["thinking"] = thinking;
    }
    if let Some(reasoning_effort) = build_reasoning_effort(request_value) {
        payload["reasoning_effort"] = json!(reasoning_effort);
    }
    if let Some(response_format) = build_response_format(request_value) {
        payload["response_format"] = response_format;
    }

    Ok(payload)
}

fn build_tools(request_value: &serde_json::Value) -> Result<Option<serde_json::Value>, String> {
    let Some(tools) = request_value.get("tools").and_then(|v| v.as_array()) else {
        return Ok(None);
    };

    let mut mapped = Vec::new();
    for tool in tools {
        if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
            continue;
        }
        let Some(function) = tool.get("function") else {
            return Err("tool.function is required".to_string());
        };
        let Some(name) = function.get("name").and_then(|v| v.as_str()) else {
            return Err("tool.function.name is required".to_string());
        };
        let mut mapped_function = json!({
            "name": name
        });
        if let Some(description) = function.get("description").and_then(|v| v.as_str()) {
            mapped_function["description"] = json!(description);
        }
        if let Some(parameters) = function.get("parameters") {
            mapped_function["parameters"] = parameters.clone();
        }
        if let Some(strict) = function.get("strict").and_then(|v| v.as_bool()) {
            mapped_function["strict"] = json!(strict);
        }
        mapped.push(json!({
            "type": "function",
            "function": mapped_function
        }));
    }

    if mapped.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::Value::Array(mapped)))
    }
}

fn build_thinking(request_value: &serde_json::Value) -> Option<serde_json::Value> {
    if request_value.get("reasoning").is_none() && request_value.get("reasoning_effort").is_none() {
        None
    } else {
        Some(json!({ "type": "enabled" }))
    }
}

fn build_reasoning_effort(request_value: &serde_json::Value) -> Option<&'static str> {
    request_value
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            request_value
                .get("reasoning_effort")
                .and_then(|v| v.as_str())
        })
        .and_then(map_reasoning_effort)
}

fn build_response_format(request_value: &serde_json::Value) -> Option<serde_json::Value> {
    let format_type = request_value
        .get("response_format")
        .and_then(|value| value.get("type"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            request_value
                .get("text")
                .and_then(|value| value.get("format"))
                .and_then(|value| value.get("type"))
                .and_then(|v| v.as_str())
        })?;

    match format_type {
        "json_object" | "json_schema" => Some(json!({ "type": "json_object" })),
        _ => None,
    }
}

fn map_reasoning_effort(effort: &str) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" | "medium" | "high" => Some("high"),
        "xhigh" | "max" => Some("max"),
        _ => None,
    }
}

fn normalize_chat_messages(
    messages: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let content = extract_text_value(message.get("content"));
        let content_text = content.clone().unwrap_or_default();
        let mut normalized = json!({
            "role": role,
            "content": content_text
        });

        if role == "assistant" {
            if let Some(reasoning_content) = message
                .get("reasoning_content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized["reasoning_content"] = json!(reasoning_content);
            }
            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                normalized["content"] = json!(content.clone().unwrap_or_default());
                normalized["tool_calls"] =
                    serde_json::Value::Array(normalize_direct_tool_calls(tool_calls)?);
            }
        } else if role == "tool" {
            let tool_call_id = message
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "tool messages require tool_call_id".to_string())?;
            normalized["tool_call_id"] = json!(tool_call_id);
        }

        if role != "assistant"
            && role != "tool"
            && content
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
        {
            continue;
        }
        out.push(normalized);
    }
    Ok(out)
}

fn normalize_direct_tool_calls(
    tool_calls: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, String> {
    let mut normalized = Vec::new();
    for tool_call in tool_calls {
        let tool_type = tool_call
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("function");
        let function = tool_call
            .get("function")
            .ok_or_else(|| "assistant tool_calls require function payloads".to_string())?;
        let name = function
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "assistant tool_calls require function.name".to_string())?;
        let arguments = function
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let tool_call_id = tool_call
            .get("id")
            .and_then(|v| v.as_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
        normalized.push(json!({
            "id": tool_call_id,
            "type": tool_type,
            "function": {
                "name": name,
                "arguments": arguments
            }
        }));
    }
    Ok(normalized)
}

fn build_messages_from_input(
    request_value: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, String> {
    let mut messages = Vec::new();

    if let Some(instructions) = request_value
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        messages.push(json!({
            "role": "system",
            "content": instructions
        }));
    }

    let input = request_value
        .get("input")
        .ok_or_else(|| "input is required when messages are not provided".to_string())?;

    if let Some(prompt) = input
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        messages.push(json!({
            "role": "user",
            "content": prompt
        }));
        return Ok(messages);
    }

    let Some(items) = input.as_array() else {
        return Err(
            "only string or array input is supported for /deepseek/v1/responses".to_string(),
        );
    };

    let mut pending = PendingAssistantTurn::default();

    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match item_type {
            "reasoning" => {
                if let Some(text) = extract_reasoning_text(item) {
                    pending.push_reasoning(text);
                }
            }
            "function_call" => {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "function_call items require call_id".to_string())?;
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "function_call items require name".to_string())?;
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                if let Some(reasoning_content) = item
                    .get("reasoning_content")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    pending.push_reasoning(reasoning_content.to_string());
                }
                pending.tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments
                    }
                }));
            }
            "function_call_output" => {
                flush_pending_assistant_turn(&mut messages, &mut pending);
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "function_call_output items require call_id".to_string())?;
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": stringify_tool_output(item.get("output").or_else(|| item.get("content")))
                }));
            }
            _ => {
                let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                let content = extract_text_value(Some(item));
                let direct_tool_calls = item.get("tool_calls").and_then(|v| v.as_array());

                if role == "assistant" && direct_tool_calls.is_some() {
                    flush_pending_assistant_turn(&mut messages, &mut pending);
                    let mut assistant = json!({
                        "role": "assistant",
                        "content": content.unwrap_or_default(),
                        "tool_calls": serde_json::Value::Array(
                            normalize_direct_tool_calls(direct_tool_calls.unwrap())?
                        )
                    });
                    if let Some(reasoning_content) = item
                        .get("reasoning_content")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        assistant["reasoning_content"] = json!(reasoning_content);
                    }
                    messages.push(assistant);
                    continue;
                }

                flush_pending_assistant_turn(&mut messages, &mut pending);
                if role == "assistant" {
                    if let Some(text) = content {
                        messages.push(json!({
                            "role": "assistant",
                            "content": text
                        }));
                    }
                } else if role == "tool" {
                    let Some(tool_call_id) = item.get("tool_call_id").and_then(|v| v.as_str())
                    else {
                        return Err("tool items require tool_call_id".to_string());
                    };
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": content.unwrap_or_default()
                    }));
                } else if let Some(text) = content {
                    messages.push(json!({
                        "role": role,
                        "content": text
                    }));
                }
            }
        }
    }

    flush_pending_assistant_turn(&mut messages, &mut pending);

    if messages.is_empty() {
        return Err("input did not contain any text or tool content".to_string());
    }

    Ok(messages)
}

#[derive(Default)]
struct PendingAssistantTurn {
    reasoning_content: Option<String>,
    tool_calls: Vec<serde_json::Value>,
}

impl PendingAssistantTurn {
    fn push_reasoning(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        match &mut self.reasoning_content {
            Some(existing) => {
                if !existing.is_empty() {
                    existing.push('\n');
                }
                existing.push_str(&text);
            }
            None => self.reasoning_content = Some(text),
        }
    }
}

fn flush_pending_assistant_turn(
    messages: &mut Vec<serde_json::Value>,
    pending: &mut PendingAssistantTurn,
) {
    if pending.tool_calls.is_empty() {
        pending.reasoning_content = None;
        return;
    }

    let mut assistant = json!({
        "role": "assistant",
        "content": "",
        "tool_calls": serde_json::Value::Array(std::mem::take(&mut pending.tool_calls))
    });
    if let Some(reasoning_content) = pending.reasoning_content.take() {
        assistant["reasoning_content"] = json!(reasoning_content);
    }
    messages.push(assistant);
}

fn extract_text_value(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?;
    if let Some(content) = value.as_str() {
        let content = content.trim();
        if !content.is_empty() {
            return Some(content.to_string());
        }
    }

    if let Some(content) = value.get("content").and_then(|v| v.as_str()) {
        let content = content.trim();
        if !content.is_empty() {
            return Some(content.to_string());
        }
    }

    let parts = value
        .get("content")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())?;
    let mut out = String::new();
    for part in parts {
        let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let text = part
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| part.get("input_text").and_then(|v| v.as_str()))
            .or_else(|| part.get("output_text").and_then(|v| v.as_str()))
            .or_else(|| part.get("content").and_then(|v| v.as_str()));
        if matches!(
            part_type,
            "" | "text" | "input_text" | "output_text" | "summary_text"
        ) {
            if let Some(text) = text.map(str::trim).filter(|value| !value.is_empty()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_reasoning_text(item: &serde_json::Value) -> Option<String> {
    if let Some(reasoning_content) = item
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(reasoning_content.to_string());
    }
    if let Some(text) = extract_text_value(item.get("summary")) {
        return Some(text);
    }
    extract_text_value(item.get("content"))
}

fn stringify_tool_output(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    serde_json::to_string(value).unwrap_or_else(|_| String::new())
}

fn chat_to_openai_response(value: &serde_json::Value, model: &str) -> serde_json::Value {
    let choice = value
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|choices| choices.first());
    let message = choice.and_then(|choice| choice.get("message"));
    let content = message
        .and_then(|message| message.get("content"))
        .and_then(|value| extract_text_value(Some(value)))
        .unwrap_or_default();
    let reasoning_content = message
        .and_then(|message| message.get("reasoning_content"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let tool_calls = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let usage = value.get("usage").cloned().unwrap_or_default();
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|v| v.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| usage.get("reasoning_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0);

    let mut output = Vec::new();
    if let Some(reasoning_content) = reasoning_content.as_ref() {
        output.push(json!({
            "id": format!("rs_{}", Uuid::new_v4().simple()),
            "type": "reasoning",
            "summary": [{
                "type": "summary_text",
                "text": reasoning_content
            }],
            "content": reasoning_content
        }));
    }

    if !tool_calls.is_empty() {
        for tool_call in tool_calls {
            let call_id = tool_call
                .get("id")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
                .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
            let function = tool_call.get("function").cloned().unwrap_or_default();
            let name = function
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("tool");
            let arguments = function
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let mut function_call = json!({
                "id": format!("fc_{}", Uuid::new_v4().simple()),
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
                "status": "completed"
            });
            if let Some(reasoning_content) = reasoning_content.as_ref() {
                function_call["reasoning_content"] = json!(reasoning_content);
            }
            output.push(function_call);
        }
    }

    if output.is_empty() || !content.trim().is_empty() {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{}", Uuid::new_v4().simple()),
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": content,
                "annotations": []
            }]
        }));
    }

    json!({
        "id": format!(
            "resp_{}",
            value
                .get("id")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string())
        ),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": "completed",
        "model": model,
        "output": output,
        "output_text": content,
        "usage": {
            "input_tokens": usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "output_tokens": usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "total_tokens": usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "input_tokens_details": {
                "cached_tokens": usage.get("prompt_cache_hit_tokens").and_then(|v| v.as_u64())
                    .or_else(|| usage.get("prompt_tokens_details").and_then(|v| v.get("cached_tokens")).and_then(|v| v.as_u64()))
                    .unwrap_or(0),
            },
            "output_tokens_details": {
                "reasoning_tokens": reasoning_tokens
            }
        }
    })
}

fn render_response_sse(response: &serde_json::Value) -> Vec<u8> {
    let mut chunks = Vec::new();

    let mut created = response.clone();
    if let Some(object) = created.as_object_mut() {
        object.insert("status".to_string(), json!("in_progress"));
    }
    chunks.extend_from_slice(
        sse_json(&json!({
            "type": "response.created",
            "response": created
        }))
        .as_slice(),
    );

    if let Some(output) = response.get("output").and_then(|v| v.as_array()) {
        for (index, item) in output.iter().enumerate() {
            chunks.extend_from_slice(
                sse_json(&json!({
                    "type": "response.output_item.done",
                    "output_index": index,
                    "item": item
                }))
                .as_slice(),
            );
        }
    }

    if let Some(text) = response.get("output_text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            chunks.extend_from_slice(
                sse_json(&json!({
                    "type": "response.output_text.delta",
                    "delta": text
                }))
                .as_slice(),
            );
        }
    }

    chunks.extend_from_slice(
        sse_json(&json!({
            "type": "response.completed",
            "response": response
        }))
        .as_slice(),
    );
    chunks.extend_from_slice(b"data: [DONE]\n\n");
    chunks
}

fn sse_json(value: &serde_json::Value) -> Vec<u8> {
    let data = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    let mut out = String::new();
    out.push_str("data: ");
    out.push_str(&data);
    out.push_str("\n\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_chat_payload_preserves_tool_call_turns_and_outputs() {
        let request = json!({
            "model": "deepseek-v4-pro",
            "instructions": "You are helpful",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Look up weather",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }],
            "reasoning": {
                "effort": "xhigh"
            },
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "Weather in Tokyo?" }]
                },
                {
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": "Need weather tool." }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_weather",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Tokyo\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_weather",
                    "output": "{\"temp_c\":24}"
                }
            ]
        });

        let payload = build_chat_payload(&request, "deepseek-v4-pro").unwrap();
        let messages = payload["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "");
        assert_eq!(messages[2]["reasoning_content"], "Need weather tool.");
        assert_eq!(
            messages[2]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_weather");
        assert_eq!(messages[3]["content"], "{\"temp_c\":24}");
        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["reasoning_effort"], "max");
    }

    #[test]
    fn chat_to_openai_response_maps_reasoning_and_function_calls() {
        let upstream = json!({
            "id": "chatcmpl-test",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "completion_tokens_details": {
                    "reasoning_tokens": 3
                }
            },
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning_content": "Need the weather tool first.",
                    "tool_calls": [{
                        "id": "call_weather",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Tokyo\"}"
                        }
                    }]
                }
            }]
        });

        let response = chat_to_openai_response(&upstream, "deepseek-v4-pro");
        let output = response["output"].as_array().unwrap();

        assert_eq!(response["object"], "response");
        assert_eq!(response["model"], "deepseek-v4-pro");
        assert_eq!(
            response["usage"]["output_tokens_details"]["reasoning_tokens"],
            3
        );
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["type"], "function_call");
        assert_eq!(output[1]["call_id"], "call_weather");
        assert_eq!(output[1]["name"], "get_weather");
        assert_eq!(output[1]["arguments"], "{\"city\":\"Tokyo\"}");
    }
}
