//! The single error type crossing the engine boundary.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The contract's closed set of error kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Auth,
    NotFound,
    RateLimited,
    Network,
    Storage,
    Invalid,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Auth => "auth",
            ErrorKind::NotFound => "not_found",
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::Network => "network",
            ErrorKind::Storage => "storage",
            ErrorKind::Invalid => "invalid",
        }
    }
}

/// `{ kind, message, reset_at }` — `reset_at` is only ever set for `rate_limited`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineError {
    pub kind: ErrorKind,
    pub message: String,
    pub reset_at: Option<i64>,
}

impl EngineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        EngineError { kind, message: message.into(), reset_at: None }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        EngineError::new(ErrorKind::Auth, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        EngineError::new(ErrorKind::NotFound, message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        EngineError::new(ErrorKind::Network, message)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        EngineError::new(ErrorKind::Storage, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        EngineError::new(ErrorKind::Invalid, message)
    }

    pub fn rate_limited(message: impl Into<String>, reset_at: Option<i64>) -> Self {
        EngineError { kind: ErrorKind::RateLimited, message: message.into(), reset_at }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for EngineError {}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        EngineError::storage(e.to_string())
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError::storage(format!("malformed stored JSON: {e}"))
    }
}

impl From<reqwest::Error> for EngineError {
    fn from(e: reqwest::Error) -> Self {
        EngineError::network(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, EngineError>;
