use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;

use crate::source::{RoutedRequest, TargetModel};
use crate::source::claude::response::resolve_mode;

pub fn convert(
    upstream_path: String,
    uri: &Uri,
    method: &Method,
    headers: &HeaderMap,
    body: Bytes,
) -> RoutedRequest {
    let query = uri.query().unwrap_or("").to_string();
    let upstream_body = if upstream_path == "responses" && *method == Method::POST {
        convert_claude_bridge_body(headers, body)
    } else {
        body
    };

    // Placeholder conversion path:
    // request/claude/codex.rs is the dedicated bridge from Claude-style prefix
    // to Codex target. Keep passthrough now; add Claude payload mapping here later.
    RoutedRequest {
        target: TargetModel::Codex,
        response_mode: resolve_mode(&upstream_path),
        upstream_path,
        upstream_query: if query.is_empty() { None } else { Some(query) },
        upstream_body,
    }
}

fn convert_claude_bridge_body(headers: &HeaderMap, body: Bytes) -> Bytes {
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        if !ct.to_ascii_lowercase().contains("application/json") {
            return body;
        }
    } else {
        return body;
    }

    let mut value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return body,
    };

    if let serde_json::Value::Object(map) = &mut value {
        // Minimal bridge compatibility for Claude-style payloads.
        if let Some(serde_json::Value::String(system)) = map.get("system").cloned() {
            if !map.contains_key("instructions") {
                map.insert("instructions".to_string(), serde_json::Value::String(system));
            }
        }
        if !map.contains_key("instructions") {
            map.insert(
                "instructions".to_string(),
                serde_json::Value::String("You are a helpful coding assistant.".to_string()),
            );
        }

        if !map.contains_key("input") {
            if let Some(serde_json::Value::Array(messages)) = map.get("messages").cloned() {
                let mut input = Vec::new();
                for msg in messages {
                    let Some(role) = msg.get("role").and_then(|r| r.as_str()) else {
                        continue;
                    };
                    let content = msg.get("content");
                    let text = match content {
                        Some(serde_json::Value::String(s)) => Some(s.clone()),
                        Some(serde_json::Value::Array(parts)) => {
                            let mut joined = String::new();
                            for p in parts {
                                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                                    joined.push_str(t);
                                }
                            }
                            if joined.is_empty() { None } else { Some(joined) }
                        }
                        _ => None,
                    };
                    if let Some(text) = text {
                        input.push(serde_json::json!({
                            "role": role,
                            "content": [{"type":"input_text","text": text}]
                        }));
                    }
                }
                if !input.is_empty() {
                    map.insert("input".to_string(), serde_json::Value::Array(input));
                }
            }
        }

        // Drop known Claude-only fields not accepted by codex responses.
        map.remove("messages");
        map.remove("max_tokens");
        map.remove("anthropic_version");
        map.remove("stop_sequences");
        map.remove("system");

        if let Ok(out) = serde_json::to_vec(&value) {
            return Bytes::from(out);
        }
    }
    body
}
