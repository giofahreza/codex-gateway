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

/// Lists OpenAI-compatible models exposed by the gateway.
#[utoipa::path(
    get,
    path = "/v1/models",
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "OpenAI-compatible model list",
            body = crate::source::openapi::OpenAiModelListResponse
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = crate::source::openapi::OpenAiErrorResponse
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn models_doc() {}

/// Returns one OpenAI-compatible model record by id.
#[utoipa::path(
    get,
    path = "/v1/models/{model_id}",
    params(("model_id" = String, Path, description = "Model id returned by /v1/models")),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "OpenAI-compatible model",
            body = crate::source::openapi::OpenAiModel
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = crate::source::openapi::OpenAiErrorResponse
        ),
        (
            status = 404,
            description = "Model was not found",
            body = crate::source::openapi::OpenAiErrorResponse
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn model_doc() {}

/// Creates an OpenAI-compatible response. Set `stream=true` and `Accept: text/event-stream` for SSE streaming.
#[utoipa::path(
    post,
    path = "/v1/responses",
    request_body(
        content = crate::source::openapi::V1ResponsesCreateRequest,
        content_type = "application/json",
        description = "OpenAI-compatible responses payload"
    ),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "OpenAI-compatible response object",
            body = crate::source::openapi::ResponseSummary
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = crate::source::openapi::OpenAiErrorResponse
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn responses_create_doc() {}

/// Fetches a previously created OpenAI-compatible response object by id.
#[utoipa::path(
    get,
    path = "/v1/responses/{response_id}",
    params(("response_id" = String, Path, description = "Response object id")),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "OpenAI-compatible response object",
            body = crate::source::openapi::ResponseSummary
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = crate::source::openapi::OpenAiErrorResponse
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn responses_get_doc() {}

/// Deletes a previously created OpenAI-compatible response object by id.
#[utoipa::path(
    delete,
    path = "/v1/responses/{response_id}",
    params(("response_id" = String, Path, description = "Response object id")),
    security(("bearer_auth" = [])),
    responses(
        (
            status = 200,
            description = "OpenAI-compatible response object",
            body = crate::source::openapi::ResponseSummary
        ),
        (
            status = 401,
            description = "Missing or invalid proxy API key",
            body = crate::source::openapi::OpenAiErrorResponse
        )
    )
)]
#[allow(dead_code)]
pub(crate) fn responses_delete_doc() {}
