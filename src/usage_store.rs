use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct UsageHistoryEntry {
    pub recorded_at: String,
    pub provider: String,
    pub account_key: String,
    pub account_label: String,
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub request_path: String,
    pub success: bool,
    pub error: bool,
    pub request_total: u64,
    pub prompt_total: u64,
    pub prompt_error_total: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cache_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_chars: u64,
    pub prompt_items: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_usage: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
pub struct UsageHistoryQuery {
    pub limit: Option<usize>,
    pub provider: Option<String>,
    pub account_key: Option<String>,
    pub model: Option<String>,
}

pub fn append(cfg: &crate::Config, entry: &UsageHistoryEntry) -> Result<(), String> {
    let path = history_path(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let line = serde_json::to_string(entry).map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())
}

pub fn load(
    cfg: &crate::Config,
    query: &UsageHistoryQuery,
) -> Result<Vec<UsageHistoryEntry>, String> {
    let path = history_path(cfg);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };

    let provider = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let account_key = query
        .account_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let model = query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let limit = query.limit.unwrap_or(0);

    let reader = BufReader::new(file);
    let mut limited = if limit > 0 {
        Some(VecDeque::with_capacity(limit))
    } else {
        None
    };
    let mut all = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: UsageHistoryEntry = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        if let Some(provider) = provider {
            if !entry.provider.eq_ignore_ascii_case(provider) {
                continue;
            }
        }
        if let Some(account_key) = account_key {
            if entry.account_key != account_key {
                continue;
            }
        }
        if let Some(model) = model {
            if entry.model.as_deref() != Some(model) {
                continue;
            }
        }

        if let Some(entries) = limited.as_mut() {
            if entries.len() == limit {
                entries.pop_front();
            }
            entries.push_back(entry);
        } else {
            all.push(entry);
        }
    }

    if let Some(entries) = limited {
        return Ok(entries.into_iter().collect());
    }

    Ok(all)
}

fn history_path(cfg: &crate::Config) -> PathBuf {
    if let Some(dir) = cfg.auth_dir.as_ref() {
        return PathBuf::from(dir).join("gateway-usage-history.jsonl");
    }
    PathBuf::from("gateway-usage-history.jsonl")
}
