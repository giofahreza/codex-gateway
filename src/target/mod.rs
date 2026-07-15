pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod deepseek;
pub mod gemini;
pub mod glm;
pub mod grok;
pub mod minimax;
pub mod oauth;
pub mod qwen;

pub(crate) fn atomic_write_json(
    path: &std::path::Path,
    value: &serde_json::Value,
) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(value).map_err(|err| err.to_string())?;
    atomic_write(path, &data, true)
}

pub(crate) fn atomic_write(
    path: &std::path::Path,
    data: &[u8],
    private: bool,
) -> Result<(), String> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("credential");
    let tmp = path.with_file_name(format!(".{}.{}.tmp", file_name, uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|err| err.to_string())?;
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|err| err.to_string())?;
        }
        let _ = private;
        file.write_all(data).map_err(|err| err.to_string())?;
        file.sync_all().map_err(|err| err.to_string())?;
        std::fs::rename(&tmp, path).map_err(|err| err.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
