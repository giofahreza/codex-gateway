use crate::source::v1::multimodal::{classify_content, is_data_url, split_data_url, PartKind};
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use base64::Engine;
use bytes::Bytes;
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;

const MODEL_FALLBACKS: &[&str] = &[
    "claude-sonnet-4-5-thinking",
    "claude-opus-4-5-thinking",
    "claude-sonnet-4-5",
    "gemini-3-flash",
    "gemini-3-pro-high",
    "gemini-3-pro-low",
    "gemini-3-pro-image",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-thinking",
    "gemini-2.5-flash-lite",
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
                    "No Antigravity accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response()
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
                .into_response()
        }
    };

    match fetch_models(&state.client, &access_token).await {
        Ok(body) => (StatusCode::OK, [("Content-Type", "application/json")], body).into_response(),
        Err(err) => {
            let data = MODEL_FALLBACKS
                .iter()
                .map(|id| {
                    json!({
                        "id": id,
                        "object": "model",
                        "created": 0,
                        "owned_by": "antigravity"
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
                .into_response()
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
                .into_response()
        }
    };

    let account = match super::accounts::pick_account(&state) {
        Some(account) => account,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    "No Antigravity accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response()
        }
    };
    let context = crate::antigravity_usage_context(
        &account,
        Some(model.clone()),
        "/agw/v1/responses",
        crate::prompt_metrics_from_request_value(&request_value),
    );

    let payload = match build_google_payload(&request_value, &model, account.project_id.as_deref())
    {
        Ok(payload) => payload,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "invalid_request_error", None),
            )
                .into_response()
        }
    };

    crate::record_antigravity_request(&state, &context);

    let access_token = match super::auth::ensure_access_token(&state, &account).await {
        Ok(token) => token,
        Err(err) => {
            crate::record_antigravity_error(&state, &context, &err);
            return (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&err, "server_error", None),
            )
                .into_response();
        }
    };

    let upstream = match send_generate_request(&state.client, &access_token, &payload).await {
        Ok(value) => value,
        Err((status, message)) => {
            crate::record_antigravity_error(&state, &context, &message);
            return (
                status,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(&message, "server_error", None),
            )
                .into_response();
        }
    };

    let mut openai_response = google_to_openai_response(&upstream, &model);
    let usage = crate::usage_metrics_from_response_value(&openai_response);
    crate::record_antigravity_success(&state, &context, &usage);
    attach_temp_downloads(&mut openai_response);
    let wants_stream = crate::source::wants_stream(&headers, &body);

    if wants_stream {
        let sse = render_response_sse(&openai_response);
        return (
            StatusCode::OK,
            [
                ("Content-Type", "text/event-stream"),
                ("Cache-Control", "no-store"),
            ],
            Body::from(sse),
        )
            .into_response();
    }

    let body = serde_json::to_vec(&openai_response).unwrap_or_default();
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}

async fn fetch_models(client: &reqwest::Client, access_token: &str) -> Result<Vec<u8>, String> {
    let mut last_error = "failed to fetch models from Antigravity".to_string();

    for endpoint in super::auth::ANTIGRAVITY_ENDPOINTS {
        let resp = client
            .post(format!("{}/v1internal:fetchAvailableModels", endpoint))
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("User-Agent", super::auth::antigravity_user_agent())
            .header(
                "X-Goog-Api-Client",
                "google-cloud-sdk vscode_cloudshelleditor/0.1",
            )
            .header(
                "Client-Metadata",
                r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
            )
            .body("{}")
            .timeout(Duration::from_secs(30))
            .send()
            .await;

        let Ok(resp) = resp else {
            last_error = "failed to reach Antigravity model endpoint".to_string();
            continue;
        };

        if !resp.status().is_success() {
            last_error = format!("fetchAvailableModels returned {}", resp.status());
            continue;
        }

        let value: serde_json::Value = match resp.json().await {
            Ok(value) => value,
            Err(_) => {
                last_error = "failed to parse Antigravity models response".to_string();
                continue;
            }
        };
        let mut data = Vec::new();

        if let Some(models) = value.get("models").and_then(|v| v.as_object()) {
            for (id, model_data) in models {
                if !looks_like_antigravity_model(id) {
                    continue;
                }
                data.push(json!({
                    "id": id,
                    "object": "model",
                    "created": 0,
                    "owned_by": "antigravity",
                    "description": model_data.get("displayName").and_then(|v| v.as_str()).unwrap_or(id)
                }));
            }
        }

        if !data.is_empty() {
            return serde_json::to_vec(&json!({
                "object": "list",
                "data": data
            }))
            .map_err(|e| e.to_string());
        }
    }

    Err(last_error)
}

async fn send_generate_request(
    client: &reqwest::Client,
    access_token: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let mut last_error = "all Antigravity endpoints failed".to_string();

    for endpoint in super::auth::ANTIGRAVITY_ENDPOINTS {
        let resp = client
            .post(format!("{}/v1internal:generateContent", endpoint))
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("User-Agent", super::auth::antigravity_user_agent())
            .header(
                "X-Goog-Api-Client",
                "google-cloud-sdk vscode_cloudshelleditor/0.1",
            )
            .header(
                "Client-Metadata",
                r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
            )
            .body(payload.to_string())
            .timeout(Duration::from_secs(180))
            .send()
            .await;

        let Ok(resp) = resp else {
            last_error = "failed to reach Antigravity generateContent endpoint".to_string();
            continue;
        };

        let status = resp.status();
        let text = match resp.text().await {
            Ok(text) => text,
            Err(err) => {
                last_error = err.to_string();
                continue;
            }
        };
        if !status.is_success() {
            last_error = format!("generateContent returned {}: {}", status, text);
            continue;
        }

        let value: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                last_error = err.to_string();
                continue;
            }
        };
        return Ok(value);
    }

    Err((StatusCode::BAD_GATEWAY, last_error))
}

fn build_google_payload(
    request_value: &serde_json::Value,
    model: &str,
    project_id: Option<&str>,
) -> Result<serde_json::Value, String> {
    let parts = extract_prompt_parts(request_value)?;
    let instructions = request_value
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let image_mode = is_image_request(request_value, model);

    let mut request = json!({
        "contents": [{
            "role": "user",
            "parts": parts
        }],
        "sessionId": Uuid::new_v4().to_string()
    });

    if let Some(instructions) = instructions {
        request["systemInstruction"] = json!({
            "parts": [{
                "text": instructions
            }]
        });
    }

    let mut generation_config = serde_json::Map::new();
    if image_mode {
        generation_config.insert("responseModalities".to_string(), json!(["TEXT", "IMAGE"]));
    }
    if let Some(max_output_tokens) = request_value
        .get("max_output_tokens")
        .and_then(|v| v.as_u64())
    {
        generation_config.insert("maxOutputTokens".to_string(), json!(max_output_tokens));
    }
    if let Some(temperature) = request_value.get("temperature").and_then(|v| v.as_f64()) {
        generation_config.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request_value.get("top_p").and_then(|v| v.as_f64()) {
        generation_config.insert("topP".to_string(), json!(top_p));
    }
    if !generation_config.is_empty() {
        request["generationConfig"] = serde_json::Value::Object(generation_config);
    }

    Ok(json!({
        "project": project_id.unwrap_or("rising-fact-p41fc"),
        "model": model,
        "request": request,
        "userAgent": "antigravity",
        "requestId": format!("agent-{}", Uuid::new_v4())
    }))
}

/// Build a Google Generative AI `parts` array from a Codex/OpenAI
/// request. Supports text, image (data URL or remote URL), and a mix of
/// both. Accepts both `input` (string) and `messages[]` shapes.
fn extract_prompt_parts(request_value: &Value) -> Result<Vec<Value>, String> {
    let mut out: Vec<Value> = Vec::new();

    if let Some(prompt) = request_value.get("input").and_then(|v| v.as_str()) {
        if !prompt.trim().is_empty() {
            out.push(json!({ "text": prompt.trim() }));
        }
    }

    if let Some(items) = request_value.get("input").and_then(|v| v.as_array()) {
        for item in items {
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            if role != "user" {
                continue;
            }
            if let Some(content) = item.get("content") {
                for part in classify_content(Some(content)) {
                    push_google_part(&mut out, part);
                }
            }
        }
    }

    if let Some(messages) = request_value.get("messages").and_then(|v| v.as_array()) {
        for message in messages {
            let content = match message.get("content") {
                Some(c) => c,
                None => continue,
            };
            for part in classify_content(Some(content)) {
                push_google_part(&mut out, part);
            }
        }
    }

    if out.is_empty() {
        return Err(
            "only string input or messages[] with content is supported for /agw/v1/responses"
                .to_string(),
        );
    }
    Ok(out)
}

fn push_google_part(out: &mut Vec<Value>, part: PartKind) {
    match part {
        PartKind::Text(text) => {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                out.push(json!({ "text": trimmed }));
            }
        }
        PartKind::Image(url) => {
            if let Some(block) = google_inline_data_part(&url) {
                out.push(block);
            }
        }
        PartKind::Other(_) => {}
    }
}

fn google_inline_data_part(url: &str) -> Option<Value> {
    if is_data_url(url) {
        let (mime_type, payload) = split_data_url(url)?;
        return Some(json!({
            "inline_data": {
                "mime_type": mime_type,
                "data": payload,
            }
        }));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(json!({
            "file_data": {
                "file_uri": url,
            }
        }));
    }
    None
}

fn is_image_request(request_value: &serde_json::Value, model: &str) -> bool {
    if model.to_ascii_lowercase().contains("image") {
        return true;
    }

    request_value
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|tools| {
            tools.iter().any(|tool| {
                tool.get("type")
                    .and_then(|v| v.as_str())
                    .map(|kind| kind == "image_generation")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn looks_like_antigravity_model(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    lower.contains("claude") || lower.contains("gemini") || lower.contains("gpt")
}

fn google_to_openai_response(value: &serde_json::Value, model: &str) -> serde_json::Value {
    let response = value.get("response").unwrap_or(value);
    let parts = response
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
        .cloned()
        .unwrap_or_default();

    let usage = response.get("usageMetadata").cloned().unwrap_or_default();
    let mut output_text = String::new();
    let mut images = Vec::new();

    for part in parts {
        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                output_text.push_str(text);
            }
        }

        if let Some(inline_data) = part.get("inlineData").and_then(|v| v.as_object()) {
            let mime_type = inline_data
                .get("mimeType")
                .and_then(|v| v.as_str())
                .unwrap_or("image/png");
            let data = inline_data
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if mime_type.starts_with("image/") && !data.is_empty() {
                images.push(json!({
                    "mime_type": mime_type,
                    "b64_json": data
                }));
            }
        }
    }

    let mut output = Vec::new();
    if !output_text.is_empty() || images.is_empty() {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{}", Uuid::new_v4().simple()),
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": output_text,
                "annotations": []
            }]
        }));
    }

    for image in &images {
        output.push(json!({
            "type": "image_generation_call",
            "id": format!("ig_{}", Uuid::new_v4().simple()),
            "status": "completed",
            "result": image
        }));
    }

    json!({
        "id": format!("resp_{}", Uuid::new_v4().simple()),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": "completed",
        "model": model,
        "output": output,
        "output_text": output_text,
        "images": images,
        "usage": {
            "input_tokens": usage.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
            "output_tokens": usage.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
            "total_tokens": usage.get("totalTokenCount").and_then(|v| v.as_u64()).unwrap_or(
                usage.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0)
                    + usage.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0)
            ),
            "input_tokens_details": {
                "cached_tokens": usage.get("cachedContentTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
            },
            "output_tokens_details": {
                "reasoning_tokens": usage.get("thoughtsTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
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

    if let Some(text) = response.get("output_text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            for delta in text_delta_chunks(text) {
                chunks.extend_from_slice(
                    sse_json(&json!({
                        "type": "response.output_text.delta",
                        "delta": delta
                    }))
                    .as_slice(),
                );
            }
        }
    }

    if let Some(images) = response.get("images").and_then(|v| v.as_array()) {
        for image in images {
            if let Some(data) = image.get("b64_json").and_then(|v| v.as_str()) {
                chunks.extend_from_slice(
                    sse_json(&json!({
                        "type": "response.image_generation_call.partial_image",
                        "partial_image_b64": data
                    }))
                    .as_slice(),
                );
            }
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

fn text_delta_chunks(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive('\n').filter(|chunk| !chunk.is_empty())
}

fn attach_temp_downloads(response: &mut serde_json::Value) {
    let Some(root) = response.as_object_mut() else {
        return;
    };

    let Some(images) = root
        .get_mut("images")
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };

    if images.is_empty() {
        return;
    }

    if std::fs::create_dir_all("/tmp/gpt-gateway-downloads").is_err() {
        return;
    }

    for image in images.iter_mut() {
        let Some(image_obj) = image.as_object_mut() else {
            continue;
        };

        let mime_type = image_obj
            .get("mime_type")
            .and_then(|value| value.as_str())
            .unwrap_or("application/octet-stream");
        let b64 = image_obj
            .get("b64_json")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if b64.is_empty() {
            continue;
        }

        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            continue;
        };

        let ext = image_extension(mime_type);
        let file_name = format!("agw-{}.{}", Uuid::new_v4().simple(), ext);
        let file_path = format!("/tmp/gpt-gateway-downloads/{}", file_name);
        if std::fs::write(&file_path, bytes).is_err() {
            continue;
        }

        image_obj.insert(
            "download_url".to_string(),
            serde_json::Value::String(format!("/temp-files/{}", file_name)),
        );
        image_obj.insert(
            "file_path".to_string(),
            serde_json::Value::String(file_path),
        );
        image_obj.insert(
            "download_name".to_string(),
            serde_json::Value::String(file_name),
        );
    }

    let enriched_images = images.clone();
    if let Some(output_items) = root
        .get_mut("output")
        .and_then(|value| value.as_array_mut())
    {
        let mut image_iter = enriched_images.into_iter();
        for item in output_items.iter_mut() {
            let is_image_item = item
                .get("type")
                .and_then(|value| value.as_str())
                .map(|kind| kind == "image_generation_call")
                .unwrap_or(false);
            if !is_image_item {
                continue;
            }
            let Some(next_image) = image_iter.next() else {
                break;
            };
            if let Some(item_obj) = item.as_object_mut() {
                item_obj.insert("result".to_string(), next_image);
            }
        }
    }
}

fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_response_serializes_control_characters_as_valid_json() {
        let upstream = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "text": "hello\nworld\t\u{0008}"
                    }]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 3,
                "candidatesTokenCount": 4,
                "totalTokenCount": 7
            }
        });

        let response = google_to_openai_response(&upstream, "gemini-3-pro-high");
        let body = serde_json::to_vec(&response).unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap();

        let sse = String::from_utf8(render_response_sse(&response)).unwrap();
        for line in sse.lines() {
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            if data == "[DONE]" {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(data).unwrap();
        }
    }

    #[test]
    fn build_google_payload_passes_through_image_in_messages() {
        let request = json!({
            "model": "gemini-2.5-pro",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "describe" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
                ]
            }]
        });
        let payload = build_google_payload(&request, "gemini-2.5-pro", Some("proj-1")).unwrap();
        let parts = payload["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "describe");
        assert_eq!(parts[1]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[1]["inline_data"]["data"], "AAAA");
    }

    #[test]
    fn build_google_payload_passes_through_image_with_input_string() {
        let request = json!({
            "model": "gemini-2.5-pro",
            "input": "describe",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "input_image", "image_url": "https://example.com/x.png" }
                ]
            }]
        });
        let payload = build_google_payload(&request, "gemini-2.5-pro", Some("proj-1")).unwrap();
        let parts = payload["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap();
        assert!(parts.iter().any(|p| p.get("text").is_some()));
        assert!(parts.iter().any(|p| p.get("file_data").is_some()));
    }

    #[test]
    fn build_google_payload_passes_through_responses_input_array() {
        let request = json!({
            "model": "gemini-2.5-pro",
            "input": [{
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "describe" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }
                ]
            }]
        });
        let payload = build_google_payload(&request, "gemini-2.5-pro", Some("proj-1")).unwrap();
        let parts = payload["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "describe");
        assert_eq!(parts[1]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[1]["inline_data"]["data"], "AAAA");
    }
}
