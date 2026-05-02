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

/// Lists raw upstream Codex models without OpenAI compatibility translation.
#[utoipa::path(
    get,
    path = "/codex/models",
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "Raw Codex model list",
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

/// Sends a raw Codex responses request through the gateway.
#[utoipa::path(
    post,
    path = "/codex/responses",
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
