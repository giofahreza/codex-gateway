use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";

const MODEL_FALLBACKS: &[(&str, &str)] = &[
    ("grok-4.3", "Grok 4.3"),
    ("grok-4.1", "Grok 4.1"),
    ("grok-3", "Grok 3"),
    ("grok-imagine-image-quality", "Grok Imagine Image (Quality)"),
    ("grok-imagine-image", "Grok Imagine Image"),
    ("grok-imagine-video", "Grok Imagine Video"),
];

pub async fn models(State(state): State<crate::AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !crate::check_api_key(&state, &headers) {
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

    let models: Vec<serde_json::Value> = if let Some(account) =
        super::accounts::first_enabled(&state)
    {
        if !account.models.is_empty() {
            account
                .models
                .iter()
                .map(|model| {
                    serde_json::json!({
                        "id": model.model_id,
                        "object": "model",
                        "created": 0u64,
                        "owned_by": if model.owned_by.is_empty() { "xai" } else { model.owned_by.as_str() },
                        "display_name": if model.display_name.is_empty() { model.model_id.as_str() } else { model.display_name.as_str() },
                        "aliases": model.aliases,
                        "context_window": model.context_window,
                        "capabilities": ["chat", "text", "images", "video"]
                    })
                })
                .collect()
        } else {
            fallback_models()
        }
    } else {
        fallback_models()
    };

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
    if !crate::check_api_key(&state, &headers) {
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

    let payload_body = serde_json::to_string(&payload).unwrap_or_default();
    let accounts = super::accounts::candidate_accounts(&state);
    if accounts.is_empty() {
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

    let mut last_error: Option<(StatusCode, String)> = None;
    for (attempt_idx, account) in accounts.iter().enumerate() {
        let context = crate::grok_usage_context(
            account,
            Some(model.clone()),
            "/grok/v1/responses",
            prompt_metrics.clone(),
        );
        crate::record_request_started(&state, &context);

        let upstream_base = account
            .api_base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/');
        let upstream_url = format!("{}/responses", upstream_base);

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
            .body(payload_body.clone())
            .timeout(Duration::from_secs(180))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                let rate_limits = super::auth::extract_rate_limits(resp.headers());
                if !status.is_success() {
                    persist_runtime_metadata(
                        &state,
                        account,
                        Some(model.as_str()),
                        &rate_limits,
                        "after error",
                    );
                    let err_body = resp.text().await.unwrap_or_default();
                    let message = err_body.clone();
                    crate::record_grok_error(&state, &context, &message);
                    if attempt_idx + 1 < accounts.len()
                        && crate::should_retry_account_error(status, &message)
                    {
                        last_error = Some((status, message));
                        continue;
                    }
                    let body_bytes = Bytes::from(err_body);
                    return (
                        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                        [("Content-Type", "application/json")],
                        crate::source::v1::response::upstream_error_to_openai(status, &body_bytes),
                    )
                        .into_response();
                }

                if stream {
                    persist_runtime_metadata(
                        &state,
                        account,
                        Some(model.as_str()),
                        &rate_limits,
                        "",
                    );
                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, axum::Error>>(16);
                    let state_clone = state.clone();
                    let context_clone = context.clone();
                    let rl_headers = forwarded_ratelimit_headers(resp.headers());
                    tokio::spawn(async move {
                        let mut chunk_stream = resp.bytes_stream();
                        let mut buffer = Vec::new();
                        while let Some(chunk) =
                            futures_util::StreamExt::next(&mut chunk_stream).await
                        {
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
                        if let Some(usage) =
                            crate::usage_metrics_from_sse_response_body(&body_bytes)
                        {
                            crate::record_grok_success(&state_clone, &context_clone, &usage);
                        }
                    });

                    let stream_body = Body::from_stream(rx_stream(rx));
                    let mut response = (
                        StatusCode::OK,
                        [("Content-Type", "text/event-stream")],
                        stream_body,
                    )
                        .into_response();
                    let response_headers = response.headers_mut();
                    for (k, v) in rl_headers {
                        response_headers.insert(k, v);
                    }
                    return response;
                } else {
                    let rl_headers = forwarded_ratelimit_headers(resp.headers());
                    let body_bytes = resp.bytes().await.unwrap_or_default();
                    let response_value: serde_json::Value =
                        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
                    let usage = crate::usage_metrics_from_response_value(&response_value);
                    crate::record_grok_success(&state, &context, &usage);
                    let effective_model = response_value
                        .get("model")
                        .and_then(|v| v.as_str())
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or(model.as_str());
                    persist_runtime_metadata(
                        &state,
                        account,
                        Some(effective_model),
                        &rate_limits,
                        "",
                    );
                    let mut response = (
                        StatusCode::OK,
                        [("Content-Type", "application/json")],
                        body_bytes,
                    )
                        .into_response();
                    let response_headers = response.headers_mut();
                    for (k, v) in rl_headers {
                        response_headers.insert(k, v);
                    }
                    return response;
                }
            }
            Err(err) => {
                let message = format!("grok upstream unavailable: {}", err);
                crate::record_grok_error(&state, &context, &message);
                last_error = Some((StatusCode::BAD_GATEWAY, message));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        }
    }

    let (status, message) = last_error.unwrap_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "All Grok accounts failed".to_string(),
        )
    });
    (
        status,
        [("Content-Type", "application/json")],
        crate::source::v1::response::openai_error_body(
            &format!("All Grok accounts failed; last error: {}", message),
            "server_error",
            None,
        ),
    )
        .into_response()
}

/// Returns the `x-ratelimit-*` headers from the upstream response, ready to be
/// spread into the axum response tuple so callers can see the live quota on
/// every response.
fn forwarded_ratelimit_headers(
    headers: &reqwest::header::HeaderMap,
) -> Vec<(axum::http::HeaderName, axum::http::HeaderValue)> {
    let mut out = Vec::new();
    for (name, value) in headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if n.starts_with("x-ratelimit-") {
            if let (Ok(aname), Ok(avalue)) = (
                axum::http::HeaderName::from_bytes(name.as_str().as_bytes()),
                axum::http::HeaderValue::from_bytes(value.as_bytes()),
            ) {
                out.push((aname, avalue));
            }
        }
    }
    out
}

fn persist_runtime_metadata(
    state: &crate::AppState,
    account: &super::accounts::GrokAccount,
    effective_model: Option<&str>,
    rate_limits: &[super::auth::GrokRateLimitInfo],
    context: &str,
) {
    match super::auth::persist_runtime_metadata(
        &state.cfg,
        account.file_name.as_deref(),
        effective_model,
        rate_limits,
    ) {
        Ok(()) => super::accounts::update_runtime_metadata(
            state,
            account.file_name.as_deref(),
            effective_model,
            rate_limits,
        ),
        Err(err) => tracing::warn!(
            "failed to persist Grok runtime metadata {}: {}",
            context,
            err
        ),
    }
}

/// Forwards `POST /v1/images/generations` to `POST {account.api_base_url}/images/generations`.
pub async fn image_generations(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    proxy_simple(&state, &headers, &body, "images/generations").await
}

/// Forwards `POST /v1/videos/generations` to `POST {account.api_base_url}/videos/generations`.
pub async fn video_generations(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    proxy_simple(&state, &headers, &body, "videos/generations").await
}

async fn proxy_simple(
    state: &crate::AppState,
    headers: &HeaderMap,
    body: &Bytes,
    upstream_suffix: &str,
) -> axum::response::Response {
    if !crate::check_api_key(&state, headers) {
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

    let parsed_body: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let model = parsed_body
        .as_ref()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_string));
    let prompt_metrics = parsed_body
        .as_ref()
        .map(crate::prompt_metrics_from_request_value)
        .unwrap_or_default();

    let accounts = super::accounts::candidate_accounts(state);
    if accounts.is_empty() {
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

    let mut last_error: Option<(StatusCode, String)> = None;
    for (attempt_idx, account) in accounts.iter().enumerate() {
        let upstream_base = account
            .api_base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/');
        let upstream_url = format!("{}/{}", upstream_base, upstream_suffix);
        let context = crate::grok_usage_context(
            account,
            model.clone(),
            &format!("/grok/v1/{}", upstream_suffix),
            prompt_metrics.clone(),
        );
        crate::record_request_started(state, &context);

        match state
            .client
            .post(&upstream_url)
            .header(
                "Authorization",
                format!("{} {}", account.token_type, account.access_token),
            )
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .timeout(Duration::from_secs(180))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                let rate_limits = super::auth::extract_rate_limits(resp.headers());
                if !status.is_success() {
                    persist_runtime_metadata(
                        state,
                        account,
                        model.as_deref(),
                        &rate_limits,
                        "after error",
                    );
                    let err_body = resp.text().await.unwrap_or_default();
                    crate::record_grok_error(state, &context, &err_body);
                    if attempt_idx + 1 < accounts.len()
                        && crate::should_retry_account_error(status, &err_body)
                    {
                        last_error = Some((status, err_body));
                        continue;
                    }
                    let body_bytes = Bytes::from(err_body);
                    return (
                        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                        [("Content-Type", "application/json")],
                        crate::source::v1::response::upstream_error_to_openai(status, &body_bytes),
                    )
                        .into_response();
                }
                persist_runtime_metadata(state, account, model.as_deref(), &rate_limits, "");
                let rl_headers = forwarded_ratelimit_headers(resp.headers());
                let body_bytes = resp.bytes().await.unwrap_or_default();
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                    let usage = crate::usage_metrics_from_response_value(&v);
                    crate::record_grok_success(state, &context, &usage);
                }
                let mut response = (
                    StatusCode::OK,
                    [("Content-Type", "application/json")],
                    body_bytes,
                )
                    .into_response();
                let h = response.headers_mut();
                for (k, v) in rl_headers {
                    h.insert(k, v);
                }
                return response;
            }
            Err(err) => {
                let message = format!("grok upstream unavailable: {}", err);
                crate::record_grok_error(state, &context, &message);
                last_error = Some((StatusCode::BAD_GATEWAY, message));
                if attempt_idx + 1 < accounts.len() {
                    continue;
                }
                break;
            }
        }
    }

    let (status, message) = last_error.unwrap_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "All Grok accounts failed".to_string(),
        )
    });
    (
        status,
        [("Content-Type", "application/json")],
        crate::source::v1::response::openai_error_body(
            &format!("All Grok accounts failed; last error: {}", message),
            "server_error",
            None,
        ),
    )
        .into_response()
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

fn fallback_models() -> Vec<serde_json::Value> {
    MODEL_FALLBACKS
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
        .collect()
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_BASE_URL;

    #[test]
    fn grok_responses_url_matches_xai_docs() {
        let upstream_url = format!("{}/responses", DEFAULT_BASE_URL.trim_end_matches('/'));
        assert_eq!(upstream_url, "https://api.x.ai/v1/responses");
    }
}
