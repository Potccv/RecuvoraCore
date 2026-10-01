//! Durable incident-to-repair orchestration with reviewed reusable scripts.
mod contract;
mod engine;
mod incident_gate;
mod migration;
mod ownership;
mod query;
mod storage;
mod storage_paths;

#[cfg(test)]
#[path = "../../../../tests/recovery_storage.rs"]
mod recovery_storage_tests;

#[cfg(test)]
#[path = "../../../../tests/recovery_migration.rs"]
mod recovery_migration_tests;

pub use contract::*;
pub use engine::RecoveryService;
pub use incident_gate::{IncidentGuard, IncidentReadiness};
pub use migration::*;
pub use ownership::{CanonicalTarget, FileTargetOwnership, TargetLease, TargetOwnership};
pub use query::*;

use crate::operation::{CallScope, Cancellation};
use crate::recovery::approval;
use crate::recovery::knowledge::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("invalid recovery input: {0}")]
    Invalid(String),
    #[error("recovery service: {0}")]
    Service(String),
    #[error("recovery journal is corrupt: {0}")]
    Corrupt(String),
    #[error("target or task already has unfinished recovery work")]
    Busy,
    #[error("recovery service is stopped")]
    Stopped,
    #[error("recovery capacity exhausted")]
    Capacity,
    #[error(transparent)]
    Approval(#[from] approval::ApprovalError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

fn service(error: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Service(error.to_string())
}
fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, RecoveryError> {
    value.lock().map_err(|_| service("poisoned recovery lock"))
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

/// A trusted clock; deadlines are stored as Unix milliseconds, approval uses seconds.
pub trait RecoveryClock: Send + Sync {
    fn now_ms(&self) -> u64;
}
pub struct SystemRecoveryClock;
impl RecoveryClock for SystemRecoveryClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
}
