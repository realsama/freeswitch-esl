use std::num::ParseIntError;

use serde_derive::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Ord, PartialOrd, Eq, Hash, Error, Serialize, Deserialize)]
#[allow(missing_docs)]
/// Error type for Esl
pub enum EslError {
    #[error("internal error: {0}")]
    InternalError(String),

    #[error("Wrong password.")]
    AuthFailed,

    #[error("Unable to connect to destination server.")]
    ConnectionError(String),

    #[error("ESL connection is closed.")]
    ConnectionClosed,

    #[error("ESL operation timed out after {0:?}.")]
    Timeout(std::time::Duration),

    #[error("{0:?}")]
    ApiError(String),

    #[error("")]
    CodeParseError(),

    #[error("Didnt get any digits")]
    NoInput,
}

impl From<std::io::Error> for EslError {
    fn from(error: std::io::Error) -> Self {
        Self::InternalError(error.to_string())
    }
}
impl From<tokio::sync::oneshot::error::RecvError> for EslError {
    fn from(error: tokio::sync::oneshot::error::RecvError) -> Self {
        Self::InternalError(error.to_string())
    }
}
impl From<serde_json::Error> for EslError {
    fn from(error: serde_json::Error) -> Self {
        Self::InternalError(error.to_string())
    }
}
impl From<ParseIntError> for EslError {
    fn from(error: ParseIntError) -> Self {
        Self::InternalError(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::EslError;

    #[test]
    fn internal_error_display_carries_the_detail() {
        let err = EslError::InternalError("Unable to find space index".into());
        assert!(
            err.to_string().contains("Unable to find space index"),
            "InternalError must not mask its detail; got: {err}"
        );
    }
}
