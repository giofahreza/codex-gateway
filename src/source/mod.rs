use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;

pub mod claude;
pub mod codex;
pub mod v1;

#[derive(Debug, Clone, Copy)]
pub enum TargetModel {
    Codex,
}

#[derive(Debug, Clone, Copy)]
pub enum ResponseMode {
    Passthrough,
    SseToJson,
}

#[derive(Debug, Clone, Copy)]
pub struct RouteError {
    pub status: axum::http::StatusCode,
    pub message: &'static str,
}

#[derive(Debug, Clone)]
pub struct RoutedRequest {
    pub target: TargetModel,
    pub upstream_path: String,
    pub upstream_query: Option<String>,
    pub upstream_body: Bytes,
    pub response_mode: ResponseMode,
}

pub fn route_request(
    path: &str,
    uri: &Uri,
    method: &Method,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<RoutedRequest, RouteError> {
    let trimmed = path.trim_start_matches('/');
    if trimmed == "v1" || trimmed.starts_with("v1/") {
        return v1::route_to_target(path, uri, method, headers, body);
    }
    if trimmed == "codex" || trimmed.starts_with("codex/") {
        return codex::route_to_target(path, uri, method, headers, body);
    }
    if trimmed == "claude" || trimmed.starts_with("claude/") {
        return claude::route_to_target(path, uri, method, headers, body);
    }
    // Backward compatibility: old behavior treated bare routes as OpenAI-compatible.
    v1::route_to_target(path, uri, method, headers, body)
}

pub(crate) fn strip_prefix_path(path: &str, prefix: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed == prefix {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix(&format!("{}/", prefix)) {
        return rest.to_string();
    }
    trimmed.to_string()
}

pub(crate) fn wants_stream(headers: &HeaderMap, body: &Bytes) -> bool {
    if let Some(accept) = headers.get("accept").and_then(|v| v.to_str().ok()) {
        if accept.to_ascii_lowercase().contains("text/event-stream") {
            return true;
        }
    }
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        if !ct.to_ascii_lowercase().contains("application/json") {
            return false;
        }
    } else {
        return false;
    }
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return false,
    };
    matches!(value.get("stream"), Some(serde_json::Value::Bool(true)))
}
