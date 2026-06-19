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
    if upstream_path == "responses" && *method == Method::POST {
        let target = crate::source::v1::provider::target_from_request_body(&body)
            .unwrap_or(crate::source::TargetModel::Codex);
        if target != crate::source::TargetModel::Codex {
            return Ok(crate::source::v1::provider::convert(
                target,
                upstream_path,
                uri,
                body,
            ));
        }
    }

    Ok(codex::convert(upstream_path, uri, method, headers, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, Method, Uri};

    #[test]
    fn codex_responses_can_route_deepseek_models() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let routed = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"deepseek-v4-pro","input":"hi"}"#),
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::DeepSeek);
        assert_eq!(routed.upstream_path, "responses");
    }
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
