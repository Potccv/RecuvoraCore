//! Trusted, durable approval decisions, separate from simulation authorization.
//!
//! Callers are trusted host code: this is not an authentication boundary against
//! arbitrary code in the host process. Model output is evidence, never a permit.

mod contract;
mod execution;
mod paths;
mod requests;
mod storage;
mod transitions;

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::File, path::PathBuf, sync::Arc};

const MAX_ID: usize = 512;
const MAX_REASON: usize = 8192;
const MAX_ACTION: usize = 131_072;
const MAX_RECORD: usize = 262_144;

pub use contract::{
    ApprovalAssessment, ApprovalDecision, ApprovalError, ApprovalPolicy, ApprovalRecord,
    ApprovalRequest, ApprovalState, ApprovalStoreConfig, AssessmentSource, ExecutionOutcome,
    ModelAssessment, ProposedOperation, ReviewAttempt, ReviewStage, ReviewerConfig,
    ReviewerIdentity,
};

/// A one-use, non-cloneable capability produced only after the execution intent
/// was synced. It does not implement Deserialize and its fields are private.
#[derive(Debug)]
pub struct ExecutionPermit {
    request_id: String,
    revision: u64,
    operation: ProposedOperation,
    store_identity: Arc<()>,
}

impl ExecutionPermit {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn operation(&self) -> &ProposedOperation {
        &self.operation
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalEntry {
    format: u32,
    sequence: u64,
    now: u64,
    event: Event,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Requested { request: ApprovalRequest },
    Changed { request_id: String, change: Change },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case", deny_unknown_fields)]
enum Change {
    Assess {
        assessment: ApprovalAssessment,
    },
    HumanDecision {
        assessment: ApprovalAssessment,
    },
    BeginReview {
        expected_revision: u64,
        timeout_secs: u64,
    },
    AssessAttempt {
        attempt: ReviewAttempt,
        assessment: ApprovalAssessment,
    },
    FailReview {
        attempt: ReviewAttempt,
        reason: String,
    },
    WaitingHuman {
        reason: String,
    },
    Revoke {
        reason: String,
    },
    Cancel {
        reason: String,
    },
    Expire,
    Consume,
    Complete {
        outcome: ExecutionOutcome,
        reason: String,
    },
    RecoverUnknown,
    Reconcile {
        outcome: ExecutionOutcome,
        reason: String,
        actor: String,
    },
}

/// A synchronous single-writer store. No external call is made while changing
/// state. Keep it in trusted host code and serialize its short local operations.
pub struct ApprovalStore {
    file: File,
    journal_path: PathBuf,
    _lock: File,
    lock_path: PathBuf,
    config: ApprovalStoreConfig,
    sequence: u64,
    bytes: u64,
    poisoned: bool,
    records: BTreeMap<String, ApprovalRecord>,
    identity: Arc<()>,
}

impl ApprovalStore {
    fn check_current(
        &mut self,
        id: &str,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<(), ApprovalError> {
        self.available()?;
        policy.validate()?;
        let record = self.records.get(id).ok_or(ApprovalError::NotFound)?;
        if &record.request.policy != policy {
            return Err(ApprovalError::Conflict);
        }
        if now < record.updated_at {
            return Err(ApprovalError::Invalid("clock moved backwards"));
        }
        if now >= record.request.expires_at
            && matches!(
                record.state,
                ApprovalState::Pending | ApprovalState::WaitingHuman | ApprovalState::Approved
            )
        {
            self.change(id, Change::Expire, now)?;
            return Err(ApprovalError::Expired);
        }
        if record.state == ApprovalState::Expired {
            return Err(ApprovalError::Expired);
        }
        Ok(())
    }

    fn available(&self) -> Result<(), ApprovalError> {
        if self.poisoned {
            Err(ApprovalError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn change(
        &mut self,
        id: &str,
        change: Change,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.append(
            Event::Changed {
                request_id: id.into(),
                change,
            },
            now,
        )
    }
}

fn text(value: &str, maximum: usize) -> Result<(), ApprovalError> {
    if value.trim().is_empty() || value.len() > maximum || value.contains('\0') {
        Err(ApprovalError::Invalid(
            "empty, oversized or NUL-containing text",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
use paths::validate_external_dir_for_source;

#[cfg(test)]
#[path = "../../tests/approval_paths.rs"]
mod path_tests;
