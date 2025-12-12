//! Provider translation layer.
//!
//! This module handles translation between different LLM provider formats.
//! All providers translate to/from a canonical format defined in `types.rs`.

#![allow(dead_code)]

pub mod anthropic;
pub mod google;
pub mod messages;
pub mod openai;

use crate::types::{Context, StreamEvent, StreamState};

/// Trait for transforming requests to a provider's format.
pub trait RequestTransformer {
    /// Transform canonical context to provider-specific request body.
    fn transform_request(&self, context: &Context) -> Result<serde_json::Value, TransformError>;

    /// Get required headers for this provider.
    fn headers(&self, api_key: &str) -> Vec<(String, String)>;

    /// Get the endpoint path for this provider.
    fn endpoint_path(&self, context: &Context) -> String;
}

/// Trait for parsing provider responses into canonical events.
pub trait ResponseTransformer {
    /// Parse a provider-specific SSE chunk into canonical stream events.
    fn parse_stream_chunk(
        &self,
        chunk: &str,
        state: &mut StreamState,
    ) -> Result<Vec<StreamEvent>, TransformError>;

    /// Parse a complete (non-streaming) response.
    fn parse_response(&self, body: &serde_json::Value) -> Result<Vec<StreamEvent>, TransformError>;
}

/// Errors during transformation.
#[derive(Debug, Clone)]
pub enum TransformError {
    /// Invalid JSON structure
    InvalidJson(String),
    /// Missing required field
    MissingField(String),
    /// Invalid field value
    InvalidValue(String),
    /// Unsupported feature
    Unsupported(String),
}

impl std::fmt::Display for TransformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(msg) => write!(f, "Invalid JSON: {}", msg),
            Self::MissingField(field) => write!(f, "Missing required field: {}", field),
            Self::InvalidValue(msg) => write!(f, "Invalid value: {}", msg),
            Self::Unsupported(msg) => write!(f, "Unsupported: {}", msg),
        }
    }
}

impl std::error::Error for TransformError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transform_error_display() {
        let err = TransformError::MissingField("model".to_string());
        assert_eq!(format!("{}", err), "Missing required field: model");

        let err = TransformError::InvalidJson("unexpected token".to_string());
        assert_eq!(format!("{}", err), "Invalid JSON: unexpected token");
    }
}
