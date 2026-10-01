//! Public proposals, exact retrieval predicates and trusted verification attestations.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub const MAX_SCRIPT_BYTES: usize = 32 * 1024;
pub const MAX_EXTERNAL_KNOWLEDGE_BYTES: usize = 4 * 1024 * 1024;

/// Read-only evidence from one Host-selected source. There is no successful
/// outcome, attestation or execution authority in this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeProposal {
    pub source_id: String,
    pub candidate: KnowledgeCandidate,
    pub evidence_refs: Vec<String>,
}

/// Immutable content for one `(id, version)`. No interpreter is run by this module.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptArtifact {
    pub id: String,
    pub version: u64,
    pub language: String,
    pub platform: String,
    pub source: String,
    pub preconditions: BTreeMap<String, String>,
    pub generated_by_harness: String,
    pub generated_in_session: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCandidate {
    pub id: String,
    pub incident_id: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub conditions: BTreeMap<String, String>,
    pub script: ScriptArtifact,
    /// Trusted host decision to make this case eligible for script reuse search.
    /// It is not execution authorization; missing legacy values default to false.
    #[serde(default)]
    pub reusable: bool,
    pub evidence_refs: Vec<String>,
    pub created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeStatus {
    Candidate,
    Verified,
    Failed,
    Unknown,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Verified,
    Failed,
    Unknown,
}

/// A durable operation result supplied by trusted orchestration, not model output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairCase {
    pub id: String,
    pub operation_id: String,
    pub target_id: String,
    pub script_id: String,
    pub script_version: u64,
    pub outcome: RepairOutcome,
    pub evidence_refs: Vec<String>,
    pub recorded_at_ms: u64,
}

/// Read-only persisted provenance. Deserializing this does not create a trusted token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessVerificationRecord {
    pub operation_id: String,
    pub target_id: String,
    pub script_id: String,
    pub script_version: u64,
    pub verifier_id: String,
    pub evidence_refs: Vec<String>,
    pub verified_at_ms: u64,
}

/// An explicit assertion by the trusted embedding application after independent
/// business checks. This type deliberately has no `Deserialize` implementation.
/// It is not authentication or a proof of evidence truth; the application must bind
/// its verifier identity, persist the verification evidence and restrict this API.
#[derive(Clone, Debug, Serialize)]
pub struct TrustedBusinessVerification(pub(super) BusinessVerificationRecord);

impl TrustedBusinessVerification {
    #[allow(clippy::too_many_arguments)]
    pub fn attest(
        operation_id: impl Into<String>,
        target_id: impl Into<String>,
        script_id: impl Into<String>,
        script_version: u64,
        verifier_id: impl Into<String>,
        evidence_refs: Vec<String>,
        verified_at_ms: u64,
    ) -> Result<Self, KnowledgeError> {
        let record = BusinessVerificationRecord {
            operation_id: operation_id.into(),
            target_id: target_id.into(),
            script_id: script_id.into(),
            script_version,
            verifier_id: verifier_id.into(),
            evidence_refs,
            verified_at_ms,
        };
        super::validation::verification(&record)?;
        Ok(Self(record))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCase {
    pub result: RepairCase,
    pub verification: Option<BusinessVerificationRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeDisablement {
    pub actor: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRecord {
    pub id: String,
    pub revision: u64,
    pub candidate: KnowledgeCandidate,
    pub status: KnowledgeStatus,
    pub cases: Vec<KnowledgeCase>,
    pub disabled: Option<KnowledgeDisablement>,
}

/// Every candidate condition and script precondition must exactly equal a supplied
/// condition. Keywords use AND, exact, case-sensitive matching; no fuzzy match
/// can bypass applicability checks. A query must supply at least one condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnowledgeQuery {
    pub conditions: BTreeMap<String, String>,
    pub keywords: Vec<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnowledgeInspectionQuery {
    pub after_id: Option<String>,
    pub status: Option<KnowledgeStatus>,
    pub quarantined: Option<bool>,
    pub limit: usize,
}

/// Bounded operational metadata. Script contents and verification evidence are
/// deliberately absent; callers use the restricted record API for those reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnowledgeRecordProjection {
    pub id: String,
    pub revision: u64,
    pub incident_id: String,
    pub summary: String,
    pub status: KnowledgeStatus,
    pub script_id: String,
    pub script_version: u64,
    pub script_bytes: usize,
    pub reusable: bool,
    pub quarantined: bool,
    pub case_count: usize,
    pub verified_case_count: usize,
    pub latest_verified_at_ms: Option<u64>,
    pub disabled: Option<KnowledgeDisablement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct KnowledgeStatusCounts {
    pub candidate: usize,
    pub verified: usize,
    pub failed: usize,
    pub unknown: usize,
    pub disabled: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnowledgeProjection {
    pub records: usize,
    pub scripts: usize,
    pub cases: usize,
    pub quarantined_versions: usize,
    pub statuses: KnowledgeStatusCounts,
    pub max_records: usize,
    pub max_cases_per_record: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KnowledgeConfig {
    pub max_records: usize,
    pub max_cases_per_record: usize,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            max_records: 1024,
            max_cases_per_record: 128,
        }
    }
}

impl KnowledgeConfig {
    pub fn validate(&self) -> Result<(), KnowledgeError> {
        if !(1..=100_000).contains(&self.max_records)
            || !(1..=1024).contains(&self.max_cases_per_record)
        {
            return Err(KnowledgeError::Invalid("store limits out of range".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum KnowledgeError {
    #[error("invalid knowledge input: {0}")]
    Invalid(String),
    #[error("knowledge state conflict: {0}")]
    Conflict(String),
    #[error("knowledge record not found: {0}")]
    NotFound(String),
    #[error("knowledge capacity exhausted: {0}")]
    Capacity(String),
    #[error(transparent)]
    Commit(#[from] crate::operation::CommitError),
}

/// Trusted Host inputs. This enum deliberately cannot be deserialized into authority.
#[derive(Clone, Debug, Serialize)]
pub enum KnowledgeCommand {
    UpsertCandidate(KnowledgeCandidate),
    RecordOutcome {
        record_id: String,
        case: RepairCase,
        verification: Option<TrustedBusinessVerification>,
    },
    Disable {
        record_id: String,
        expected_revision: u64,
        actor: String,
        reason: String,
    },
}

/// Host supplies a confirmed historical transaction and reconstructs any trusted
/// business assertion only from its protected authoritative records.
pub struct KnowledgeReplayEntry {
    pub request: crate::operation::CommitRequest,
    pub command: KnowledgeCommand,
    pub receipt: crate::operation::CommitReceipt,
}

/// Complete data export for Host transport and queries, not installable authority.
/// Immutable scripts and all case/disablement facts are retained inside records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnowledgeSnapshot {
    pub revision: u64,
    pub config: KnowledgeConfig,
    pub records: Vec<KnowledgeRecord>,
}
