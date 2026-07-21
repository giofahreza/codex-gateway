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

/// Strip provider-unsupported keys from the Codex Responses body before it
/// is sent upstream. ChatGPT-plan codex models (anything in the `gpt-5.x`
/// family, plus `codex-auto-review`) reject `max_output_tokens`, `max_tokens`,
/// and image-generation tools with a 400 Bad Request, which Codex CLI
/// silently fails to recover from. The CLI sends these params by default,
/// so we drop them here.
fn sanitize_native_codex_responses_body(body: Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(object) = value.as_object_mut() else {
        return body;
    };
    let Some(model) = object
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
    else {
        return body;
    };

    let mut changed = false;
    let mut remove_tools = false;

    if model_rejects_image_generation_tool(&model) {
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
    }

    if model_rejects_token_limit_params(&model) {
        // ChatGPT-plan codex models hard-reject several OpenAI Responses
        // parameters that the CLI sends by default. Drop them before
        // forwarding so the request reaches the upstream.
        //
        // The list was assembled from real responses proxied through
        // the gateway:
        //   - max_output_tokens / max_tokens: explicit token-limit params
        //   - prompt_cache_retention: 24h storage is not available on
        //     ChatGPT plans
        //   - background: async tasks are not exposed by ChatGPT codex
        //   - truncation: ChatGPT owns truncation
        //   - metadata / safety_identifier: only honored on API-tier
        //   - service_tier="auto": rejected; other values are accepted
        for key in [
            "max_output_tokens",
            "max_tokens",
            "prompt_cache_retention",
            "background",
            "truncation",
            "metadata",
            "safety_identifier",
        ] {
            if object.remove(key).is_some() {
                changed = true;
            }
        }
        // service_tier is accepted but only specific values like
        // "priority". The CLI default "auto" is rejected, so drop it.
        if let Some(value) = object.get("service_tier").and_then(|v| v.as_str()) {
            if value != "priority" && value != "default" {
                object.remove("service_tier");
                changed = true;
            }
        }
    }

    if changed {
        serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
    } else {
        body
    }
}

fn model_rejects_image_generation_tool(model: &str) -> bool {
    matches!(model.to_ascii_lowercase().as_str(), "gpt-5.3-codex-spark")
}

/// Models whose upstream rejects `max_output_tokens` and `max_tokens`.
///
/// ChatGPT-plan Codex slugs (`gpt-5.4`, `gpt-5.4-mini`, `gpt-5.5`,
/// `gpt-5.6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`, plus
/// `codex-auto-review`) all return `400 Unsupported parameter:
/// max_output_tokens` from `https://chatgpt.com/backend-api/codex/responses`.
/// The CLI sends both keys by default; we drop them here so the request
/// reaches the upstream successfully.
fn model_rejects_token_limit_params(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let trimmed = lower.strip_prefix("models/").unwrap_or(&lower);
    matches!(
        trimmed,
        "gpt-5.4"
            | "gpt-5.4-mini"
            | "gpt-5.5"
            | "gpt-5.6-luna"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "codex-auto-review"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_plan_models_drop_max_output_tokens_and_max_tokens() {
        for model in [
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "codex-auto-review",
        ] {
            let body = serde_json::json!({
                "model": model,
                "input": [{"role": "user", "content": "hi"}],
                "max_output_tokens": 64,
                "max_tokens": 64,
                "stream": true
            })
            .to_string();
            let sanitized = sanitize_native_codex_responses_body(Bytes::from(body));
            let v: Value = serde_json::from_slice(&sanitized).expect("valid JSON");
            assert!(
                v.get("max_output_tokens").is_none(),
                "{model}: max_output_tokens should be stripped"
            );
            assert!(
                v.get("max_tokens").is_none(),
                "{model}: max_tokens should be stripped"
            );
            // Other fields preserved.
            assert_eq!(v["model"], model);
            assert_eq!(v["stream"], true);
        }
    }

    #[test]
    fn chatgpt_plan_models_drop_other_unsupported_params() {
        // Each of these individually returns HTTP 400 from
        // chatgpt.com/backend-api/codex/responses on a ChatGPT-plan
        // account. The sanitizer must drop them all so the CLI default
        // request shape still reaches the upstream successfully.
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [{"role": "user", "content": "hi"}],
            "max_output_tokens": 64,
            "prompt_cache_retention": "24h",
            "background": true,
            "truncation": "auto",
            "metadata": {"trace_id": "x"},
            "safety_identifier": "user-test",
            "service_tier": "auto",
            "stream": true
        })
        .to_string();
        let sanitized = sanitize_native_codex_responses_body(Bytes::from(body));
        let v: Value = serde_json::from_slice(&sanitized).unwrap();
        for key in [
            "max_output_tokens",
            "prompt_cache_retention",
            "background",
            "truncation",
            "metadata",
            "safety_identifier",
            "service_tier",
        ] {
            assert!(v.get(key).is_none(), "{key} should be stripped");
        }
        assert_eq!(v["stream"], true);
        assert_eq!(v["model"], "gpt-5.6-sol");
    }

    #[test]
    fn chatgpt_plan_models_keep_priority_service_tier() {
        // "priority" (the fast lane on ChatGPT Plus/Pro) is accepted
        // upstream; the sanitizer should not touch it.
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [{"role": "user", "content": "hi"}],
            "service_tier": "priority",
            "max_output_tokens": 64
        })
        .to_string();
        let sanitized = sanitize_native_codex_responses_body(Bytes::from(body));
        let v: Value = serde_json::from_slice(&sanitized).unwrap();
        assert_eq!(v["service_tier"], "priority");
        assert!(v.get("max_output_tokens").is_none());
    }

    #[test]
    fn chatgpt_plan_models_keep_unrelated_params_and_drop_image_generation_for_spark() {
        // gpt-5.3-codex-spark rejects the image_generation tool but accepts
        // token-limit params, so only one branch should fire.
        let body = serde_json::json!({
            "model": "gpt-5.3-codex-spark",
            "input": [{"role": "user", "content": "hi"}],
            "max_output_tokens": 64,
            "tools": [
                {"type": "function", "name": "shell"},
                {"type": "image_generation"}
            ],
            "parallel_tool_calls": true,
        })
        .to_string();
        let sanitized = sanitize_native_codex_responses_body(Bytes::from(body));
        let v: Value = serde_json::from_slice(&sanitized).unwrap();
        // image_generation tool is stripped.
        let remaining_tools: Vec<Value> = v["tools"].as_array().cloned().unwrap_or_default();
        assert!(remaining_tools
            .iter()
            .all(|t| t.get("type").and_then(|x| x.as_str()) != Some("image_generation")));
        // max_output_tokens is preserved (this model is not in the
        // ChatGPT-plan strip list).
        assert_eq!(v["max_output_tokens"], 64);
        // parallel_tool_calls preserved.
        assert_eq!(v["parallel_tool_calls"], true);
    }

    #[test]
    fn non_plan_models_pass_through_unchanged() {
        let body = serde_json::json!({
            "model": "openai/gpt-4o-mini",
            "input": [{"role": "user", "content": "hi"}],
            "max_output_tokens": 64
        })
        .to_string();
        let before = body.clone();
        let sanitized = sanitize_native_codex_responses_body(Bytes::from(body));
        assert_eq!(sanitized, before.as_bytes());
    }
}
