use axum::http::{HeaderMap, Method};
use bytes::Bytes;

pub fn build_upstream_url(base: &str, path: &str, query: Option<&str>) -> String {
    let mut base = base.trim_end_matches('/').to_string();
    if !path.is_empty() {
        base.push('/');
        base.push_str(path.trim_start_matches('/'));
    }
    if let Some(q) = query {
        base.push('?');
        base.push_str(q);
    }
    base
}

pub fn build_request_body(
    method: &Method,
    upstream_path: &str,
    headers: &HeaderMap,
    body: Bytes,
    session_id: &str,
) -> Bytes {
    if *method == Method::POST && upstream_path == "responses" {
        let body = maybe_apply_prompt_cache_key(headers, body, session_id);
        return ensure_store_and_stream(headers, body);
    }
    body
}

pub fn apply_default_headers(
    mut req: reqwest::RequestBuilder,
    incoming: &HeaderMap,
    account_id: Option<&str>,
    session_id: &str,
) -> reqwest::RequestBuilder {
    if !incoming.contains_key("content-type") {
        req = req.header("Content-Type", "application/json");
    }
    if !incoming.contains_key("accept") {
        req = req.header("Accept", "text/event-stream");
    }
    if !incoming.contains_key("connection") {
        req = req.header("Connection", "Keep-Alive");
    }
    if !incoming.contains_key("openai-beta") {
        req = req.header("Openai-Beta", "responses=experimental");
    }
    if !incoming.contains_key("version") {
        req = req.header("Version", "0.21.0");
    }
    if !incoming.contains_key("session_id") {
        req = req.header("Session_id", session_id);
    }
    if !incoming.contains_key("conversation_id") {
        req = req.header("Conversation_id", session_id);
    }
    if !incoming.contains_key("user-agent") {
        req = req.header(
            "User-Agent",
            "codex_cli_rs/0.50.0 (Mac OS 26.0.1; arm64) Apple_Terminal/464",
        );
    }
    if !incoming.contains_key("origin") {
        req = req.header("Origin", "https://chatgpt.com");
    }
    if !incoming.contains_key("referer") {
        req = req.header("Referer", "https://chatgpt.com/");
    }
    if !incoming.contains_key("accept-language") {
        req = req.header("Accept-Language", "en-US,en;q=0.9");
    }
    if !incoming.contains_key("accept-encoding") {
        req = req.header("Accept-Encoding", "identity");
    }
    if !incoming.contains_key("originator") {
        req = req.header("Originator", "codex_cli_rs");
    }
    if !incoming.contains_key("chatgpt-account-id") {
        if let Some(id) = account_id {
            if !id.trim().is_empty() {
                req = req.header("Chatgpt-Account-Id", id);
            }
        }
    }
    req
}

fn maybe_apply_prompt_cache_key(headers: &HeaderMap, body: Bytes, session_id: &str) -> Bytes {
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
        if !map.contains_key("prompt_cache_key") {
            map.insert(
                "prompt_cache_key".to_string(),
                serde_json::Value::String(session_id.to_string()),
            );
        }
        if !map.contains_key("stream") {
            map.insert("stream".to_string(), serde_json::Value::Bool(true));
        }
        if let Ok(out) = serde_json::to_vec(&value) {
            return Bytes::from(out);
        }
    }
    body
}

fn ensure_store_and_stream(headers: &HeaderMap, body: Bytes) -> Bytes {
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
        map.insert("store".to_string(), serde_json::Value::Bool(false));
        map.insert("stream".to_string(), serde_json::Value::Bool(true));
        if let Ok(out) = serde_json::to_vec(&value) {
            return Bytes::from(out);
        }
    }
    body
}
