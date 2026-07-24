use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;
use serde_json::Value;

pub mod claude;
pub mod codex;
pub mod openapi;
pub mod v1;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TargetModel {
    Codex,
    Antigravity,
    Gemini,
    Qwen,
    DeepSeek,
    Grok,
    MiniMax,
    Copilot,
    Claude,
    Glm,
    Custom,
    CodexModels,
    UnifiedV1Models,
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

pub(crate) fn decode_stringified_responses_input(body: Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(object) = value.as_object_mut() else {
        return body;
    };
    let Some(input) = object.get("input").and_then(|value| value.as_str()) else {
        return body;
    };
    let Some(decoded) = parse_stringified_responses_input(input) else {
        return body;
    };

    object.insert("input".to_string(), decoded);
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

fn parse_stringified_responses_input(input: &str) -> Option<Value> {
    let trimmed = input.trim();
    if !trimmed.starts_with('[') {
        return None;
    }

    let decoded = serde_json::from_str::<Value>(trimmed).ok()?;
    let items = decoded.as_array()?;
    let all_items_have_response_markers = items.iter().all(has_responses_input_item_markers);
    let has_known_response_item =
        items.is_empty() || items.iter().any(looks_like_responses_input_item);
    if all_items_have_response_markers && has_known_response_item {
        Some(decoded)
    } else {
        None
    }
}

fn has_responses_input_item_markers(item: &Value) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    object
        .get("role")
        .and_then(|value| value.as_str())
        .is_some()
        || object
            .get("type")
            .and_then(|value| value.as_str())
            .is_some()
}

fn looks_like_responses_input_item(item: &Value) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    if object
        .get("role")
        .and_then(|value| value.as_str())
        .is_some()
    {
        return true;
    }

    matches!(
        object.get("type").and_then(|value| value.as_str()),
        Some(
            "message"
                | "function_call"
                | "function_call_output"
                | "tool_result"
                | "reasoning"
                | "compaction"
                | "computer_call"
                | "computer_call_output"
                | "file_search_call"
                | "web_search_call"
                | "image_generation_call"
        )
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_stringified_responses_input_array() {
        let transcript = serde_json::json!([
            {
                "role": "user",
                "content": [{ "type": "input_text", "text": "hello" }]
            }
        ]);
        let body = serde_json::json!({
            "model": "gpt-5.4",
            "input": transcript.to_string()
        })
        .to_string();

        let decoded = decode_stringified_responses_input(Bytes::from(body));
        let value: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(value["input"], transcript);
    }

    #[test]
    fn leaves_plain_prompt_strings_untouched() {
        let body = Bytes::from_static(br#"{"model":"gpt-5.4","input":"[not json"}"#);
        let decoded = decode_stringified_responses_input(body.clone());
        assert_eq!(decoded, body);
    }

    #[test]
    fn leaves_json_array_prompts_that_are_not_response_items_untouched() {
        let body = Bytes::from_static(br#"{"model":"gpt-5.4","input":"[\"literal\"]"}"#);
        let decoded = decode_stringified_responses_input(body.clone());
        assert_eq!(decoded, body);
    }

    #[test]
    fn decodes_mixed_transcript_with_unknown_typed_item() {
        let transcript = serde_json::json!([
            {
                "role": "user",
                "content": [{ "type": "input_text", "text": "hello" }]
            },
            {
                "type": "future_response_item",
                "payload": {}
            }
        ]);
        let body = serde_json::json!({
            "model": "gpt-5.4",
            "input": transcript.to_string()
        })
        .to_string();

        let decoded = decode_stringified_responses_input(Bytes::from(body));
        let value: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(value["input"], transcript);
    }

    #[test]
    fn leaves_unknown_typed_json_array_prompt_untouched() {
        let body = Bytes::from_static(br#"{"model":"gpt-5.4","input":"[{\"type\":\"todo\"}]"}"#);
        let decoded = decode_stringified_responses_input(body.clone());
        assert_eq!(decoded, body);
    }
}
