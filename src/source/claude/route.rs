use axum::http::{Method, StatusCode};

use crate::source::{strip_prefix_path, RouteError};

pub fn resolve(path: &str, method: &Method) -> Result<String, RouteError> {
    let upstream_path = strip_prefix_path(path, "claude");
    if upstream_path.is_empty() {
        return Err(RouteError {
            status: StatusCode::NOT_FOUND,
            message: "claude endpoint not found",
        });
    }

    match upstream_path.as_str() {
        "messages" | "v1/messages" if *method == Method::POST => Ok(upstream_path),
        "responses" if *method == Method::POST => Ok(upstream_path),
        "models" | "v1/models" if *method == Method::GET || *method == Method::HEAD => {
            Ok(upstream_path)
        }
        "messages" | "v1/messages" | "responses" | "models" | "v1/models" => Err(RouteError {
            status: StatusCode::METHOD_NOT_ALLOWED,
            message: "method not allowed for claude endpoint",
        }),
        _ => Err(RouteError {
            status: StatusCode::NOT_FOUND,
            message: "claude endpoint not found",
        }),
    }
}
