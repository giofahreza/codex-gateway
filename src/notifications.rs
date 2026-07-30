use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const SETTINGS_FILE: &str = "notification-settings.json";
const MODEL_QUOTA_STATE_FILE: &str = "notification-model-quota-state.json";
const TELEGRAM_MAX_TEXT_LEN: usize = 3900;
const FULLY_USED_PERCENT: f64 = 99.9;

static MODEL_QUOTA_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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

pub(crate) fn notify_model_quota_transitions(
    state: &crate::AppState,
    provider: &str,
    provider_label: &str,
    accounts: &[serde_json::Value],
    account_options: Vec<NotificationAccountOption>,
    observed_at: &str,
) {
    let settings = state.notification_settings.lock().unwrap().clone();
    if !settings.enabled {
        return;
    }
    let snapshots = model_quota_snapshots(
        provider,
        provider_label,
        accounts,
        &account_options,
        &settings.watched_accounts,
    );
    if snapshots.is_empty() {
        return;
    }

    let path = model_quota_state_path(state.cfg.as_ref());
    let (events, changed) = {
        let _guard = MODEL_QUOTA_STATE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let mut store = load_model_quota_state(&path);
        let (events, changed) = apply_model_quota_snapshots(&mut store, snapshots, observed_at);
        if changed {
            if let Err(err) = save_model_quota_state(&path, &store) {
                tracing::error!("failed to save model quota notification state: {}", err);
            }
        }
        (events, changed)
    };

    if !changed || events.is_empty() {
        return;
    }
    let client = state.client.clone();
    for event in events {
        let settings = settings.clone();
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(err) = send_notification(&client, &settings, &event.text).await {
                tracing::error!("notification send failed: {}", err);
            }
        });
    }
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

fn model_quota_state_path(cfg: &crate::Config) -> PathBuf {
    cfg.auth_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(MODEL_QUOTA_STATE_FILE)
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

pub(crate) fn display_datetime(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    chrono::DateTime::parse_from_rfc3339(trimmed)
        .map(|datetime| {
            datetime
                .with_timezone(&chrono::Local)
                .format("%b %-d, %Y, %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| trimmed.to_string())
}

fn error_message_text(context: &crate::UsageContext, message: &str, observed_at: &str) -> String {
    let mut lines = vec![
        "IO Gateway account alert".to_string(),
        format!("Time: {}", display_datetime(observed_at)),
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

#[derive(Clone, Debug)]
struct ModelQuotaSnapshot {
    key: String,
    provider_label: String,
    account_label: String,
    account_id: String,
    model_label: String,
    window_label: String,
    used_percent: f64,
    reset_label: String,
}

#[derive(Debug)]
struct ModelQuotaEvent {
    text: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct ModelQuotaNotificationStore {
    states: HashMap<String, ModelQuotaNotificationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct ModelQuotaNotificationRecord {
    fully_used: bool,
    notified_fully_used: bool,
    provider_label: String,
    account_label: String,
    account_id: String,
    model_label: String,
    window_label: String,
    used_percent: f64,
    reset_label: String,
    observed_at: String,
}

impl Default for ModelQuotaNotificationRecord {
    fn default() -> Self {
        Self {
            fully_used: false,
            notified_fully_used: false,
            provider_label: String::new(),
            account_label: String::new(),
            account_id: String::new(),
            model_label: String::new(),
            window_label: String::new(),
            used_percent: 0.0,
            reset_label: String::new(),
            observed_at: String::new(),
        }
    }
}

fn load_model_quota_state(path: &PathBuf) -> ModelQuotaNotificationStore {
    let Ok(data) = std::fs::read_to_string(path) else {
        return ModelQuotaNotificationStore::default();
    };
    serde_json::from_str::<ModelQuotaNotificationStore>(&data).unwrap_or_default()
}

fn save_model_quota_state(
    path: &PathBuf,
    store: &ModelQuotaNotificationStore,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create model quota state dir: {}", err))?;
    }
    let data = serde_json::to_vec_pretty(store)
        .map_err(|err| format!("failed to serialize model quota state: {}", err))?;
    std::fs::write(path, data).map_err(|err| format!("failed to write model quota state: {}", err))
}

fn apply_model_quota_snapshots(
    store: &mut ModelQuotaNotificationStore,
    snapshots: Vec<ModelQuotaSnapshot>,
    observed_at: &str,
) -> (Vec<ModelQuotaEvent>, bool) {
    let mut changed = false;
    let mut events = Vec::new();

    for snapshot in snapshots {
        let fully_used = is_fully_used(snapshot.used_percent);
        let next_record = snapshot_record(&snapshot, fully_used, observed_at);
        match store.states.get_mut(&snapshot.key) {
            Some(record) => {
                if fully_used && !record.fully_used {
                    let mut record_value = next_record;
                    record_value.notified_fully_used = true;
                    *record = record_value;
                    events.push(ModelQuotaEvent {
                        text: model_quota_event_text(&snapshot, true, observed_at),
                    });
                    changed = true;
                } else if !fully_used && record.fully_used {
                    let should_notify_reset = record.notified_fully_used;
                    *record = next_record;
                    if should_notify_reset {
                        events.push(ModelQuotaEvent {
                            text: model_quota_event_text(&snapshot, false, observed_at),
                        });
                    }
                    changed = true;
                } else {
                    let notified_fully_used = record.notified_fully_used;
                    let mut record_value = next_record;
                    record_value.notified_fully_used = notified_fully_used;
                    if !records_equivalent(record, &record_value) {
                        *record = record_value;
                        changed = true;
                    }
                }
            }
            None => {
                store.states.insert(snapshot.key.clone(), next_record);
                changed = true;
            }
        }
    }

    (events, changed)
}

fn snapshot_record(
    snapshot: &ModelQuotaSnapshot,
    fully_used: bool,
    observed_at: &str,
) -> ModelQuotaNotificationRecord {
    ModelQuotaNotificationRecord {
        fully_used,
        notified_fully_used: false,
        provider_label: snapshot.provider_label.clone(),
        account_label: snapshot.account_label.clone(),
        account_id: snapshot.account_id.clone(),
        model_label: snapshot.model_label.clone(),
        window_label: snapshot.window_label.clone(),
        used_percent: snapshot.used_percent,
        reset_label: snapshot.reset_label.clone(),
        observed_at: observed_at.to_string(),
    }
}

fn records_equivalent(
    left: &ModelQuotaNotificationRecord,
    right: &ModelQuotaNotificationRecord,
) -> bool {
    left.fully_used == right.fully_used
        && left.notified_fully_used == right.notified_fully_used
        && left.provider_label == right.provider_label
        && left.account_label == right.account_label
        && left.account_id == right.account_id
        && left.model_label == right.model_label
        && left.window_label == right.window_label
        && (left.used_percent - right.used_percent).abs() < f64::EPSILON
        && left.reset_label == right.reset_label
}

fn model_quota_snapshots(
    provider: &str,
    provider_label: &str,
    accounts: &[serde_json::Value],
    account_options: &[NotificationAccountOption],
    watched_accounts: &HashSet<String>,
) -> Vec<ModelQuotaSnapshot> {
    let mut snapshots = Vec::new();
    for account in accounts {
        let Some(option) = matching_account_option(provider, account, account_options) else {
            continue;
        };
        if !option.enabled || !watched_accounts.contains(&option.key) {
            continue;
        }
        collect_account_model_quota_snapshots(
            provider,
            provider_label,
            account,
            option,
            &mut snapshots,
        );
    }
    snapshots.sort_by(|left, right| left.key.cmp(&right.key));
    snapshots
}

fn matching_account_option<'a>(
    provider: &str,
    account: &serde_json::Value,
    account_options: &'a [NotificationAccountOption],
) -> Option<&'a NotificationAccountOption> {
    let provider_options = account_options
        .iter()
        .filter(|option| option.provider == provider)
        .collect::<Vec<_>>();
    if provider_options.is_empty() {
        return None;
    }

    if let Some(file_name) = string_field(account, &["file_name"]) {
        if let Some(option) = provider_options.iter().find(|option| {
            option
                .credential_file
                .as_deref()
                .map(|value| value == file_name)
                .unwrap_or(false)
        }) {
            return Some(*option);
        }
    }

    for field in [
        "account_id",
        "email",
        "login",
        "user_id",
        "organization_uuid",
        "label",
    ] {
        let Some(value) = string_field(account, &[field]) else {
            continue;
        };
        if let Some(option) = provider_options.iter().find(|option| {
            option.account_id == value || option.label == value || option.key.ends_with(&value)
        }) {
            return Some(*option);
        }
    }

    provider_options.into_iter().next()
}

fn collect_account_model_quota_snapshots(
    provider: &str,
    provider_label: &str,
    account: &serde_json::Value,
    option: &NotificationAccountOption,
    out: &mut Vec<ModelQuotaSnapshot>,
) {
    let start_len = out.len();
    collect_named_quota_array(
        provider,
        provider_label,
        option,
        account.get("models").and_then(|value| value.as_array()),
        out,
    );
    collect_named_quota_array(
        provider,
        provider_label,
        option,
        account
            .get("additional_rate_limits")
            .and_then(|value| value.as_array()),
        out,
    );
    collect_named_quota_array(
        provider,
        provider_label,
        option,
        account.get("limits").and_then(|value| value.as_array()),
        out,
    );

    if out.len() == start_len {
        collect_named_quota_item(
            provider,
            provider_label,
            option,
            "Account quota",
            "Account quota",
            account,
            out,
        );
    }
}

fn collect_named_quota_array(
    provider: &str,
    provider_label: &str,
    option: &NotificationAccountOption,
    items: Option<&Vec<serde_json::Value>>,
    out: &mut Vec<ModelQuotaSnapshot>,
) {
    let Some(items) = items else {
        return;
    };
    for item in items {
        let model_key = string_field(
            item,
            &[
                "model_id",
                "model_name",
                "upstream_model",
                "scope",
                "label",
                "display_name",
                "name",
                "id",
            ],
        )
        .unwrap_or_else(|| "quota".to_string());
        let model_label = string_field(
            item,
            &[
                "display_name",
                "model_name",
                "label",
                "name",
                "model_id",
                "scope",
                "id",
            ],
        )
        .unwrap_or_else(|| model_key.clone());
        collect_named_quota_item(
            provider,
            provider_label,
            option,
            &model_key,
            &model_label,
            item,
            out,
        );
    }
}

fn collect_named_quota_item(
    provider: &str,
    provider_label: &str,
    option: &NotificationAccountOption,
    model_key: &str,
    model_label: &str,
    item: &serde_json::Value,
    out: &mut Vec<ModelQuotaSnapshot>,
) {
    for (field, label) in [
        ("current", "current"),
        ("current_window", "current"),
        ("five_hour", "5h"),
        ("weekly", "weekly"),
    ] {
        if let Some(bucket) = item.get(field) {
            push_model_quota_snapshot(
                provider,
                provider_label,
                option,
                model_key,
                model_label,
                label,
                bucket,
                out,
            );
        }
    }

    if quota_used_percent(item).is_some() {
        push_model_quota_snapshot(
            provider,
            provider_label,
            option,
            model_key,
            model_label,
            "quota",
            item,
            out,
        );
    }
}

fn push_model_quota_snapshot(
    provider: &str,
    provider_label: &str,
    option: &NotificationAccountOption,
    model_key: &str,
    model_label: &str,
    window_label: &str,
    bucket: &serde_json::Value,
    out: &mut Vec<ModelQuotaSnapshot>,
) {
    let Some(used_percent) = quota_used_percent(bucket) else {
        return;
    };
    let model_key = model_key.trim();
    let model_label = model_label.trim();
    if model_key.is_empty() && model_label.is_empty() {
        return;
    }
    let normalized_model_key = if model_key.is_empty() {
        model_label
    } else {
        model_key
    };
    let display_model = if model_label.is_empty() {
        normalized_model_key
    } else {
        model_label
    };
    let window_label = window_label.trim();
    let key = format!(
        "{}|{}|{}|{}",
        provider,
        option.key,
        normalize_state_key(normalized_model_key),
        normalize_state_key(window_label)
    );
    out.push(ModelQuotaSnapshot {
        key,
        provider_label: provider_label.to_string(),
        account_label: option.label.clone(),
        account_id: option.account_id.clone(),
        model_label: display_model.to_string(),
        window_label: window_label.to_string(),
        used_percent,
        reset_label: string_field(bucket, &["reset_label", "reset_at"]).unwrap_or_default(),
    });
}

fn quota_used_percent(value: &serde_json::Value) -> Option<f64> {
    let used_percent = number_field(value, &["used_percent", "usedPercent"]).or_else(|| {
        number_field(value, &["remaining_percent", "remainingPercent"])
            .map(|remaining| 100.0 - remaining)
    });
    if used_percent.is_some() {
        return used_percent.map(|value| value.clamp(0.0, 100.0));
    }

    let limit = number_field(value, &["limit", "total_count", "total", "entitlement"])?;
    if limit <= 0.0 {
        return None;
    }
    if let Some(used) = number_field(value, &["used", "usage_count", "usage"]) {
        return Some(((used.max(0.0) / limit) * 100.0).clamp(0.0, 100.0));
    }
    if let Some(remaining) = number_field(value, &["remaining", "remaining_count"]) {
        let used = (limit - remaining).max(0.0);
        return Some(((used / limit) * 100.0).clamp(0.0, 100.0));
    }
    None
}

fn string_field(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn number_field(value: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|value| value.as_f64()))
        .filter(|value| !value.is_nan())
}

fn normalize_state_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn is_fully_used(used_percent: f64) -> bool {
    used_percent >= FULLY_USED_PERCENT
}

fn model_quota_event_text(
    snapshot: &ModelQuotaSnapshot,
    fully_used: bool,
    observed_at: &str,
) -> String {
    let status = if fully_used {
        "fully used"
    } else {
        "reset / available again"
    };
    let mut lines = vec![
        "IO Gateway model quota alert".to_string(),
        format!("Time: {}", display_datetime(observed_at)),
        format!("Provider: {}", snapshot.provider_label),
        format!("Account: {}", display_snapshot_account(snapshot)),
        format!("Model: {}", snapshot.model_label),
        format!("Window: {}", snapshot.window_label),
        format!("Status: {}", status),
        format!("Usage: {:.1}%", snapshot.used_percent),
    ];
    if !snapshot.reset_label.trim().is_empty() {
        lines.push(format!("Reset: {}", snapshot.reset_label));
    }
    truncate(&lines.join("\n"), TELEGRAM_MAX_TEXT_LEN)
}

fn display_snapshot_account(snapshot: &ModelQuotaSnapshot) -> String {
    if !snapshot.account_label.trim().is_empty() && !snapshot.account_id.trim().is_empty() {
        if snapshot.account_label == snapshot.account_id {
            snapshot.account_label.clone()
        } else {
            format!("{} ({})", snapshot.account_label, snapshot.account_id)
        }
    } else if !snapshot.account_label.trim().is_empty() {
        snapshot.account_label.clone()
    } else {
        snapshot.account_id.clone()
    }
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

    #[test]
    fn model_quota_transitions_baseline_then_notify_full_and_reset() {
        let mut store = ModelQuotaNotificationStore::default();
        let observed = "2026-07-06T00:00:00Z";
        let first_full = vec![test_snapshot("gemini-pro", "5h", 100.0)];
        let (events, changed) = apply_model_quota_snapshots(&mut store, first_full, observed);
        assert!(changed);
        assert!(events.is_empty());

        let reset_without_prior_full_alert = vec![test_snapshot("gemini-pro", "5h", 20.0)];
        let (events, changed) =
            apply_model_quota_snapshots(&mut store, reset_without_prior_full_alert, observed);
        assert!(changed);
        assert!(events.is_empty());

        let full_transition = vec![test_snapshot("gemini-pro", "5h", 100.0)];
        let (events, changed) = apply_model_quota_snapshots(&mut store, full_transition, observed);
        assert!(changed);
        assert_eq!(events.len(), 1);
        assert!(events[0].text.contains("Status: fully used"));

        let duplicate_full = vec![test_snapshot("gemini-pro", "5h", 100.0)];
        let (events, changed) = apply_model_quota_snapshots(&mut store, duplicate_full, observed);
        assert!(!changed);
        assert!(events.is_empty());

        let reset_transition = vec![test_snapshot("gemini-pro", "5h", 0.0)];
        let (events, changed) = apply_model_quota_snapshots(&mut store, reset_transition, observed);
        assert!(changed);
        assert_eq!(events.len(), 1);
        assert!(events[0].text.contains("Status: reset / available again"));
    }

    #[test]
    fn model_quota_snapshots_respect_watchlist_and_extract_model_windows() {
        let accounts = vec![serde_json::json!({
            "label": "Gio",
            "email": "gio@example.com",
            "file_name": "gemini-gio.json",
            "models": [
                {
                    "model_id": "gemini-2.5-pro",
                    "display_name": "Gemini 2.5 Pro",
                    "current": {
                        "used_percent": 100.0,
                        "reset_label": "resets in 1h"
                    }
                }
            ]
        })];
        let options = vec![NotificationAccountOption {
            provider: "gemini".to_string(),
            provider_label: "Gemini".to_string(),
            key: "gemini:email:gio@example.com".to_string(),
            label: "Gio".to_string(),
            account_id: "gio@example.com".to_string(),
            credential_file: Some("gemini-gio.json".to_string()),
            enabled: true,
        }];
        let watched_accounts = HashSet::from(["gemini:email:gio@example.com".to_string()]);
        let snapshots =
            model_quota_snapshots("gemini", "Gemini", &accounts, &options, &watched_accounts);

        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].model_label, "Gemini 2.5 Pro");
        assert_eq!(snapshots[0].window_label, "current");
        assert_eq!(snapshots[0].used_percent, 100.0);
        assert_eq!(snapshots[0].reset_label, "resets in 1h");
    }

    fn test_snapshot(model: &str, window: &str, used_percent: f64) -> ModelQuotaSnapshot {
        ModelQuotaSnapshot {
            key: format!("gemini|account|{}|{}", model, window),
            provider_label: "Gemini".to_string(),
            account_label: "Gio".to_string(),
            account_id: "gio@example.com".to_string(),
            model_label: model.to_string(),
            window_label: window.to_string(),
            used_percent,
            reset_label: "resets in 1h".to_string(),
        }
    }
}
