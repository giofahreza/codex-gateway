use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;

use crate::source::codex::response::resolve_mode;
use crate::source::{RoutedRequest, TargetModel};

pub fn convert(
    upstream_path: String,
    uri: &Uri,
    _method: &Method,
    _headers: &HeaderMap,
    body: Bytes,
) -> RoutedRequest {
    let query = uri.query().unwrap_or("").to_string();

    RoutedRequest {
        target: TargetModel::Codex,
        response_mode: resolve_mode(&upstream_path),
        upstream_path,
        upstream_query: if query.is_empty() { None } else { Some(query) },
        upstream_body: body,
    }
}
