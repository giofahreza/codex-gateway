use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;

use crate::source::{RouteError, RoutedRequest};

pub mod codex;
pub mod multimodal;
pub mod provider;
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
    if (*method == Method::GET || *method == Method::HEAD)
        && (upstream_path == "models" || upstream_path.starts_with("models/"))
    {
        return Ok(provider::convert(
            crate::source::TargetModel::UnifiedV1Models,
            upstream_path,
            uri,
            body,
        ));
    }

    if matches!(upstream_path.as_str(), "responses" | "responses/compact")
        && *method == Method::POST
    {
        provider::validate_provider_prefix_in_body(&body)?;
        let target =
            provider::target_from_request_body(&body).unwrap_or(crate::source::TargetModel::Codex);
        if target != crate::source::TargetModel::Codex {
            return Ok(provider::convert(target, upstream_path, uri, body));
        }
    }

    if upstream_path == "chat/completions" && *method == Method::POST {
        provider::validate_provider_prefix_in_body(&body)?;
        let target =
            provider::target_from_request_body(&body).unwrap_or(crate::source::TargetModel::Codex);
        if target != crate::source::TargetModel::Codex {
            return Ok(provider::convert(target, upstream_path, uri, body));
        }
    }

    if (upstream_path == "images/generations" || upstream_path == "videos/generations")
        && *method == Method::POST
    {
        provider::validate_provider_prefix_in_body(&body)?;
        let target =
            provider::target_from_request_body(&body).unwrap_or(crate::source::TargetModel::Codex);
        if target != crate::source::TargetModel::Codex {
            return Ok(provider::convert(target, upstream_path, uri, body));
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_to_target_uses_unified_catalog_for_v1_models() {
        let uri: Uri = "/v1/models".parse().unwrap();
        let routed = route_to_target(
            "/v1/models",
            &uri,
            &Method::GET,
            &HeaderMap::new(),
            Bytes::new(),
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::UnifiedV1Models);
        assert_eq!(routed.upstream_path, "models");
    }

    #[test]
    fn route_to_target_preserves_v1_model_retrieve_id() {
        let uri: Uri = "/v1/models/grok-4.3".parse().unwrap();
        let routed = route_to_target(
            "/v1/models/grok-4.3",
            &uri,
            &Method::GET,
            &HeaderMap::new(),
            Bytes::new(),
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::UnifiedV1Models);
        assert_eq!(routed.upstream_path, "models/grok-4.3");
    }

    #[test]
    fn route_to_target_uses_qwen_for_qwen_models() {
        let uri: Uri = "/v1/responses".parse().unwrap();
        let body = Bytes::from_static(br#"{"model":"qwen3.7-plus","input":"hi"}"#);
        let routed = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            body,
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::Qwen);
        assert_eq!(routed.upstream_path, "responses");
    }

    #[test]
    fn route_to_target_uses_glm_for_openai_chat_completions() {
        let uri: Uri = "/v1/chat/completions".parse().unwrap();
        let body = Bytes::from_static(
            br#"{"model":"glm:glm-5.2","messages":[{"role":"user","content":"hi"}]}"#,
        );
        let routed = route_to_target(
            "/v1/chat/completions",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            body,
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::Glm);
        assert_eq!(routed.upstream_path, "chat/completions");
        let body: serde_json::Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "glm-5.2");
    }

    #[test]
    fn route_to_target_uses_native_gemini_for_standard_gemini_models() {
        let uri: Uri = "/v1/responses".parse().unwrap();
        let body = Bytes::from_static(br#"{"model":"gemini-2.5-pro","input":"hi"}"#);
        let routed = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            body,
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::Gemini);
    }

    #[test]
    fn route_to_target_uses_provider_prefix_and_strips_it_for_upstream() {
        let uri: Uri = "/v1/responses".parse().unwrap();
        let routed = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"agw:gemini-2.5-pro","input":"hi"}"#),
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::Antigravity);
        let body: serde_json::Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "gemini-2.5-pro");
        assert_eq!(body["input"], "hi");

        let direct = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gem:gemini-2.5-pro","input":"hi"}"#),
        )
        .unwrap();

        assert_eq!(direct.target, crate::source::TargetModel::Gemini);
        let body: serde_json::Value = serde_json::from_slice(&direct.upstream_body).unwrap();
        assert_eq!(body["model"], "gemini-2.5-pro");

        let codex = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"cod:gpt-5.4","input":"hi"}"#),
        )
        .unwrap();

        assert_eq!(codex.target, crate::source::TargetModel::Codex);
        let body: serde_json::Value = serde_json::from_slice(&codex.upstream_body).unwrap();
        assert_eq!(body["model"], "gpt-5.4");
    }

    #[test]
    fn route_to_target_uses_custom_model_prefix_and_strips_it_for_lookup() {
        let uri: Uri = "/v1/responses".parse().unwrap();
        let routed = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"ctm:workhorse","input":"hi"}"#),
        )
        .unwrap();

        assert_eq!(routed.target, crate::source::TargetModel::Custom);
        let body: serde_json::Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "workhorse");
        assert_eq!(body["input"], "hi");
    }

    #[test]
    fn route_to_target_rejects_invalid_provider_prefix_syntax() {
        let uri: Uri = "/v1/responses".parse().unwrap();
        let spaced = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"agw: gemini-2.5-pro","input":"hi"}"#),
        )
        .unwrap_err();
        assert_eq!(spaced.status, axum::http::StatusCode::BAD_REQUEST);

        let long = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gemini:gemini-2.5-pro","input":"hi"}"#),
        )
        .unwrap_err();
        assert_eq!(long.status, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn route_to_target_uses_claude_for_claude_and_antigravity_for_special_gemini_models() {
        let uri: Uri = "/v1/responses".parse().unwrap();

        let claude = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"claude-sonnet-4-5","input":"hi"}"#),
        )
        .unwrap();
        assert_eq!(claude.target, crate::source::TargetModel::Claude);

        let agw_gemini = route_to_target(
            "/v1/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gemini-3-pro-image","input":"hi"}"#),
        )
        .unwrap();
        assert_eq!(agw_gemini.target, crate::source::TargetModel::Antigravity);
    }
}
