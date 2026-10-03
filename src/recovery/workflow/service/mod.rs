//! Pure recovery transitions and caller-owned commit/execution contracts.
mod contract;
mod engine;
mod experience;
pub use experience::*;
mod query;
use crate::recovery::{approval, knowledge::*};
pub use contract::*;
pub use engine::*;
pub use query::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("invalid recovery input: {0}")]
    Invalid(String),
    #[error("task or target revision conflict")]
    Conflict,
    #[error("target has unfinished recovery work")]
    Busy,
    #[error("domain capacity exhausted")]
    Capacity,
    #[error(transparent)]
    Approval(#[from] approval::ApprovalError),
    #[error(transparent)]
    Knowledge(#[from] KnowledgeError),
    #[error(transparent)]
    Commit(#[from] crate::operation::CommitError),
}
fn text(value: &str, max: usize) -> Result<(), RecoveryError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        Err(RecoveryError::Invalid(
            "empty, oversized or NUL-containing value".into(),
        ))
    } else {
        Ok(())
    }
}
fn invalid(reason: &str) -> RecoveryError {
    RecoveryError::Invalid(reason.into())
}

impl From<crate::recovery::BusinessError> for RecoveryError {
    fn from(error: crate::recovery::BusinessError) -> Self {
        match error {
            crate::recovery::BusinessError::Invalid(reason) => Self::Invalid(reason),
            crate::recovery::BusinessError::Capacity => Self::Capacity,
        }
    }
}
