use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;
use serde_json::{Map, Value};

use crate::source::{RouteError, RoutedRequest, TargetModel};

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
    if upstream_path == "models" && *method == Method::GET {
        return Ok(RoutedRequest {
            target: TargetModel::CodexModels,
            upstream_path,
            upstream_query: None,
            upstream_body: body,
            response_mode: crate::source::codex::response::resolve_mode("models"),
        });
    }

    if upstream_path == "responses" && *method == Method::POST {
        crate::source::v1::provider::validate_provider_prefix_in_body(&body)?;
        let target = crate::source::v1::provider::target_from_request_body(&body)
            .unwrap_or(TargetModel::Codex);
        if target != TargetModel::Codex {
            if matches!(target, TargetModel::MiniMax | TargetModel::Copilot) {
                return Ok(crate::source::v1::provider::convert(
                    target,
                    upstream_path,
                    uri,
                    body,
                ));
            }
            let body = normalize_codex_provider_body(target, body);
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

fn normalize_codex_provider_body(target: TargetModel, body: Bytes) -> Bytes {
    let Ok(Value::Object(input)) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };

    let mut output = Map::new();
    copy_json_field(&input, &mut output, "model");
    copy_json_field(&input, &mut output, "instructions");
    copy_json_field(&input, &mut output, "input");
    copy_json_field(&input, &mut output, "messages");
    if let Some(tools) = normalize_provider_tools(input.get("tools")) {
        output.insert("tools".to_string(), Value::Array(tools));
    }
    copy_json_field(&input, &mut output, "tool_choice");
    copy_json_field(&input, &mut output, "parallel_tool_calls");
    copy_json_field(&input, &mut output, "max_output_tokens");
    copy_json_field(&input, &mut output, "temperature");
    copy_json_field(&input, &mut output, "top_p");
    copy_json_field(&input, &mut output, "stop");
    copy_json_field(&input, &mut output, "stream");
    copy_json_field(&input, &mut output, "reasoning");
    copy_json_field(&input, &mut output, "reasoning_effort");
    copy_json_field(&input, &mut output, "text");
    copy_json_field(&input, &mut output, "response_format");

    if matches!(target, TargetModel::Gemini | TargetModel::Antigravity)
        && !output.contains_key("messages")
    {
        if let Some(messages) = codex_input_to_messages(input.get("input")) {
            output.insert("messages".to_string(), Value::Array(messages));
        }
    }

    serde_json::to_vec(&Value::Object(output))
        .map(Bytes::from)
        .unwrap_or(body)
}

fn copy_json_field(input: &Map<String, Value>, output: &mut Map<String, Value>, name: &str) {
    if let Some(value) = input.get(name) {
        output.insert(name.to_string(), value.clone());
    }
}

fn normalize_provider_tools(tools: Option<&Value>) -> Option<Vec<Value>> {
    let tools = tools?.as_array()?;
    let mut normalized = Vec::new();
    for tool in tools {
        if tool.get("type").and_then(|value| value.as_str()) != Some("function") {
            continue;
        }

        if let Some(function) = tool.get("function") {
            normalized.push(serde_json::json!({
                "type": "function",
                "function": function
            }));
            continue;
        }

        let Some(name) = tool.get("name").and_then(|value| value.as_str()) else {
            continue;
        };
        let mut function = Map::new();
        function.insert("name".to_string(), Value::String(name.to_string()));
        copy_json_field_object(tool, &mut function, "description");
        copy_json_field_object(tool, &mut function, "parameters");
        copy_json_field_object(tool, &mut function, "strict");
        normalized.push(serde_json::json!({
            "type": "function",
            "function": Value::Object(function)
        }));
    }

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn copy_json_field_object(input: &Value, output: &mut Map<String, Value>, name: &str) {
    if let Some(value) = input.get(name) {
        output.insert(name.to_string(), value.clone());
    }
}

fn codex_input_to_messages(input: Option<&Value>) -> Option<Vec<Value>> {
    let items = input?.as_array()?;
    let mut messages = Vec::new();
    for item in items {
        let role = item
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("user");
        if let Some(text) = extract_text(item) {
            messages.push(serde_json::json!({
                "role": role,
                "content": text
            }));
        }
    }
    if messages.is_empty() {
        None
    } else {
        Some(messages)
    }
}

fn extract_text(value: &Value) -> Option<String> {
    if let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(text) = value
        .get("content")
        .and_then(|content| content.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(text.to_string());
    }

    let parts = value
        .get("content")
        .and_then(|content| content.as_array())
        .or_else(|| value.as_array())?;
    let mut out = String::new();
    for part in parts {
        let part_type = part
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !matches!(
            part_type,
            "" | "text" | "input_text" | "output_text" | "summary_text"
        ) {
            continue;
        }
        let text = part
            .get("text")
            .and_then(|value| value.as_str())
            .or_else(|| part.get("input_text").and_then(|value| value.as_str()))
            .or_else(|| part.get("output_text").and_then(|value| value.as_str()))
            .or_else(|| part.get("content").and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(text) = text {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
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
            Bytes::from_static(
                br#"{"model":"deepseek-v4-pro","input":"hi","store":true,"include":["x"]}"#,
            ),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::DeepSeek);
        assert_eq!(routed.upstream_path, "responses");
        let body: Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "deepseek-v4-pro");
        assert_eq!(body["input"], "hi");
        assert!(body.get("store").is_none());
        assert!(body.get("include").is_none());
    }

    #[test]
    fn codex_responses_converts_gemini_input_array_to_messages() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let routed = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"gemini-2.5-pro","input":[{"role":"user","content":[{"type":"input_text","text":"hello"}]}]}"#,
            ),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::Gemini);
        let body: Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn codex_responses_uses_provider_prefix_and_strips_it_for_upstream() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let routed = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"agw:gemini-2.5-pro","input":[{"role":"user","content":[{"type":"input_text","text":"hello"}]}]}"#,
            ),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::Antigravity);
        let body: Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "gemini-2.5-pro");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn codex_responses_passes_minimax_body_through() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let routed = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"min:MiniMax-M3","input":"hello","store":true,"include":["reasoning.encrypted_content"],"tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}]}"#,
            ),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::MiniMax);
        let body: Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "MiniMax-M3");
        assert_eq!(body["input"], "hello");
        assert_eq!(body["store"], true);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["tools"][0]["name"], "shell");
    }

    #[test]
    fn codex_responses_passes_copilot_body_through() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let routed = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"cop:gpt-5.1","input":"hello","store":true,"include":["reasoning.encrypted_content"],"tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}]}"#,
            ),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::Copilot);
        let body: Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "gpt-5.1");
        assert_eq!(body["input"], "hello");
        assert_eq!(body["store"], true);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["tools"][0]["name"], "shell");
    }

    #[test]
    fn codex_responses_strips_codex_provider_prefix_for_upstream() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let routed = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"cod:gpt-5.4","input":"hello"}"#),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::Codex);
        let body: Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["input"], "hello");
    }

    #[test]
    fn codex_responses_rejects_invalid_provider_prefix_syntax() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let err = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(br#"{"model":"agw: gemini-2.5-pro","input":"hello"}"#),
        )
        .unwrap_err();

        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn codex_responses_converts_responses_tools_for_deepseek() {
        let uri: Uri = "/codex/responses".parse().unwrap();
        let routed = route_to_target(
            "/codex/responses",
            &uri,
            &Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"deepseek-v4-pro","input":"hi","tools":[{"type":"function","name":"shell","description":"run a command","parameters":{"type":"object"},"strict":true}]}"#,
            ),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::DeepSeek);
        let body: Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "shell");
        assert_eq!(body["tools"][0]["function"]["description"], "run a command");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
        assert_eq!(body["tools"][0]["function"]["strict"], true);
    }

    #[test]
    fn codex_models_routes_to_codex_catalog() {
        let uri: Uri = "/codex/models".parse().unwrap();
        let routed = route_to_target(
            "/codex/models",
            &uri,
            &Method::GET,
            &HeaderMap::new(),
            Bytes::new(),
        )
        .unwrap();

        assert_eq!(routed.target, TargetModel::CodexModels);
        assert_eq!(routed.upstream_path, "models");
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
