use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;

use crate::source::{RouteError, RoutedRequest};

pub mod codex;
pub mod response;
pub mod route;

pub fn route_to_target(
    path: &str,
    uri: &Uri,
    method: &Method,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<RoutedRequest, RouteError> {
    let upstream_path = route::resolve(path, method)?;
    Ok(codex::convert(upstream_path, uri, method, headers, body))
}

/// Lists models through the Claude-prefixed compatibility surface.
#[utoipa::path(
    get,
    path = "/claude/models",
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "Raw model list",
            body = crate::source::openapi::UpstreamModelListResponse
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = String
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn models_doc() {}

/// Accepts a Claude-style messages payload and bridges it to the Codex responses backend.
#[utoipa::path(
    post,
    path = "/claude/messages",
    request_body(
        content = crate::source::openapi::ClaudeMessagesCreateRequest,
        content_type = "application/json",
        description = "Claude-style messages payload"
    ),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "Bridged response from the Codex backend",
            body = crate::source::openapi::ResponseSummary
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = String
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn messages_doc() {}

/// Sends a Codex-compatible responses payload through the Claude-prefixed surface.
#[utoipa::path(
    post,
    path = "/claude/responses",
    request_body(
        content = crate::source::openapi::CodexResponsesCreateRequest,
        content_type = "application/json",
        description = "Codex responses payload"
    ),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "Upstream Codex response",
            body = crate::source::openapi::ResponseSummary
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = String
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn responses_doc() {}
