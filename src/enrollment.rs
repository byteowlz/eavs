//! One-time device enrollment. Templates are not delegated grants.
pub mod http;
use crate::keys::hash_key;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentError {
    Unauthorized,
    Invalid,
    Unavailable,
}

/// Root authorization proof, never listener authorization.
pub struct Administrator(());
impl Administrator {
    pub fn authenticate(configured: Option<&str>, supplied: &str) -> Result<Self, EnrollmentError> {
        let expected = configured
            .filter(|s| !s.is_empty())
            .ok_or(EnrollmentError::Unauthorized)?;
        let a = hash_key(expected);
        let b = hash_key(supplied);
        let diff = a.bytes().zip(b.bytes()).fold(0u8, |n, (a, b)| n | (a ^ b));
        if diff != 0 {
            return Err(EnrollmentError::Unauthorized);
        }
        Ok(Self(()))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteRequest {
    pub template_id: String,
    pub invitation_ttl_seconds: u32,
    pub device_ttl_seconds: u32,
}
/// No Debug: token only in explicit no-store response.
#[derive(Serialize)]
pub struct Invitation {
    pub token: String,
    pub expires_at: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenRequest {
    pub token: String,
}
#[derive(Serialize)]
pub struct DeviceCredential {
    pub key: String,
    pub key_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}
