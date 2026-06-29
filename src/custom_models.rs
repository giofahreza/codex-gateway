use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CustomModel {
    pub alias: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_load_balance")]
    pub load_balance: bool,
    #[serde(default)]
    pub primary_models: Vec<CustomModelTarget>,
    #[serde(default)]
    pub fallback_models: Vec<CustomModelTarget>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CustomModelTarget {
    pub model: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

#[derive(Default, Deserialize, Serialize)]
struct CustomModelFile {
    #[serde(default)]
    models: Vec<CustomModel>,
}

fn default_enabled() -> bool {
    true
}

fn default_load_balance() -> bool {
    true
}

fn default_weight() -> u32 {
    1
}

pub(crate) fn custom_models_path(cfg: &crate::Config) -> PathBuf {
    cfg.auth_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("custom-models.json")
}

pub(crate) fn load(cfg: &crate::Config) -> Vec<CustomModel> {
    let path = custom_models_path(cfg);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<CustomModelFile>(&data) else {
        return Vec::new();
    };
    normalize_models(file.models)
}

pub(crate) fn save(cfg: &crate::Config, models: &[CustomModel]) -> Result<(), String> {
    let path = custom_models_path(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let file = CustomModelFile {
        models: normalize_models(models.to_vec()),
    };
    let data = serde_json::to_vec_pretty(&file).map_err(|err| err.to_string())?;
    std::fs::write(path, data).map_err(|err| err.to_string())
}

pub(crate) fn normalize_alias(alias: &str) -> String {
    alias
        .trim()
        .strip_prefix("ctm:")
        .unwrap_or_else(|| alias.trim())
        .trim()
        .to_string()
}

pub(crate) fn public_model_id(alias: &str) -> String {
    format!("ctm:{}", normalize_alias(alias))
}

pub(crate) fn validate_model(model: &CustomModel) -> Result<(), String> {
    validate_alias(&model.alias)?;
    if model
        .primary_models
        .iter()
        .filter(|target| target.enabled)
        .count()
        == 0
    {
        return Err("at least one enabled primary model is required".to_string());
    }
    for target in model
        .primary_models
        .iter()
        .chain(model.fallback_models.iter())
    {
        validate_target(target)?;
    }
    Ok(())
}

fn validate_alias(alias: &str) -> Result<(), String> {
    let alias = normalize_alias(alias);
    if alias.is_empty() {
        return Err("alias is required".to_string());
    }
    if alias
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, ':' | '/' | '\\'))
    {
        return Err("alias must not contain whitespace, colon, slash, or backslash".to_string());
    }
    Ok(())
}

fn validate_target(target: &CustomModelTarget) -> Result<(), String> {
    let model = target.model.trim();
    if model.is_empty() {
        return Err("target model is required".to_string());
    }
    if model.starts_with("ctm:") {
        return Err("custom models cannot target another custom model".to_string());
    }
    if model.contains(':') && !is_supported_target_prefix(model) {
        return Err(format!("unsupported target model prefix in '{}'", model));
    }
    Ok(())
}

fn is_supported_target_prefix(model: &str) -> bool {
    let Some((prefix, rest)) = model.split_once(':') else {
        return true;
    };
    !rest.trim().is_empty()
        && matches!(
            prefix.to_ascii_lowercase().as_str(),
            "agw" | "gem" | "qwn" | "dsk" | "grk" | "min" | "cop" | "cod"
        )
}

pub(crate) fn normalize_model(mut model: CustomModel) -> CustomModel {
    model.alias = normalize_alias(&model.alias);
    model.display_name = model
        .display_name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    model.primary_models = normalize_targets(model.primary_models);
    model.fallback_models = normalize_targets(model.fallback_models);
    model
}

fn normalize_models(models: Vec<CustomModel>) -> Vec<CustomModel> {
    let mut out = Vec::new();
    for model in models {
        let model = normalize_model(model);
        if validate_model(&model).is_ok() {
            out.retain(|existing: &CustomModel| !existing.alias.eq_ignore_ascii_case(&model.alias));
            out.push(model);
        }
    }
    out.sort_by(|left, right| left.alias.cmp(&right.alias));
    out
}

fn normalize_targets(targets: Vec<CustomModelTarget>) -> Vec<CustomModelTarget> {
    targets
        .into_iter()
        .map(|mut target| {
            target.model = target.model.trim().to_string();
            if target.weight == 0 {
                target.weight = 1;
            }
            target
        })
        .filter(|target| !target.model.is_empty())
        .collect()
}

pub(crate) fn parse_model_list(value: &str) -> Vec<CustomModelTarget> {
    value
        .lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(|model| CustomModelTarget {
            model: model.to_string(),
            enabled: true,
            weight: 1,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn custom_model(alias: &str, primary: Vec<&str>, fallback: Vec<&str>) -> CustomModel {
        CustomModel {
            alias: alias.to_string(),
            display_name: None,
            enabled: true,
            load_balance: true,
            primary_models: primary
                .into_iter()
                .map(|model| CustomModelTarget {
                    model: model.to_string(),
                    enabled: true,
                    weight: 1,
                })
                .collect(),
            fallback_models: fallback
                .into_iter()
                .map(|model| CustomModelTarget {
                    model: model.to_string(),
                    enabled: true,
                    weight: 1,
                })
                .collect(),
        }
    }

    fn test_config(auth_dir: String) -> Config {
        Config {
            listen: "127.0.0.1:0".to_string(),
            upstream_base: "https://example.test".to_string(),
            proxy_api_key: "test".to_string(),
            tokens: Vec::new(),
            auth_dir: Some(auth_dir),
            disabled_files: None,
            admin_auth: Default::default(),
            oauth: Default::default(),
        }
    }

    #[test]
    fn normalizes_public_alias_and_model_lists() {
        assert_eq!(normalize_alias(" ctm:my-model "), "my-model");
        assert_eq!(public_model_id("ctm:my-model"), "ctm:my-model");

        let parsed = parse_model_list("agw:gemini-2.5-pro, min:MiniMax-M3\ncop:gpt-5.1");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].model, "agw:gemini-2.5-pro");
        assert_eq!(parsed[1].model, "min:MiniMax-M3");
        assert_eq!(parsed[2].model, "cop:gpt-5.1");
    }

    #[test]
    fn rejects_invalid_custom_model_targets() {
        let no_primary = custom_model("alias", Vec::new(), Vec::new());
        assert!(validate_model(&no_primary).is_err());

        let recursive = custom_model("alias", vec!["ctm:other"], Vec::new());
        assert!(validate_model(&recursive).is_err());

        let unsupported = custom_model("alias", vec!["openai:gpt-4"], Vec::new());
        assert!(validate_model(&unsupported).is_err());
    }

    #[test]
    fn save_and_load_replace_duplicate_aliases() {
        let unique = format!("codex-gateway-custom-model-test-{}", std::process::id());
        let dir = std::env::temp_dir().join(unique);
        let cfg = test_config(dir.to_string_lossy().to_string());

        let first = custom_model("demo", vec!["agw:gemini-2.5-pro"], Vec::new());
        let second = custom_model("ctm:demo", vec!["min:MiniMax-M3"], vec!["cop:gpt-5.1"]);
        save(&cfg, &[first, second]).expect("save custom models");

        let loaded = load(&cfg);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].alias, "demo");
        assert_eq!(loaded[0].primary_models[0].model, "min:MiniMax-M3");
        assert_eq!(loaded[0].fallback_models[0].model, "cop:gpt-5.1");

        let _ = std::fs::remove_dir_all(dir);
    }
}
