use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;

use crate::source::v1::response::resolve_mode;
use crate::source::{RoutedRequest, TargetModel};

const DEFAULT_CLIENT_VERSION_QUERY: &str = "1.0.0";

pub fn convert(
    upstream_path: String,
    uri: &Uri,
    method: &Method,
    headers: &HeaderMap,
    body: Bytes,
) -> RoutedRequest {
    let mut query = uri.query().unwrap_or("").to_string();
    if upstream_path == "models" {
        query = normalize_models_query(&query);
    }

    let is_responses_post = upstream_path == "responses" && *method == Method::POST;
    let response_mode = resolve_mode(&upstream_path, method, headers, &body);

    let upstream_body = if is_responses_post {
        convert_openai_compat_body_to_codex(headers, body)
    } else {
        body
    };

    RoutedRequest {
        target: TargetModel::Codex,
        upstream_path,
        upstream_query: if query.is_empty() { None } else { Some(query) },
        upstream_body,
        response_mode,
    }
}

fn normalize_models_query(query: &str) -> String {
    let mut parts = Vec::new();
    let mut has_client_version = false;

    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        if part.starts_with("client_version=") {
            if !has_client_version {
                parts.push(format!("client_version={DEFAULT_CLIENT_VERSION_QUERY}"));
                has_client_version = true;
            }
            continue;
        }
        parts.push(part.to_string());
    }

    if !has_client_version {
        parts.push(format!("client_version={DEFAULT_CLIENT_VERSION_QUERY}"));
    }

    parts.join("&")
}

fn convert_openai_compat_body_to_codex(headers: &HeaderMap, body: Bytes) -> Bytes {
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
        // Codex backend requires non-empty instructions.
        let needs_default_instructions = match map.get("instructions") {
            None => true,
            Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.trim().is_empty(),
            _ => false,
        };
        if needs_default_instructions {
            map.insert(
                "instructions".to_string(),
                serde_json::Value::String("You are a helpful coding assistant.".to_string()),
            );
        }

        // OpenAI-compatible callers often send `input` as string; Codex endpoint expects a list.
        if let Some(serde_json::Value::String(text)) = map.get("input").cloned() {
            map.insert(
                "input".to_string(),
                serde_json::json!([{
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": text
                    }]
                }]),
            );
        }

        // Codex backend rejects this field.
        map.remove("max_output_tokens");

        if let Ok(out) = serde_json::to_vec(&value) {
            return Bytes::from(out);
        }
    }
    body
}
