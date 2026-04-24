use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::Engine;

use crate::error::BridgeError;

#[derive(Debug, Clone)]
pub struct BasicCredentials {
    pub user: String,
    pub password: String,
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for BasicCredentials {
    type Rejection = BridgeError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .ok_or(BridgeError::MissingAuth)?
            .to_str()
            .map_err(|_| BridgeError::BadAuth("non-ascii header".into()))?;

        let b64 = header
            .strip_prefix("Basic ")
            .or_else(|| header.strip_prefix("basic "))
            .ok_or_else(|| BridgeError::BadAuth("expected Basic scheme".into()))?;

        let raw = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|_| BridgeError::BadAuth("invalid base64".into()))?;
        let decoded =
            String::from_utf8(raw).map_err(|_| BridgeError::BadAuth("invalid utf-8".into()))?;

        let (user, password) = decoded
            .split_once(':')
            .ok_or_else(|| BridgeError::BadAuth("missing colon".into()))?;

        if user.is_empty() {
            return Err(BridgeError::BadAuth("empty user".into()));
        }

        Ok(BasicCredentials {
            user: user.to_string(),
            password: password.to_string(),
        })
    }
}
