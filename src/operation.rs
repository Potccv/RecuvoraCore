//! Descriptive action content; never an execution permission.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const MAX_ARTIFACT_BYTES: usize = 32 * 1024;

/// Immutable content for one `(id, version)`. The caller interprets kind and payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairArtifact {
    pub id: String,
    pub version: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub preconditions: BTreeMap<String, String>,
    pub generated_by_harness: String,
    pub generated_in_session: String,
}

impl RepairArtifact {
    /// Checks bounded neutral data, not provider support or execution authority.
    pub fn validate(&self) -> Result<(), crate::recovery::BusinessError> {
        crate::recovery::knowledge::validation::artifact(self)
    }
}
