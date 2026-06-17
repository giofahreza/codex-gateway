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

const MODEL_FALLBACKS: &[(&str, &str, &str)] = &[
    (
        "qwen3-coder-plus",
        "Qwen3 Coder Plus",
        "Advanced code generation and understanding model",
    ),
    (
        "qwen3-coder-flash",
        "Qwen3 Coder Flash",
        "Fast code generation model",
    ),
    ("vision-model", "Qwen3 Vision Model", "Vision model"),
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
                    "No Qwen accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let access_token = match super::auth::ensure_access_token(&state, &account).await {
        Ok(token) => token,
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "server_error", None),
            )
                .into_response();
        }
    };

    match fetch_models(
        &state.client,
        &access_token,
        &super::auth::base_url(&state, &account),
    )
    .await
    {
        Ok(body) => (StatusCode::OK, [("Content-Type", "application/json")], body).into_response(),
        Err(err) => {
            let data = MODEL_FALLBACKS
                .iter()
                .map(|(id, display_name, description)| {
                    json!({
                        "id": id,
                        "object": "model",
                        "created": 0,
                        "owned_by": "qwen",
                        "display_name": display_name,
                        "description": description
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
                    "No Qwen accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };
    let context = crate::qwen_usage_context(
        &account,
        Some(model.clone()),
        "/qwen/v1/responses",
        crate::prompt_metrics_from_request_value(&request_value),
    );

    crate::record_qwen_request(&state, &context);

    let access_token = match super::auth::ensure_access_token(&state, &account).await {
        Ok(token) => token,
        Err(err) => {
            crate::record_qwen_error(&state, &context, &err);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "server_error", None),
            )
                .into_response();
        }
    };

    let upstream = match send_chat_request(
        &state.client,
        &access_token,
        &super::auth::base_url(&state, &account),
        &payload,
    )
    .await
    {
        Ok(value) => value,
        Err((status, message)) => {
            crate::record_qwen_error(&state, &context, &message);
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
    crate::record_qwen_success(&state, &context, &usage);
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

async fn fetch_models(
    client: &reqwest::Client,
    access_token: &str,
    base_url: &str,
) -> Result<Vec<u8>, String> {
    let request = client
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30));
    let resp = super::auth::qwen_headers(request, access_token)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Qwen models endpoint returned {}", body));
    }

    let value: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if value.get("data").and_then(|v| v.as_array()).is_some() {
        return serde_json::to_vec(&value).map_err(|e| e.to_string());
    }

    let Some(models) = value.get("models").and_then(|v| v.as_array()) else {
        return Err("Qwen models response was missing data".to_string());
    };

    let data = models
        .iter()
        .filter_map(|model| {
            let id = model.get("id").and_then(|v| v.as_str())?;
            Some(json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": "qwen"
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
    access_token: &str,
    base_url: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let request = client
        .post(format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(payload.to_string())
        .timeout(Duration::from_secs(180));
    let resp = super::auth::qwen_headers(request, access_token)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    if !status.is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Qwen returned {}: {}", status, text),
        ));
    }

    serde_json::from_str(&text).map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("failed to parse Qwen response: {}", e),
        )
    })
}

fn build_chat_payload(
    request_value: &serde_json::Value,
    model: &str,
) -> Result<serde_json::Value, String> {
    let messages = if let Some(messages) = request_value.get("messages").and_then(|v| v.as_array())
    {
        if messages.is_empty() {
            return Err("messages must not be empty".to_string());
        }
        messages.clone()
    } else {
        build_messages_from_input(request_value)?
    };

    let mut payload = json!({
        "model": model,
        "messages": messages,
        "stream": false
    });

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

    Ok(payload)
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
        return Err("only string or array input is supported for /qwen/v1/responses".to_string());
    };

    for item in items {
        if let Some(text) = extract_text_from_input_item(item) {
            let role = item
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("user");
            messages.push(json!({
                "role": role,
                "content": text
            }));
        }
    }

    if messages.is_empty() {
        return Err("input did not contain any text content".to_string());
    }

    Ok(messages)
}

fn extract_text_from_input_item(item: &serde_json::Value) -> Option<String> {
    if let Some(content) = item.get("content").and_then(|v| v.as_str()) {
        let content = content.trim();
        if !content.is_empty() {
            return Some(content.to_string());
        }
    }

    let parts = item.get("content").and_then(|v| v.as_array())?;
    let mut out = String::new();
    for part in parts {
        let part_type = part
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let text = part
            .get("text")
            .and_then(|value| value.as_str())
            .or_else(|| part.get("input_text").and_then(|value| value.as_str()));
        if matches!(part_type, "text" | "input_text" | "output_text") {
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

fn chat_to_openai_response(value: &serde_json::Value, model: &str) -> serde_json::Value {
    let content = value
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .map(extract_content_text)
        .unwrap_or_default();

    let usage = value.get("usage").cloned().unwrap_or_default();

    json!({
        "id": format!(
            "resp_{}",
            value
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(&Uuid::new_v4().simple().to_string())
        ),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": "completed",
        "model": model,
        "output": [{
            "type": "message",
            "id": format!("msg_{}", Uuid::new_v4().simple()),
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": content,
                "annotations": []
            }]
        }],
        "output_text": content,
        "usage": {
            "input_tokens": usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "output_tokens": usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "total_tokens": usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
        }
    })
}

fn extract_content_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }

    let Some(parts) = content.as_array() else {
        return String::new();
    };

    let mut out = String::new();
    for part in parts {
        let text = part
            .get("text")
            .and_then(|value| value.as_str())
            .or_else(|| part.get("content").and_then(|value| value.as_str()));
        if let Some(text) = text.map(str::trim).filter(|value| !value.is_empty()) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
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
