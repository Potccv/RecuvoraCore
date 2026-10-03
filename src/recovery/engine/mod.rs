//! Recovery orchestration with injected runtime capabilities and atomic session commits.
mod approvals;
mod runner;
mod session;
pub use runner::*;
pub use session::*;

use super::{approval::ApprovalError, knowledge::KnowledgeError, workflow::RecoveryError};
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    #[error(transparent)]
    Approval(#[from] ApprovalError),
    #[error(transparent)]
    Knowledge(#[from] KnowledgeError),
    #[error(transparent)]
    Commit(#[from] crate::operation::CommitError),
    #[error("invalid engine input: {0}")]
    Invalid(String),
    #[error("runtime capability failed: {0}")]
    Port(String),
    #[error("recovery revision conflict")]
    Conflict,
    #[error("recovery is busy")]
    Busy,
    #[error("recovery runtime stopped")]
    Stopped,
    #[error("recovery budget exhausted")]
    Capacity,
}
impl From<serde_json::Error> for EngineError {
    fn from(value: serde_json::Error) -> Self {
        Self::Invalid(value.to_string())
    }
}
pub type EngineResult<T> = Result<T, EngineError>;
