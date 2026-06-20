//! Helpers for translating Codex Responses-API / OpenAI Chat-Completions
//! content into the content shape each upstream target expects.
//!
//! The Codex SDK and OpenAI chat clients send user content as an array of
//! typed parts, e.g.:
//!
//! - text: `{ "type": "input_text" | "text" | "output_text", "text": "..." }`
//! - image: `{ "type": "input_image" | "image_url" | "image",
//!             "image_url": "data:..." | { "url": "..." | "data": "..." } }`
//! - file:  `{ "type": "input_file",  "filename": "...", "file_data": "..." }`
//!           (file parts are passed through as-is when the target supports them)
//!
//! A `messages[]` array in OpenAI Chat-Completions form uses the same part
//! shapes (the difference is the part name: `text` vs `input_text`).
//!
//! This module exposes small helpers that recognize the supported part
//! shapes and extract the underlying text or image data. Each upstream
//! target then has its own thin format converter that calls these helpers
//! to produce the content shape that target's API expects (Anthropic
//! `content` blocks, Google `parts`, OpenAI `content` array, etc.).

use serde_json::{json, Value};

/// Result of classifying a single content part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartKind {
    /// Plain text part. Holds the text payload.
    Text(String),
    /// Image part. Holds the source URL or data URL string (e.g. `data:image/png;base64,...`
    /// or `https://example.com/x.png`).
    Image(String),
    /// Some other typed part we don't recognize. Passed through as-is so
    /// callers can decide whether to forward or drop it.
    Other(Value),
}

/// Classify a content part. Returns `None` for parts with no usable data.
pub fn classify_part(part: &Value) -> Option<PartKind> {
    if let Some(s) = part.as_str() {
        if s.is_empty() {
            return None;
        }
        // Bare strings in the content array are treated as text.
        return Some(PartKind::Text(s.to_string()));
    }
    let part_type = part
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Text in any of the supported shapes.
    let text = part
        .get("text")
        .or_else(|| part.get("input_text"))
        .or_else(|| part.get("output_text"));
    if let Some(text_value) = text {
        if let Some(s) = text_value.as_str() {
            if !s.is_empty() {
                return Some(PartKind::Text(s.to_string()));
            }
        }
    }
    if matches!(part_type, "text" | "input_text" | "output_text" | "summary_text") {
        // Text part with an unrecognised inner value. Skip rather than crash.
        return None;
    }

    // Image in any of the supported shapes.
    if matches!(part_type, "image_url" | "input_image" | "image") {
        if let Some(image_part) = extract_image_url(part) {
            return Some(PartKind::Image(image_part));
        }
    }
    if let Some(image_part) = extract_image_url(part) {
        return Some(PartKind::Image(image_part));
    }

    // File / other typed part — pass through.
    if !part_type.is_empty() {
        return Some(PartKind::Other(part.clone()));
    }

    None
}

/// Walk an OpenAI-style `content` field (string, array of parts, or single
/// object) and yield a classified part for each. Empty / missing content
/// yields nothing.
pub fn classify_content(value: Option<&Value>) -> Vec<PartKind> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(text) = value.as_str() {
        if !text.is_empty() {
            return vec![PartKind::Text(text.to_string())];
        }
        return Vec::new();
    }
    if let Some(arr) = value.as_array() {
        return arr.iter().filter_map(classify_part).collect();
    }
    // Single object — wrap in a one-element classification.
    classify_part(value).into_iter().collect()
}

/// Extract an image URL string from a content part, accepting either a
/// string-shaped `image_url` (`"data:..."` or `"https://..."`) or an
/// object-shaped `image_url` with `url`, `data`, or `b64_json` fields.
///
/// Returns `None` if the part has no image data.
pub fn extract_image_url(part: &Value) -> Option<String> {
    let image_url = part.get("image_url")?;

    // String form: "data:..." or "https://...".
    if let Some(s) = image_url.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
        return None;
    }
    // Object form: {url|data|b64_json, detail?}.
    if let Some(obj) = image_url.as_object() {
        if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
        if let Some(data) = obj.get("data").and_then(|v| v.as_str()) {
            if !data.is_empty() {
                return Some(data.to_string());
            }
        }
        if let Some(b64) = obj.get("b64_json").and_then(|v| v.as_str()) {
            if !b64.is_empty() {
                return Some(b64.to_string());
            }
        }
    }
    None
}

/// Split a data URL (`data:<mime>;base64,<payload>`) into its parts.
pub fn split_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime_type = meta.strip_suffix(";base64").unwrap_or(meta);
    Some((mime_type, payload))
}

/// True if the given string is a `data:` URL.
pub fn is_data_url(s: &str) -> bool {
    s.starts_with("data:") && s.contains(";base64,")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_text_part_under_input_text() {
        let p = json!({ "type": "input_text", "text": "hi" });
        assert_eq!(classify_part(&p), Some(PartKind::Text("hi".into())));
    }

    #[test]
    fn classifies_text_part_under_text_key() {
        let p = json!({ "type": "text", "text": "hi" });
        assert_eq!(classify_part(&p), Some(PartKind::Text("hi".into())));
    }

    #[test]
    fn classifies_bare_string_as_text() {
        let p = json!("hi");
        assert_eq!(classify_part(&p), Some(PartKind::Text("hi".into())));
    }

    #[test]
    fn classifies_responses_input_image_with_string_url() {
        let p = json!({
            "type": "input_image",
            "image_url": "data:image/png;base64,AAAA"
        });
        assert_eq!(
            classify_part(&p),
            Some(PartKind::Image("data:image/png;base64,AAAA".into()))
        );
    }

    #[test]
    fn classifies_openai_image_url_with_object() {
        let p = json!({
            "type": "image_url",
            "image_url": { "url": "https://example.com/x.png" }
        });
        assert_eq!(
            classify_part(&p),
            Some(PartKind::Image("https://example.com/x.png".into()))
        );
    }

    #[test]
    fn classifies_image_url_without_type_field() {
        let p = json!({ "image_url": { "url": "data:image/jpeg;base64,BBBB" } });
        assert_eq!(
            classify_part(&p),
            Some(PartKind::Image("data:image/jpeg;base64,BBBB".into()))
        );
    }

    #[test]
    fn classify_content_handles_string_and_array() {
        let s = json!("hello");
        assert_eq!(classify_content(Some(&s)), vec![PartKind::Text("hello".into())]);
        let arr = json!([
            { "type": "input_text", "text": "see " },
            { "type": "input_image", "image_url": "data:image/png;base64,ZZ" }
        ]);
        assert_eq!(
            classify_content(Some(&arr)),
            vec![
                PartKind::Text("see ".into()),
                PartKind::Image("data:image/png;base64,ZZ".into())
            ]
        );
    }

    #[test]
    fn split_data_url_returns_mime_and_payload() {
        let (m, p) = split_data_url("data:image/png;base64,ABCD").unwrap();
        assert_eq!(m, "image/png");
        assert_eq!(p, "ABCD");
    }

    #[test]
    fn is_data_url_recognises_base64() {
        assert!(is_data_url("data:image/png;base64,XYZ"));
        assert!(!is_data_url("https://example.com/x.png"));
        assert!(!is_data_url("data:text/plain,hello"));
    }
}

/// Build an OpenAI Chat-Completions-shaped `content` value from a Codex
/// Responses API / OpenAI Chat-Completions content value.
///
/// Returns:
/// - `Some(Value::String(s))` for plain-text content (preserved as a
///   string so the request body stays compact and the upstream API sees
///   the same shape it would for a text-only caller).
/// - `Some(Value::Array(parts))` for multimodal content (text + image,
///   or image-only). The array is the OpenAI chat-completions content
///   shape: `[{type: "text", text: "..."}, {type: "image_url", image_url: {url: "..."}}]`.
/// - `None` if the value carries no usable content.
///
/// This is the canonical helper for targets that speak the OpenAI
/// Chat Completions API (minimax, qwen, deepseek-with-openai-mode, ...).
pub fn openai_chat_content(value: Option<&Value>) -> Option<Value> {
    let Some(value) = value else {
        return None;
    };
    if let Some(text) = value.as_str() {
        if text.is_empty() {
            return None;
        }
        return Some(Value::String(text.to_string()));
    }
    if let Some(arr) = value.as_array() {
        let mut text_chunks: Vec<String> = Vec::new();
        let mut image_parts: Vec<Value> = Vec::new();
        for part in arr {
            if let Some(image_part) = openai_image_part(part) {
                image_parts.push(image_part);
                continue;
            }
            let text_opt = part
                .get("text")
                .and_then(|v| v.as_str())
                .or_else(|| part.get("input_text").and_then(|v| v.as_str()))
                .or_else(|| part.get("output_text").and_then(|v| v.as_str()))
                .or_else(|| {
                    if part.is_string() {
                        part.as_str()
                    } else {
                        None
                    }
                });
            if let Some(text) = text_opt {
                if !text.is_empty() {
                    text_chunks.push(text.to_string());
                }
            }
        }
        if !image_parts.is_empty() {
            let mut parts: Vec<Value> = text_chunks
                .into_iter()
                .map(|t| json!({ "type": "text", "text": t }))
                .collect();
            parts.extend(image_parts);
            return Some(Value::Array(parts));
        }
        if text_chunks.is_empty() {
            return None;
        }
        return Some(Value::String(text_chunks.join("\n")));
    }
    // Single object: maybe `{type: "image_url", ...}` or `{text: "..."}`.
    if let Some(image_part) = openai_image_part(value) {
        return Some(Value::Array(vec![image_part]));
    }
    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            return Some(Value::String(text.to_string()));
        }
    }
    None
}

fn openai_image_part(part: &Value) -> Option<Value> {
    if let Some(s) = part.as_str() {
        if looks_like_openai_image_url(s) {
            return Some(json!({
                "type": "image_url",
                "image_url": { "url": s }
            }));
        }
        return None;
    }
    let part_type = part
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let is_image_type = matches!(part_type, "image_url" | "input_image" | "image");
    if !is_image_type {
        return None;
    }
    let url = extract_image_url(part)?;
    if url.is_empty() {
        return None;
    }
    Some(json!({
        "type": "image_url",
        "image_url": { "url": url }
    }))
}

fn looks_like_openai_image_url(s: &str) -> bool {
    s.starts_with("data:") && s.contains(";base64,")
        || s.starts_with("http://")
        || s.starts_with("https://")
}

#[cfg(test)]
mod openai_chat_tests {
    use super::*;

    #[test]
    fn openai_chat_content_string_passthrough() {
        let v = json!("hello");
        assert_eq!(
            openai_chat_content(Some(&v)),
            Some(Value::String("hello".into()))
        );
    }

    #[test]
    fn openai_chat_content_text_only_array_becomes_string() {
        let v = json!([
            { "type": "input_text", "text": "hello " },
            { "type": "input_text", "text": "world" }
        ]);
        assert_eq!(
            openai_chat_content(Some(&v)),
            Some(Value::String("hello \nworld".into()))
        );
    }

    #[test]
    fn openai_chat_content_with_image_becomes_array() {
        let v = json!([
            { "type": "input_text", "text": "see" },
            { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }
        ]);
        let out = openai_chat_content(Some(&v)).unwrap();
        let arr = out.as_array().expect("must be array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[1]["type"], "image_url");
        assert_eq!(arr[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn openai_chat_content_responses_input_image_with_object_url() {
        let v = json!([
            { "type": "text", "text": "x" },
            { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } }
        ]);
        let out = openai_chat_content(Some(&v)).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr[1]["image_url"]["url"], "https://example.com/a.png");
    }
}
