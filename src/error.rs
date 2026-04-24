use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("missing Authorization header")]
    MissingAuth,

    #[error("invalid Authorization header: {0}")]
    BadAuth(String),

    #[error("no database specified and no default_database configured")]
    NoDatabase,

    #[error("empty SQL")]
    EmptySql,

    #[error("result exceeds max_rows limit")]
    ResultTooLarge,

    #[error("query timed out after {0}s")]
    QueryTimeout(u64),

    #[error("connection pool error: {0}")]
    Pool(String),

    #[error("SQL error: {0}")]
    Sql(String),

    #[error("unsupported column type: {0}")]
    UnsupportedType(String),

    #[error("SQL Server authentication failed")]
    SqlAuthFailed,

    #[error("internal error: {0}")]
    Internal(String),
}

impl BridgeError {
    pub fn from_tiberius(e: tiberius::error::Error) -> Self {
        use tiberius::error::Error::*;
        match e {
            Server(ref token) => {
                // 18456 = login failed for user.
                if token.code() == 18456 {
                    BridgeError::SqlAuthFailed
                } else {
                    BridgeError::Sql(format!("code={} msg={}", token.code(), token.message()))
                }
            }
            other => BridgeError::Sql(other.to_string()),
        }
    }
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> Response {
        let (status, kind) = match &self {
            BridgeError::MissingAuth => (StatusCode::UNAUTHORIZED, "unauthorized"),
            BridgeError::BadAuth(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            BridgeError::SqlAuthFailed => (StatusCode::UNAUTHORIZED, "unauthorized"),
            BridgeError::NoDatabase => (StatusCode::BAD_REQUEST, "bad_request"),
            BridgeError::EmptySql => (StatusCode::BAD_REQUEST, "bad_request"),
            BridgeError::ResultTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "result_too_large"),
            BridgeError::QueryTimeout(_) => (StatusCode::GATEWAY_TIMEOUT, "timeout"),
            BridgeError::Pool(_) => (StatusCode::SERVICE_UNAVAILABLE, "pool_exhausted"),
            BridgeError::Sql(_) => (StatusCode::BAD_REQUEST, "sql_error"),
            BridgeError::UnsupportedType(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unsupported"),
            BridgeError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };

        let body = Json(json!({
            "error": kind,
            "message": self.to_string(),
        }));

        let mut resp = (status, body).into_response();
        if matches!(
            self,
            BridgeError::MissingAuth | BridgeError::BadAuth(_) | BridgeError::SqlAuthFailed
        ) {
            resp.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Basic realm=\"mssql-bridge\""),
            );
        }
        resp
    }
}
