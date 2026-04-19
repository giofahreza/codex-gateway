use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;

use crate::source::{RouteError, RoutedRequest};

pub mod codex;
pub mod response;
pub mod route;

pub fn route_to_target(
    path: &str,
    uri: &Uri,
    method: &Method,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<RoutedRequest, RouteError> {
    let upstream_path = route::resolve(path, method)?;
    Ok(codex::convert(upstream_path, uri, method, headers, body))
}
