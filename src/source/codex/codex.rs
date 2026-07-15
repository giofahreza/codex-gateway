use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;
use serde_json::Value;

use crate::source::codex::response::resolve_mode;
use crate::source::{RoutedRequest, TargetModel};

pub fn convert(
    upstream_path: String,
    uri: &Uri,
    method: &Method,
    _headers: &HeaderMap,
    body: Bytes,
) -> RoutedRequest {
    let query = uri.query().unwrap_or("").to_string();
    let upstream_body = if matches!(upstream_path.as_str(), "responses" | "responses/compact")
        && *method == Method::POST
    {
        sanitize_native_codex_responses_body(
            crate::source::v1::provider::strip_provider_prefix_from_body(body),
        )
    } else {
        body
    };

    RoutedRequest {
        target: TargetModel::Codex,
        response_mode: resolve_mode(&upstream_path),
        upstream_path,
        upstream_query: if query.is_empty() { None } else { Some(query) },
        upstream_body,
    }
}

fn sanitize_native_codex_responses_body(body: Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };

    let Some(object) = value.as_object_mut() else {
        return body;
    };
    let Some(model) = object.get("model").and_then(|value| value.as_str()) else {
        return body;
    };
    if !model_rejects_image_generation_tool(model) {
        return body;
    }

    let mut changed = false;
    let mut remove_tools = false;
    if let Some(tools) = object
        .get_mut("tools")
        .and_then(|value| value.as_array_mut())
    {
        let original_len = tools.len();
        tools.retain(|tool| {
            tool.get("type").and_then(|value| value.as_str()) != Some("image_generation")
        });
        changed = tools.len() != original_len;
        remove_tools = tools.is_empty();
    }

    if remove_tools {
        object.remove("tools");
    }

    if changed {
        serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
    } else {
        body
    }
}

fn model_rejects_image_generation_tool(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "gpt-5.3-codex-spark"
    )
}
