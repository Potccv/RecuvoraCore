use super::*;

/// Maximum serialized repair request, including fault, observation and references.
pub const MAX_REPAIR_REQUEST_BYTES: usize = 32 * 1024;
pub(super) const MAX_MATCHED_EXPERIENCES: usize = 100_000;

/// A single bounded repair session. Experience is reference material, not authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRepairRequest {
    pub problem: ProblemContext,
    pub observation: TargetObservation,
    pub matched_experience_count: usize,
    pub experiences: Vec<RepairExperience>,
    pub harness_id: String,
    pub delegation: String,
    pub target: TargetBinding,
    pub max_tool_calls: usize,
    pub summarize_experience: bool,
    pub assess_scriptability: bool,
}

impl HarnessRepairRequest {
    pub(super) fn within_size_limit(&self) -> Result<bool, RecoveryError> {
        serde_json::to_vec(self)
            .map(|encoded| encoded.len() <= MAX_REPAIR_REQUEST_BYTES)
            .map_err(|_| invalid("cannot encode repair request"))
    }

    pub(super) fn within_size_limit_with(
        &self,
        experiences: &[&RepairExperience],
    ) -> Result<bool, RecoveryError> {
        #[derive(Serialize)]
        struct RequestView<'a> {
            problem: &'a ProblemContext,
            observation: &'a TargetObservation,
            matched_experience_count: usize,
            experiences: &'a [&'a RepairExperience],
            harness_id: &'a str,
            delegation: &'a str,
            target: &'a TargetBinding,
            max_tool_calls: usize,
            summarize_experience: bool,
            assess_scriptability: bool,
        }

        let view = RequestView {
            problem: &self.problem,
            observation: &self.observation,
            matched_experience_count: self.matched_experience_count,
            experiences,
            harness_id: &self.harness_id,
            delegation: &self.delegation,
            target: &self.target,
            max_tool_calls: self.max_tool_calls,
            summarize_experience: self.summarize_experience,
            assess_scriptability: self.assess_scriptability,
        };
        serde_json::to_vec(&view)
            .map(|encoded| encoded.len() <= MAX_REPAIR_REQUEST_BYTES)
            .map_err(|_| invalid("cannot encode repair request"))
    }
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
    pub fn record(&self) -> Result<RepairExperience, RecoveryError> {
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
        let observation = self
            .task
            .observation
            .as_ref()
            .ok_or_else(|| invalid("missing observation"))?;
        let request: HarnessRepairRequest = serde_json::from_value(
            operation
                .action
                .get("request")
                .cloned()
                .ok_or_else(|| invalid("missing repair request"))?,
        )
        .map_err(|_| invalid("invalid repair request"))?;
        let conditions = super::contract::stable_conditions(&self.task.problem, &request.target)?;
        if conditions
            .iter()
            .filter(|(key, _)| key.as_str() != super::contract::FAULT_FINGERPRINT_CONDITION)
            .any(|(key, value)| observation.facts.get(key) != Some(value))
        {
            return Err(invalid("incident environment changed"));
        }
        Ok(RepairExperience {
            id: self.id.clone(),
            operation_id: operation.operation_id.clone(),
            target_id: operation.target.clone(),
            conditions,
            keywords: self.task.problem.keywords.clone(),
            outcome: self.outcome,
            actions: receipt.execution_trace.clone(),
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
