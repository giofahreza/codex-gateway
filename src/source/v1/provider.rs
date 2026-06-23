use axum::http::Uri;
use bytes::Bytes;

use crate::source::{ResponseMode, RoutedRequest, TargetModel};

pub fn target_from_request_body(body: &Bytes) -> Option<TargetModel> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let model = value.get("model").and_then(|value| value.as_str())?;
    Some(target_from_model(model))
}

pub fn target_from_model(model: &str) -> TargetModel {
    if let Some((target, _model)) = parse_provider_prefixed_model(model) {
        return target;
    }

    target_from_unprefixed_model(model)
}

fn target_from_unprefixed_model(model: &str) -> TargetModel {
    let lower = model.trim().to_ascii_lowercase();
    if lower.starts_with("qwen") {
        TargetModel::Qwen
    } else if lower.starts_with("deepseek") {
        TargetModel::DeepSeek
    } else if lower.starts_with("grok") {
        TargetModel::Grok
    } else if lower.starts_with("minimax") || lower.starts_with("abab") {
        TargetModel::MiniMax
    } else if lower.starts_with("claude") {
        TargetModel::Antigravity
    } else if lower.starts_with("gemini") {
        if is_antigravity_gemini_model(&lower) {
            TargetModel::Antigravity
        } else {
            TargetModel::Gemini
        }
    } else {
        TargetModel::Codex
    }
}

fn parse_provider_prefixed_model(model: &str) -> Option<(TargetModel, String)> {
    let (provider, model) = model.split_once(':')?;
    let target = target_from_provider_prefix(provider.trim())?;
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    Some((target, model.to_string()))
}

fn target_from_provider_prefix(provider: &str) -> Option<TargetModel> {
    match provider.to_ascii_lowercase().as_str() {
        "agw" | "antigravity" => Some(TargetModel::Antigravity),
        "gemini" | "google" => Some(TargetModel::Gemini),
        "qwen" => Some(TargetModel::Qwen),
        "deepseek" => Some(TargetModel::DeepSeek),
        "grok" | "xai" => Some(TargetModel::Grok),
        "minimax" => Some(TargetModel::MiniMax),
        "codex" | "openai" => Some(TargetModel::Codex),
        _ => None,
    }
}

pub(crate) fn strip_provider_prefix_from_body(body: Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };

    let Some(object) = value.as_object_mut() else {
        return body;
    };
    let Some(model_value) = object.get_mut("model") else {
        return body;
    };
    let Some(model) = model_value.as_str() else {
        return body;
    };
    let Some((_target, upstream_model)) = parse_provider_prefixed_model(model) else {
        return body;
    };

    *model_value = serde_json::Value::String(upstream_model);
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

pub fn convert(
    target: TargetModel,
    upstream_path: String,
    uri: &Uri,
    body: Bytes,
) -> RoutedRequest {
    let query = uri.query().unwrap_or("").to_string();
    RoutedRequest {
        target,
        upstream_path,
        upstream_query: if query.is_empty() { None } else { Some(query) },
        upstream_body: strip_provider_prefix_from_body(body),
        response_mode: ResponseMode::Passthrough,
    }
}

fn is_antigravity_gemini_model(model: &str) -> bool {
    model.ends_with("-thinking")
        || model.ends_with("-image")
        || model.ends_with("-high")
        || model.ends_with("-low")
        || model.ends_with("-agent")
        || model.starts_with("gemini-3.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_from_model_routes_to_expected_provider() {
        assert_eq!(target_from_model("gpt-5.2"), TargetModel::Codex);
        assert_eq!(target_from_model("qwen3.7-plus"), TargetModel::Qwen);
        assert_eq!(target_from_model("deepseek-v4-pro"), TargetModel::DeepSeek);
        assert_eq!(target_from_model("grok-4.3"), TargetModel::Grok);
        assert_eq!(target_from_model("MiniMax-M3"), TargetModel::MiniMax);
        assert_eq!(target_from_model("MiniMax-Text-01"), TargetModel::MiniMax);
        assert_eq!(target_from_model("abab6.5s-chat"), TargetModel::MiniMax);
        assert_eq!(target_from_model("minimax-text-01"), TargetModel::MiniMax);
        assert_eq!(
            target_from_model("claude-sonnet-4-5"),
            TargetModel::Antigravity
        );
        assert_eq!(target_from_model("gemini-2.5-pro"), TargetModel::Gemini);
        assert_eq!(
            target_from_model("agw: gemini-2.5-pro"),
            TargetModel::Antigravity
        );
        assert_eq!(
            target_from_model("antigravity: gemini-2.5-pro"),
            TargetModel::Antigravity
        );
        assert_eq!(
            target_from_model("gemini: gemini-2.5-pro"),
            TargetModel::Gemini
        );
        assert_eq!(target_from_model("openai: gpt-5.4"), TargetModel::Codex);
        assert_eq!(
            target_from_model("gemini-3-pro-image"),
            TargetModel::Antigravity
        );
        assert_eq!(
            target_from_model("gemini-2.5-flash-thinking"),
            TargetModel::Antigravity
        );
        assert_eq!(
            target_from_model("gemini-3-flash-agent"),
            TargetModel::Antigravity
        );
        assert_eq!(
            target_from_model("gemini-3.1-flash-lite"),
            TargetModel::Antigravity
        );
        assert_eq!(target_from_model("gemini-3-pro"), TargetModel::Gemini);
    }

    #[test]
    fn convert_strips_provider_prefix_from_upstream_model() {
        let uri: Uri = "/v1/responses".parse().unwrap();
        let routed = convert(
            TargetModel::Antigravity,
            "responses".to_string(),
            &uri,
            Bytes::from_static(br#"{"model":"agw: gemini-2.5-pro","input":"hi"}"#),
        );

        let body: serde_json::Value = serde_json::from_slice(&routed.upstream_body).unwrap();
        assert_eq!(body["model"], "gemini-2.5-pro");
        assert_eq!(body["input"], "hi");
    }
}
