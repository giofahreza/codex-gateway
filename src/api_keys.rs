use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use uuid::Uuid;

const API_KEYS_FILE: &str = "api-keys.json";
const LEGACY_PROXY_API_KEY_LABEL: &str = "Legacy proxy_api_key";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ApiKeyStore {
    pub keys: Vec<ApiKeyRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ApiKeyRecord {
    pub id: String,
    pub label: String,
    pub key_prefix: String,
    pub lookup_hash: String,
    pub hash: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    pub source: ApiKeySource,
}

impl Default for ApiKeyRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            key_prefix: String::new(),
            lookup_hash: String::new(),
            hash: String::new(),
            created_at: String::new(),
            last_used_at: None,
            revoked_at: None,
            source: ApiKeySource::Managed,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApiKeySource {
    #[default]
    Managed,
    LegacyConfig,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PublicApiKeyRecord {
    pub id: String,
    pub label: String,
    pub key_prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    pub source: ApiKeySource,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CreatedApiKey {
    pub key: PublicApiKeyRecord,
    pub plain_text_key: String,
}

pub(crate) fn api_keys_path(cfg: &crate::Config) -> PathBuf {
    cfg.auth_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(API_KEYS_FILE)
}

pub(crate) fn load(cfg: &crate::Config) -> ApiKeyStore {
    let path = api_keys_path(cfg);
    let Ok(data) = std::fs::read_to_string(path) else {
        return ApiKeyStore::default();
    };
    serde_json::from_str::<ApiKeyStore>(&data).unwrap_or_default()
}

pub(crate) fn save(cfg: &crate::Config, store: &ApiKeyStore) -> Result<(), String> {
    let path = api_keys_path(cfg);
    let data = serde_json::to_vec_pretty(store)
        .map_err(|err| format!("failed to serialize API keys: {}", err))?;
    crate::target::atomic_write(&path, &data, true)
        .map_err(|err| format!("failed to save API keys: {}", err))
}

pub(crate) fn bootstrap_legacy_key(
    store: &mut ApiKeyStore,
    raw_key: &str,
    now: &str,
) -> Result<bool, String> {
    let raw_key = raw_key.trim();
    if raw_key.is_empty() || find_matching_index(store, raw_key, true).is_some() {
        return Ok(false);
    }
    store.keys.push(ApiKeyRecord {
        id: Uuid::new_v4().simple().to_string(),
        label: LEGACY_PROXY_API_KEY_LABEL.to_string(),
        key_prefix: key_prefix(raw_key),
        lookup_hash: lookup_hash(raw_key),
        hash: hash_api_key(raw_key)?,
        created_at: now.to_string(),
        last_used_at: None,
        revoked_at: None,
        source: ApiKeySource::LegacyConfig,
    });
    Ok(true)
}

pub(crate) fn verify_token(store: &ApiKeyStore, raw_key: &str) -> Option<String> {
    find_matching_index(store, raw_key, false).map(|index| store.keys[index].id.clone())
}

pub(crate) fn token_lookup_hash(raw_key: &str) -> String {
    lookup_hash(raw_key.trim())
}

pub(crate) fn verification_candidates(store: &ApiKeyStore, raw_key: &str) -> Vec<ApiKeyRecord> {
    let raw_key = raw_key.trim();
    if raw_key.is_empty() {
        return Vec::new();
    }
    let candidate_lookup_hash = lookup_hash(raw_key);
    store
        .keys
        .iter()
        .filter(|record| {
            record.revoked_at.is_none()
                && (record.lookup_hash.trim().is_empty()
                    || record.lookup_hash == candidate_lookup_hash)
        })
        .cloned()
        .collect()
}

pub(crate) fn verify_record(record: &ApiKeyRecord, raw_key: &str) -> bool {
    record.revoked_at.is_none() && verify_hash(record, raw_key.trim())
}

pub(crate) fn touch_last_used(store: &mut ApiKeyStore, id: &str, now: &str) -> bool {
    let Some(record) = store.keys.iter_mut().find(|record| record.id == id) else {
        return false;
    };
    if record.revoked_at.is_some() || same_minute(record.last_used_at.as_deref(), Some(now)) {
        return false;
    }
    record.last_used_at = Some(now.to_string());
    true
}

pub(crate) fn create_key(
    store: &mut ApiKeyStore,
    label: &str,
    now: &str,
) -> Result<CreatedApiKey, String> {
    let normalized_label = normalize_label(label);
    let plain_text_key = generate_api_key();
    let record = ApiKeyRecord {
        id: Uuid::new_v4().simple().to_string(),
        label: normalized_label,
        key_prefix: key_prefix(&plain_text_key),
        lookup_hash: lookup_hash(&plain_text_key),
        hash: hash_api_key(&plain_text_key)?,
        created_at: now.to_string(),
        last_used_at: None,
        revoked_at: None,
        source: ApiKeySource::Managed,
    };
    let public = public_record(&record);
    store.keys.push(record);
    Ok(CreatedApiKey {
        key: public,
        plain_text_key,
    })
}

pub(crate) fn revoke_key(store: &mut ApiKeyStore, id: &str, now: &str) -> Result<bool, String> {
    let Some(record) = store.keys.iter_mut().find(|record| record.id == id) else {
        return Err("API key not found".to_string());
    };
    if record.revoked_at.is_some() {
        return Ok(false);
    }
    record.revoked_at = Some(now.to_string());
    Ok(true)
}

pub(crate) fn public_records(store: &ApiKeyStore) -> Vec<PublicApiKeyRecord> {
    let mut out = store
        .keys
        .iter()
        .map(public_record)
        .collect::<Vec<PublicApiKeyRecord>>();
    out.sort_by(
        |left, right| match (left.revoked_at.is_some(), right.revoked_at.is_some()) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => right.created_at.cmp(&left.created_at),
        },
    );
    out
}

fn public_record(record: &ApiKeyRecord) -> PublicApiKeyRecord {
    PublicApiKeyRecord {
        id: record.id.clone(),
        label: record.label.clone(),
        key_prefix: record.key_prefix.clone(),
        created_at: record.created_at.clone(),
        last_used_at: record.last_used_at.clone(),
        revoked_at: record.revoked_at.clone(),
        source: record.source,
    }
}

fn normalize_label(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        "API key".to_string()
    } else {
        label.to_string()
    }
}

fn key_prefix(raw_key: &str) -> String {
    let visible = raw_key.chars().take(12).collect::<String>();
    if raw_key.chars().count() > 12 {
        format!("{}...", visible)
    } else {
        visible
    }
}

fn lookup_hash(raw_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    hex_encode(&hasher.finalize())
}

fn hash_api_key(raw_key: &str) -> Result<String, String> {
    let salt = SaltString::encode_b64(&Uuid::new_v4().into_bytes())
        .map_err(|err| format!("failed to encode API key salt: {}", err))?;
    Argon2::default()
        .hash_password(raw_key.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| format!("failed to hash API key: {}", err))
}

fn verify_hash(record: &ApiKeyRecord, raw_key: &str) -> bool {
    let Ok(parsed_hash) = PasswordHash::new(&record.hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(raw_key.as_bytes(), &parsed_hash)
        .is_ok()
}

fn find_matching_index(store: &ApiKeyStore, raw_key: &str, include_revoked: bool) -> Option<usize> {
    let raw_key = raw_key.trim();
    if raw_key.is_empty() {
        return None;
    }
    let candidate_lookup_hash = lookup_hash(raw_key);
    store.keys.iter().position(|record| {
        if !include_revoked && record.revoked_at.is_some() {
            return false;
        }
        if !record.lookup_hash.trim().is_empty() && record.lookup_hash != candidate_lookup_hash {
            return false;
        }
        verify_hash(record, raw_key)
    })
}

fn same_minute(left: Option<&str>, right: Option<&str>) -> bool {
    minute_bucket(left) == minute_bucket(right)
}

fn minute_bucket(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| value.get(..16))
}

fn generate_api_key() -> String {
    format!(
        "cgw_{}_{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> crate::Config {
        crate::Config {
            listen: "127.0.0.1:0".to_string(),
            upstream_base: "https://example.test".to_string(),
            proxy_api_key: "legacy-test-key".to_string(),
            tokens: Vec::new(),
            auth_dir: None,
            disabled_files: None,
            admin_auth: crate::admin_auth::AdminAuthConfig::default(),
            oauth: crate::target::oauth::OAuthConfig::default(),
            max_request_body_bytes: crate::default_max_request_body_bytes(),
            max_concurrent_requests: crate::default_max_concurrent_requests(),
            trusted_proxy: false,
            history_retention_days: crate::default_history_retention_days(),
            history_max_entries: crate::default_history_max_entries(),
            upstream_connect_timeout_seconds: crate::default_upstream_connect_timeout_seconds(),
            upstream_read_timeout_seconds: crate::default_upstream_read_timeout_seconds(),
            upstream_first_event_timeout_seconds:
                crate::default_upstream_first_event_timeout_seconds(),
        }
    }

    #[test]
    fn create_verify_and_revoke_api_key() {
        let mut store = ApiKeyStore::default();
        let created = create_key(&mut store, "Primary", "2026-07-11T00:00:00Z").unwrap();
        let key_id = verify_token(&store, &created.plain_text_key);
        assert_eq!(key_id.as_deref(), Some(created.key.id.as_str()));

        assert!(touch_last_used(
            &mut store,
            &created.key.id,
            "2026-07-11T00:01:00Z"
        ));
        assert!(store.keys[0].last_used_at.is_some());

        assert!(revoke_key(&mut store, &created.key.id, "2026-07-11T00:02:00Z").unwrap());
        assert!(verify_token(&store, &created.plain_text_key).is_none());
    }

    #[test]
    fn bootstrap_legacy_key_does_not_restore_revoked_key() {
        let mut store = ApiKeyStore::default();
        assert!(bootstrap_legacy_key(&mut store, "legacy-secret", "2026-07-11T00:00:00Z").unwrap());
        let key_id = store.keys[0].id.clone();
        assert!(revoke_key(&mut store, &key_id, "2026-07-11T00:05:00Z").unwrap());
        assert!(
            !bootstrap_legacy_key(&mut store, "legacy-secret", "2026-07-11T00:06:00Z").unwrap()
        );
        assert_eq!(store.keys.len(), 1);
    }

    #[test]
    fn saves_and_loads_store() {
        let dir = tempfile_dir();
        let mut cfg = test_config();
        cfg.auth_dir = Some(dir.to_string_lossy().to_string());

        let mut store = ApiKeyStore::default();
        let created = create_key(&mut store, "Persisted", "2026-07-11T00:00:00Z").unwrap();
        save(&cfg, &store).unwrap();

        let loaded = load(&cfg);
        assert_eq!(loaded.keys.len(), 1);
        assert_eq!(
            verify_token(&loaded, &created.plain_text_key).as_deref(),
            Some(loaded.keys[0].id.as_str())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("codex-gateway-api-keys-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
