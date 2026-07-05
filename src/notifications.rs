use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

const SETTINGS_FILE: &str = "notification-settings.json";
const TELEGRAM_MAX_TEXT_LEN: usize = 3900;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub channel: String,
    pub telegram: TelegramSettings,
    pub google_chat: GoogleChatSettings,
    pub watched_accounts: HashSet<String>,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: "telegram".to_string(),
            telegram: TelegramSettings::default(),
            google_chat: GoogleChatSettings::default(),
            watched_accounts: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramSettings {
    pub bot_token: String,
    pub chat_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoogleChatSettings {
    pub webhook_url: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationAccountOption {
    pub provider: String,
    pub provider_label: String,
    pub key: String,
    pub label: String,
    pub account_id: String,
    pub credential_file: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct NotificationSettingsUpdate {
    pub enabled: Option<bool>,
    pub channel: Option<String>,
    pub telegram: Option<TelegramSettingsUpdate>,
    pub google_chat: Option<GoogleChatSettingsUpdate>,
    pub watched_accounts: Option<Vec<String>>,
}

impl Default for NotificationSettingsUpdate {
    fn default() -> Self {
        Self {
            enabled: None,
            channel: None,
            telegram: None,
            google_chat: None,
            watched_accounts: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TelegramSettingsUpdate {
    pub bot_token: Option<String>,
    pub chat_id: Option<String>,
    pub clear_bot_token: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GoogleChatSettingsUpdate {
    pub webhook_url: Option<String>,
    pub clear_webhook_url: bool,
}

pub(crate) fn load(cfg: &crate::Config) -> NotificationSettings {
    let path = settings_path(cfg);
    let Ok(data) = std::fs::read_to_string(path) else {
        return NotificationSettings::default();
    };
    serde_json::from_str::<NotificationSettings>(&data)
        .map(normalize_settings)
        .unwrap_or_default()
}

pub(crate) fn save(cfg: &crate::Config, settings: &NotificationSettings) -> Result<(), String> {
    let settings = normalize_settings(settings.clone());
    let path = settings_path(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create notification settings dir: {}", err))?;
    }
    let data = serde_json::to_vec_pretty(&settings)
        .map_err(|err| format!("failed to serialize notification settings: {}", err))?;
    std::fs::write(&path, data)
        .map_err(|err| format!("failed to write notification settings: {}", err))
}

pub(crate) fn apply_update(
    current: &NotificationSettings,
    update: NotificationSettingsUpdate,
) -> NotificationSettings {
    let mut next = current.clone();
    if let Some(enabled) = update.enabled {
        next.enabled = enabled;
    }
    if let Some(channel) = update.channel {
        next.channel = normalize_channel(&channel);
    }
    if let Some(telegram) = update.telegram {
        if telegram.clear_bot_token {
            next.telegram.bot_token.clear();
        } else if let Some(bot_token) = telegram.bot_token {
            let bot_token = bot_token.trim();
            if !bot_token.is_empty() {
                next.telegram.bot_token = bot_token.to_string();
            }
        }
        if let Some(chat_id) = telegram.chat_id {
            next.telegram.chat_id = chat_id.trim().to_string();
        }
    }
    if let Some(google_chat) = update.google_chat {
        if google_chat.clear_webhook_url {
            next.google_chat.webhook_url.clear();
        } else if let Some(webhook_url) = google_chat.webhook_url {
            let webhook_url = webhook_url.trim();
            if !webhook_url.is_empty() {
                next.google_chat.webhook_url = webhook_url.to_string();
            }
        }
    }
    if let Some(accounts) = update.watched_accounts {
        next.watched_accounts = accounts
            .into_iter()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .collect();
    }
    normalize_settings(next)
}

pub(crate) fn public_json(
    settings: &NotificationSettings,
    accounts: Vec<NotificationAccountOption>,
) -> serde_json::Value {
    let mut watched_accounts = settings
        .watched_accounts
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    watched_accounts.sort();
    serde_json::json!({
        "enabled": settings.enabled,
        "channel": normalize_channel(&settings.channel),
        "telegram": {
            "bot_token_configured": !settings.telegram.bot_token.trim().is_empty(),
            "chat_id": settings.telegram.chat_id
        },
        "google_chat": {
            "webhook_configured": !settings.google_chat.webhook_url.trim().is_empty()
        },
        "watched_accounts": watched_accounts,
        "accounts": accounts
    })
}

pub(crate) fn notify_error(
    state: &crate::AppState,
    context: &crate::UsageContext,
    message: &str,
    observed_at: &str,
) {
    let settings = state.notification_settings.lock().unwrap().clone();
    if !settings.enabled || !settings.watched_accounts.contains(&context.key) {
        return;
    }
    let text = error_message_text(context, message, observed_at);
    let client = state.client.clone();
    tokio::spawn(async move {
        if let Err(err) = send_notification(&client, &settings, &text).await {
            tracing::error!("notification send failed: {}", err);
        }
    });
}

pub(crate) async fn send_notification(
    client: &reqwest::Client,
    settings: &NotificationSettings,
    text: &str,
) -> Result<(), String> {
    let settings = normalize_settings(settings.clone());
    if !settings.enabled {
        return Err("notifications are disabled".to_string());
    }
    match settings.channel.as_str() {
        "telegram" => send_telegram(client, &settings.telegram, text).await,
        "google_chat" => send_google_chat(client, &settings.google_chat, text).await,
        other => Err(format!("unsupported notification channel: {}", other)),
    }
}

pub(crate) fn settings_path(cfg: &crate::Config) -> PathBuf {
    cfg.auth_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(SETTINGS_FILE)
}

fn normalize_settings(mut settings: NotificationSettings) -> NotificationSettings {
    settings.channel = normalize_channel(&settings.channel);
    settings.telegram.bot_token = settings.telegram.bot_token.trim().to_string();
    settings.telegram.chat_id = settings.telegram.chat_id.trim().to_string();
    settings.google_chat.webhook_url = settings.google_chat.webhook_url.trim().to_string();
    settings
}

fn normalize_channel(value: &str) -> String {
    match value
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
        .as_str()
    {
        "google" | "googlechat" | "google_chat" | "gchat" => "google_chat".to_string(),
        _ => "telegram".to_string(),
    }
}

fn error_message_text(context: &crate::UsageContext, message: &str, observed_at: &str) -> String {
    let mut lines = vec![
        "codex-gateway account alert".to_string(),
        format!("Time: {}", observed_at),
        format!("Provider: {}", context.provider_name),
        format!("Account: {}", display_account(context)),
    ];
    if let Some(model) = context
        .model
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("Model: {}", model));
    }
    if !context.request_path.trim().is_empty() {
        lines.push(format!("Path: {}", context.request_path));
    }
    lines.push(format!("Error: {}", truncate(message, 2200)));
    truncate(&lines.join("\n"), TELEGRAM_MAX_TEXT_LEN)
}

fn display_account(context: &crate::UsageContext) -> String {
    if !context.label.trim().is_empty() && !context.account_id.trim().is_empty() {
        if context.label == context.account_id {
            context.label.clone()
        } else {
            format!("{} ({})", context.label, context.account_id)
        }
    } else if !context.label.trim().is_empty() {
        context.label.clone()
    } else if !context.account_id.trim().is_empty() {
        context.account_id.clone()
    } else {
        context.key.clone()
    }
}

async fn send_telegram(
    client: &reqwest::Client,
    telegram: &TelegramSettings,
    text: &str,
) -> Result<(), String> {
    let bot_token = telegram.bot_token.trim();
    let chat_id = telegram.chat_id.trim();
    if bot_token.is_empty() {
        return Err("telegram bot token is not configured".to_string());
    }
    if chat_id.is_empty() {
        return Err("telegram chat id is not configured".to_string());
    }
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
    let response = client
        .post(url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true
        }))
        .send()
        .await
        .map_err(|err| format!("telegram request failed: {}", err))?;
    ensure_response_success("telegram", response).await
}

async fn send_google_chat(
    client: &reqwest::Client,
    google_chat: &GoogleChatSettings,
    text: &str,
) -> Result<(), String> {
    let webhook_url = google_chat.webhook_url.trim();
    if webhook_url.is_empty() {
        return Err("Google Chat webhook URL is not configured".to_string());
    }
    let response = client
        .post(webhook_url)
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .map_err(|err| format!("Google Chat request failed: {}", err))?;
    ensure_response_success("Google Chat", response).await
}

async fn ensure_response_success(channel: &str, response: reqwest::Response) -> Result<(), String> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| format!("notification response body read failed: {}", err))?;
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("{} returned {}: {}", channel, status, text))
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let suffix = "...";
    let take = max_chars.saturating_sub(suffix.len());
    let mut out = value.chars().take(take).collect::<String>();
    out.push_str(suffix);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_update_keeps_existing_secrets_when_blank() {
        let current = NotificationSettings {
            enabled: true,
            channel: "telegram".to_string(),
            telegram: TelegramSettings {
                bot_token: "old-token".to_string(),
                chat_id: "1".to_string(),
            },
            google_chat: GoogleChatSettings {
                webhook_url: "https://chat.example/webhook".to_string(),
            },
            watched_accounts: HashSet::from(["codex:file:a.json".to_string()]),
        };
        let next = apply_update(
            &current,
            NotificationSettingsUpdate {
                telegram: Some(TelegramSettingsUpdate {
                    bot_token: Some("".to_string()),
                    chat_id: Some("2".to_string()),
                    clear_bot_token: false,
                }),
                ..Default::default()
            },
        );
        assert_eq!(next.telegram.bot_token, "old-token");
        assert_eq!(next.telegram.chat_id, "2");
    }

    #[test]
    fn channel_aliases_are_normalized() {
        assert_eq!(normalize_channel("Google Chat"), "google_chat");
        assert_eq!(normalize_channel("telegram"), "telegram");
    }
}
