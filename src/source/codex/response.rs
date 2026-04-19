use crate::source::ResponseMode;

pub fn resolve_mode(_upstream_path: &str) -> ResponseMode {
    ResponseMode::Passthrough
}
