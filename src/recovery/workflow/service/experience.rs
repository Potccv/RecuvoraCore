use super::*;

/// A single bounded repair session. Experience is reference material, not authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRepairRequest {
    pub problem: ProblemContext,
    pub observation: TargetObservation,
    pub experiences: Vec<RepairExperience>,
    pub harness_id: String,
    pub delegation: String,
    pub target: TargetBinding,
    pub max_tool_calls: usize,
    pub summarize_experience: bool,
    pub assess_scriptability: bool,
}

/// Persisted independently of business completion, with a stable delivery identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceJob {
    pub id: String,
    pub task: RecoveryTask,
    pub outcome: RepairOutcome,
    pub recorded_at_ms: u64,
    pub attempt: u64,
    pub call_id: Option<String>,
    pub report: Option<ExperienceReport>,
    pub last_error: Option<String>,
    pub delivered: bool,
}
impl ExperienceJob {
    pub fn record(&self, platform: &str) -> Result<RepairExperience, RecoveryError> {
        let operation = self
            .task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing repair operation"))?;
        let receipt = self
            .task
            .receipt
            .as_ref()
            .ok_or_else(|| invalid("missing repair receipt"))?;
        let mut conditions = self
            .task
            .observation
            .as_ref()
            .ok_or_else(|| invalid("missing observation"))?
            .facts
            .clone();
        conditions.insert(
            "fault_fingerprint".into(),
            self.task.problem.fingerprint.clone(),
        );
        conditions.insert("platform".into(), platform.into());
        Ok(RepairExperience {
            id: self.id.clone(),
            operation_id: operation.operation_id.clone(),
            target_id: operation.target.clone(),
            conditions,
            keywords: self.task.problem.keywords.clone(),
            outcome: self.outcome,
            evidence_refs: self.task.verification.as_ref().map_or_else(
                || receipt.evidence_refs.clone(),
                |v| v.evidence_refs.clone(),
            ),
            recorded_at_ms: self.recorded_at_ms,
            report: self
                .report
                .clone()
                .ok_or_else(|| invalid("summary not completed"))?,
        })
    }
}

impl RecoveryState {
    pub fn pending_experiences(&self) -> Vec<ExperienceJob> {
        let mut jobs: Vec<_> = self
            .experiences
            .values()
            .filter(|job| !job.delivered)
            .cloned()
            .collect();
        jobs.sort_by(|a, b| {
            a.recorded_at_ms
                .cmp(&b.recorded_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        jobs
    }
}
