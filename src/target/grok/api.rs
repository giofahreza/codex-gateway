use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.x.ai";

const MODEL_FALLBACKS: &[(&str, &str)] = &[
    ("grok-4.3", "Grok 4.3"),
    ("grok-4.1", "Grok 4.1"),
    ("grok-3", "Grok 3"),
    ("grok-imagine-image-quality", "Grok Imagine Image (Quality)"),
    ("grok-imagine-image", "Grok Imagine Image"),
    ("grok-imagine-video", "Grok Imagine Video"),
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

    let models: Vec<serde_json::Value> = MODEL_FALLBACKS
        .iter()
        .map(|(id, name)| {
            serde_json::json!({
                "id": id,
                "object": "model",
                "created": 0u64,
                "owned_by": "xai",
                "display_name": name,
                "capabilities": ["chat", "text", "images", "video"]
            })
        })
        .collect();

    axum::Json(serde_json::json!({
        "object": "list",
        "data": models
    }))
    .into_response()
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

    let account = match super::accounts::first_enabled(&state) {
        Some(a) => a,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    "No Grok accounts configured",
                    "server_error",
                    None,
                ),
            )
                .into_response();
        }
    };

    let request_value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
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

    let model = request_value
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("grok-4.3")
        .to_string();

    let stream = request_value
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let prompt_metrics = crate::prompt_metrics_from_request_value(&request_value);
    let context = crate::grok_usage_context(
        &account,
        Some(model.clone()),
        "/grok/v1/responses",
        prompt_metrics,
    );
    crate::record_request_started(&state, &context);

    let mut payload = serde_json::json!({
        "model": &model,
        "input": request_value.get("input").cloned().unwrap_or(serde_json::Value::Null),
        "store": false,
    });

    if let Some(instructions) = request_value
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        payload["instructions"] = serde_json::Value::String(instructions.to_string());
    }

    if let Some(tools) = request_value.get("tools").filter(|v| v.is_array()) {
        payload["tools"] = tools.clone();
        payload["tool_choice"] = serde_json::Value::String("auto".to_string());
        payload["parallel_tool_calls"] = serde_json::Value::Bool(true);
    }

    if stream {
        payload["stream"] = serde_json::Value::Bool(true);
    }

    let upstream_url = format!("{}/v1/responses", DEFAULT_BASE_URL.trim_end_matches('/'));

    match state
        .client
        .post(&upstream_url)
        .header(
            "Authorization",
            format!("{} {}", account.token_type, account.access_token),
        )
        .header("Content-Type", "application/json")
        .header(
            "Accept",
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .body(serde_json::to_string(&payload).unwrap_or_default())
        .timeout(Duration::from_secs(180))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let err_body = resp.text().await.unwrap_or_default();
                let message = err_body.clone();
                crate::record_grok_error(&state, &context, &message);
                let body_bytes = Bytes::from(err_body);
                return (
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                    [("Content-Type", "application/json")],
                    crate::source::v1::response::upstream_error_to_openai(status, &body_bytes),
                )
                    .into_response();
            }

            if stream {
                let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, axum::Error>>(16);
                let state_clone = state.clone();
                let context_clone = context.clone();
                tokio::spawn(async move {
                    let mut chunk_stream = resp.bytes_stream();
                    let mut buffer = Vec::new();
                    while let Some(chunk) = futures_util::StreamExt::next(&mut chunk_stream).await {
                        match chunk {
                            Ok(bytes) => {
                                buffer.extend_from_slice(&bytes);
                                let _ = tx.send(Ok(bytes)).await;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(axum::Error::new(e))).await;
                                break;
                            }
                        }
                    }
                    let body_bytes = Bytes::from(buffer);
                    if let Some(usage) = crate::usage_metrics_from_sse_response_body(&body_bytes) {
                        crate::record_grok_success(&state_clone, &context_clone, &usage);
                    }
                });

                let stream_body = Body::from_stream(rx_stream(rx));
                (
                    StatusCode::OK,
                    [("Content-Type", "text/event-stream")],
                    stream_body,
                )
                    .into_response()
            } else {
                let body_bytes = resp.bytes().await.unwrap_or_default();
                let response_value: serde_json::Value =
                    serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
                let usage = crate::usage_metrics_from_response_value(&response_value);
                crate::record_grok_success(&state, &context, &usage);
                (
                    StatusCode::OK,
                    [("Content-Type", "application/json")],
                    body_bytes,
                )
                    .into_response()
            }
        }
        Err(err) => {
            let message = format!("grok upstream unavailable: {}", err);
            crate::record_grok_error(&state, &context, &message);
            (
                StatusCode::BAD_GATEWAY,
                [("Content-Type", "application/json")],
                crate::source::v1::response::openai_error_body(
                    &format!("grok upstream unavailable: {}", err),
                    "server_error",
                    None,
                ),
            )
                .into_response()
        }
    }
}

fn rx_stream(
    mut rx: tokio::sync::mpsc::Receiver<Result<Bytes, axum::Error>>,
) -> impl futures_util::Stream<Item = Result<Bytes, axum::Error>> {
    async_stream::stream! {
        while let Some(chunk) = rx.recv().await {
            yield chunk;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_BASE_URL;

    #[test]
    fn grok_responses_url_matches_xai_docs() {
        let upstream_url = format!("{}/v1/responses", DEFAULT_BASE_URL.trim_end_matches('/'));
        assert_eq!(upstream_url, "https://api.x.ai/v1/responses");
    }
}
