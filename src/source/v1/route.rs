use axum::http::{Method, StatusCode};

use crate::source::{strip_prefix_path, RouteError};

pub fn resolve(path: &str, method: &Method) -> Result<String, RouteError> {
    let upstream_path = strip_prefix_path(path, "v1");
    if upstream_path.is_empty() {
        return Err(RouteError {
            status: StatusCode::NOT_FOUND,
            message: "v1 endpoint not found",
        });
    }

    if upstream_path == "models" {
        return if *method == Method::GET || *method == Method::HEAD {
            Ok("models".to_string())
        } else {
            Err(RouteError {
                status: StatusCode::METHOD_NOT_ALLOWED,
                message: "method not allowed for v1 endpoint",
            })
        };
    }
    if upstream_path.starts_with("models/") {
        return if *method == Method::GET || *method == Method::HEAD {
            // Codex upstream does not expose OpenAI-style model retrieve;
            // fetch list and adapt downstream into /v1/models/{id}.
            Ok("models".to_string())
        } else {
            Err(RouteError {
                status: StatusCode::METHOD_NOT_ALLOWED,
                message: "method not allowed for v1 endpoint",
            })
        };
    }
    if upstream_path == "responses" {
        return if *method == Method::POST {
            Ok(upstream_path)
        } else {
            Err(RouteError {
                status: StatusCode::METHOD_NOT_ALLOWED,
                message: "method not allowed for v1 endpoint",
            })
        };
    }
    if upstream_path.starts_with("responses/") {
        return if *method == Method::GET || *method == Method::DELETE {
            Ok(upstream_path)
        } else {
            Err(RouteError {
                status: StatusCode::METHOD_NOT_ALLOWED,
                message: "method not allowed for v1 endpoint",
            })
        };
    }
    Err(RouteError {
        status: StatusCode::NOT_FOUND,
        message: "v1 endpoint not found",
    })
}
