use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TargetBinding {
    pub target_id: String,
    /// Stable logical executor identity. Host owns how this identity is routed.
    pub executor_id: String,
    pub platform: String,
    pub allowed_languages: Vec<String>,
    pub diagnostic_queries: Vec<String>,
    pub verification_profile: String,
    pub required_facts: BTreeMap<String, String>,
    pub action_timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    pub schema_version: u32,
    /// Logical Harness identity selected by policy; Host resolves its route/workspace.
    pub execution_harness: String,
    pub target: TargetBinding,
    pub approval: approval::ApprovalPolicy,
    /// Reuse always receives a fresh independent Harness review.
    pub script_approval: approval::ApprovalPolicy,
    pub diagnosis_timeout_secs: u64,
    pub review_timeout_secs: u64,
    pub max_tool_calls: usize,
    pub max_diagnoses: u32,
    /// Minimum distinct incident episodes, never the number of abnormal samples.
    pub minimum_script_occurrences: u64,
    pub max_tasks: usize,
}

impl RecoveryConfig {
    pub fn validate(&self) -> Result<(), RecoveryError> {
        self.approval.validate()?;
        self.script_approval.validate()?;
        for value in [
            &self.execution_harness,
            &self.target.target_id,
            &self.target.executor_id,
            &self.target.platform,
            &self.target.verification_profile,
        ] {
            if !crate::identity::valid_id(value) {
                return Err(RecoveryError::Invalid("invalid identity".into()));
            }
        }
        if self.schema_version != 1
            || !(1..=1800).contains(&self.diagnosis_timeout_secs)
            || !(1..=1800).contains(&self.review_timeout_secs)
            || !(1..=1800).contains(&self.target.action_timeout_secs)
            || !(1..=64).contains(&self.max_tool_calls)
            || !(1..=8).contains(&self.max_diagnoses)
            || !(1..=100_000).contains(&self.minimum_script_occurrences)
            || !(1..=10_000).contains(&self.max_tasks)
            || self.target.allowed_languages.is_empty()
            || self.target.allowed_languages.len() > 3
            || self
                .target
                .allowed_languages
                .iter()
                .any(|v| !matches!(v.as_str(), "powershell" | "sh" | "python"))
            || self.target.diagnostic_queries.is_empty()
            || self.target.diagnostic_queries.len() > 32
            || self
                .target
                .diagnostic_queries
                .iter()
                .any(|v| !crate::identity::valid_id(v))
            || !matches!(
                self.script_approval.reviewer,
                approval::ReviewerConfig::Harness { .. }
            )
        {
            return Err(RecoveryError::Invalid(
                "invalid recovery limits or script review policy".into(),
            ));
        }
        facts(&self.target.required_facts)?;
        for policy in [&self.approval, &self.script_approval] {
            if !policy.allowed_targets.contains(&self.target.target_id)
                || !policy
                    .allowed_action_kinds
                    .iter()
                    .any(|kind| matches!(kind.as_str(), "execute_script" | "repair_with_harness"))
            {
                return Err(RecoveryError::Invalid(
                    "target/script action not explicitly delegated".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProblemContext {
    pub incident_id: String,
    pub incident_revision: u64,
    pub target_id: String,
    pub fingerprint: String,
    pub summary: String,
    /// Abnormal observation samples within this incident, not repeated episodes.
    pub occurrences: u64,
    pub keywords: Vec<String>,
    pub conditions: BTreeMap<String, String>,
    pub evidence_refs: Vec<String>,
}

impl ProblemContext {
    pub(super) fn validate(&self) -> Result<(), RecoveryError> {
        for value in [&self.incident_id, &self.target_id, &self.fingerprint] {
            text(value, 128)?;
        }
        text(&self.summary, 8192)?;
        if self.incident_revision == 0 || self.occurrences == 0 || self.keywords.len() > 32 {
            return Err(RecoveryError::Invalid(
                "invalid problem revision or limits".into(),
            ));
        }
        let mut words = BTreeSet::new();
        for word in &self.keywords {
            text(word, 128)?;
            if !words.insert(word) {
                return Err(RecoveryError::Invalid("duplicate problem keyword".into()));
            }
        }
        facts(&self.conditions)?;
        evidence(&self.evidence_refs)
    }
}

pub(super) fn facts(values: &BTreeMap<String, String>) -> Result<(), RecoveryError> {
    if values.len() > 32 {
        return Err(RecoveryError::Capacity);
    }
    for (key, value) in values {
        text(key, 128)?;
        text(value, 1024)?;
    }
    Ok(())
}
pub(super) fn evidence(values: &[String]) -> Result<(), RecoveryError> {
    if values.is_empty() || values.len() > 32 {
        return Err(RecoveryError::Invalid(
            "evidence required, maximum 32 references".into(),
        ));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        text(value, 1024)?;
        if !unique.insert(value) {
            return Err(RecoveryError::Invalid(
                "duplicate evidence reference".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetObservation {
    pub target_id: String,
    pub facts: BTreeMap<String, String>,
    pub evidence_refs: Vec<String>,
    pub observed_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairPlan {
    pub summary: String,
    pub script: ScriptArtifact,
    pub reusable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScriptOutcome {
    Executed,
    Failed,
    Unknown,
}

/// Domain result returned by the external executor through the Host adapter.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptReceipt {
    /// Actual actions supplied by the trusted backend, never by the model's final text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_trace: Vec<ScriptArtifact>,
    pub operation_id: String,
    pub target_id: String,
    pub outcome: ScriptOutcome,
    pub executor_stopped: bool,
    pub evidence_refs: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStage {
    Queued,
    Diagnosing,
    AwaitingApproval,
    Executing,
    Verifying,
    Completed,
    Failed,
    Denied,
    Canceled,
    Unknown,
    Paused,
}
impl RecoveryStage {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Denied | Self::Canceled
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryTask {
    pub id: String,
    pub revision: u64,
    pub problem: ProblemContext,
    /// Trusted number of distinct matching incident episodes recorded by this store.
    /// Legacy logs are normalized from incident identities during validated replay.
    #[serde(default)]
    pub episode_count: u64,
    pub stage: RecoveryStage,
    pub diagnosis_attempts: u32,
    pub plan: Option<RepairPlan>,
    pub knowledge_id: Option<String>,
    pub reused_script: bool,
    /// None while AwaitingApproval has a durable operation awaiting idempotent association.
    pub approval_id: Option<String>,
    pub operation: Option<approval::ProposedOperation>,
    pub observation: Option<TargetObservation>,
    pub receipt: Option<ScriptReceipt>,
    pub verification: Option<BusinessVerification>,
    /// Independent execution facts and caller attribution, never inferred from health.
    #[serde(default)]
    pub result_check: Option<ResultCheckRecord>,
    pub note: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub diagnosis_call: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BusinessVerification {
    pub operation_id: String,
    pub target_id: String,
    pub profile: String,
    /// None means insufficient evidence, never successful recovery.
    pub healthy: Option<bool>,
    pub executor_stopped: bool,
    pub evidence_refs: Vec<String>,
    pub verified_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckedExecution {
    Executed,
    Failed,
    NotExecuted,
    Unknown,
}

/// Trusted operation-status evidence from the bound executor, independent of
/// business health. A healthy target does not prove that this operation ran.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResultCheck {
    pub operation_id: String,
    pub target_id: String,
    pub executor_id: String,
    pub outcome: CheckedExecution,
    pub executor_stopped: bool,
    pub evidence_refs: Vec<String>,
    /// Unix-millisecond timestamp attached to the trusted executor evidence.
    pub checked_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResultCheckRecord {
    pub execution: ExecutionResultCheck,
    /// Audit attribution supplied by the trusted caller, not authentication.
    pub actor: String,
}
