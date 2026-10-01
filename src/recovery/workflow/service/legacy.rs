//! Validated full-history legacy import; no filesystem format or executable effects.
use super::*;
use crate::operation::{CommitReceipt, Prepared};
use approval::{
    ApprovalChange, ApprovalEntry, ApprovalLedger, ApprovalLimits, ApprovalState, ExecutionOutcome,
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LegacyRecoveryStage {
    Queued,
    Diagnosing,
    AwaitingApproval,
    Executing,
    Verifying,
    Publishing,
    Completed,
    Failed,
    Denied,
    Canceled,
    Unknown,
    Paused,
}
impl LegacyRecoveryStage {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Denied | Self::Canceled
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyRecoveryTask {
    pub id: String,
    pub revision: u64,
    pub problem: ProblemContext,
    /// Trusted number of distinct matching incident episodes recorded by this store.
    /// Legacy logs are normalized from incident identities during validated replay.
    #[serde(default)]
    pub episode_count: u64,
    pub stage: LegacyRecoveryStage,
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
}

fn same_plan(left: Option<&RepairPlan>, right: Option<&RepairPlan>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.summary == right.summary
                && left.script == right.script
                && left.reusable == right.reusable
        }
        _ => false,
    }
}

fn valid_transition(old: &LegacyRecoveryStage, new: &LegacyRecoveryStage) -> bool {
    use LegacyRecoveryStage::*;
    if *new == Unknown {
        return !old.terminal();
    }
    match old {
        Queued => matches!(new, Diagnosing | AwaitingApproval | Canceled),
        Diagnosing => matches!(new, Diagnosing | AwaitingApproval | Failed | Canceled),
        AwaitingApproval => matches!(new, Executing | Denied | Canceled | Diagnosing | Paused),
        Executing => matches!(new, Verifying | Publishing | Denied | Paused),
        Verifying => matches!(new, Publishing),
        Publishing => matches!(new, Completed | Failed | Diagnosing),
        Unknown => matches!(new, Publishing | Canceled),
        Paused => matches!(new, AwaitingApproval),
        Completed | Failed | Denied | Canceled => false,
    }
}

// Pure validation is shared by live saves, startup replay and offline migration.
struct TaskValidator<'a> {
    tasks: &'a BTreeMap<String, LegacyRecoveryTask>,
    config: &'a RecoveryConfig,
    max_tasks: usize,
}

fn validate_task(
    tasks: &BTreeMap<String, LegacyRecoveryTask>,
    config: &RecoveryConfig,
    task: &LegacyRecoveryTask,
) -> Result<(), RecoveryError> {
    TaskValidator {
        tasks,
        config,
        max_tasks: config.max_tasks,
    }
    .validate(task)
}

impl TaskValidator<'_> {
    fn validate(&self, task: &LegacyRecoveryTask) -> Result<(), RecoveryError> {
        text(&task.id, 128)?;
        task.problem.validate()?;
        self.validate_shape(task)?;
        if task.updated_at_ms < task.created_at_ms {
            return Err(RecoveryError::Invalid("task time moved backwards".into()));
        }
        if let Some(old) = self.tasks.get(&task.id) {
            if Some(task.revision) != old.revision.checked_add(1)
                || task.problem != old.problem
                || task.episode_count != old.episode_count
                || task.created_at_ms != old.created_at_ms
                || task.updated_at_ms < old.updated_at_ms
            {
                return Err(RecoveryError::Invalid(
                    "task revision or identity conflict".into(),
                ));
            }
            let attaching_approval = old.stage == LegacyRecoveryStage::AwaitingApproval
                && task.stage == LegacyRecoveryStage::AwaitingApproval
                && old.approval_id.is_none()
                && task.approval_id.is_some();
            if !valid_transition(&old.stage, &task.stage) && !attaching_approval {
                return Err(RecoveryError::Invalid(
                    "invalid recovery stage transition".into(),
                ));
            }
            if task.diagnosis_attempts != old.diagnosis_attempts
                && !(old.stage == LegacyRecoveryStage::Diagnosing
                    && task.stage == LegacyRecoveryStage::Diagnosing
                    && old.diagnosis_attempts.checked_add(1) == Some(task.diagnosis_attempts))
            {
                return Err(RecoveryError::Invalid(
                    "diagnosis budget transition mismatch".into(),
                ));
            }
            if old.operation.is_some()
                && old.stage != LegacyRecoveryStage::Diagnosing
                && task.stage != LegacyRecoveryStage::Diagnosing
                && (old.operation != task.operation
                    || (old.approval_id != task.approval_id && !attaching_approval)
                    || !same_plan(old.plan.as_ref(), task.plan.as_ref()))
            {
                return Err(RecoveryError::Invalid(
                    "approved operation or script changed across stages".into(),
                ));
            }
            if attaching_approval
                && (serde_json::to_value(&old.observation)
                    .map_err(|_| invalid("legacy serialization"))?
                    != serde_json::to_value(&task.observation)
                        .map_err(|_| invalid("legacy serialization"))?
                    || old.reused_script != task.reused_script
                    || old.knowledge_id != task.knowledge_id
                    || serde_json::to_value(&old.receipt)
                        .map_err(|_| invalid("legacy serialization"))?
                        != serde_json::to_value(&task.receipt)
                            .map_err(|_| invalid("legacy serialization"))?
                    || serde_json::to_value(&old.verification)
                        .map_err(|_| invalid("legacy serialization"))?
                        != serde_json::to_value(&task.verification)
                            .map_err(|_| invalid("legacy serialization"))?
                    || old.result_check != task.result_check
                    || old.diagnosis_attempts != task.diagnosis_attempts)
            {
                return Err(RecoveryError::Invalid(
                    "approval association changed durable plan evidence".into(),
                ));
            }
            if old.stage == LegacyRecoveryStage::Publishing
                && task.stage != LegacyRecoveryStage::Unknown
                && (serde_json::to_value(&old.receipt)
                    .map_err(|_| invalid("legacy serialization"))?
                    != serde_json::to_value(&task.receipt)
                        .map_err(|_| invalid("legacy serialization"))?
                    || serde_json::to_value(&old.verification)
                        .map_err(|_| invalid("legacy serialization"))?
                        != serde_json::to_value(&task.verification)
                            .map_err(|_| invalid("legacy serialization"))?)
            {
                return Err(RecoveryError::Invalid(
                    "publication changed durable execution or verification evidence".into(),
                ));
            }
            if old.result_check != task.result_check
                && task.stage != LegacyRecoveryStage::Diagnosing
                && old.stage != LegacyRecoveryStage::Diagnosing
                && (old.stage != LegacyRecoveryStage::Unknown
                    || task.stage != LegacyRecoveryStage::Unknown
                    || old.result_check.as_ref().is_some_and(|previous| {
                        previous.execution.outcome != CheckedExecution::Unknown
                            && task.result_check.as_ref().is_none_or(|next| {
                                previous.execution.outcome != next.execution.outcome
                            })
                    }))
            {
                return Err(RecoveryError::Invalid(
                    "result check facts changed outside explicit unknown recovery".into(),
                ));
            }
        } else if task.revision != 1 || self.tasks.len() >= self.max_tasks {
            return Err(RecoveryError::Capacity);
        } else if task.episode_count
            != self
                .tasks
                .values()
                .filter(|old| {
                    old.problem.target_id == task.problem.target_id
                        && old.problem.fingerprint == task.problem.fingerprint
                        && old.problem.conditions == task.problem.conditions
                })
                .count() as u64
                + 1
            || task.stage != LegacyRecoveryStage::Queued
            || task.diagnosis_attempts != 0
            || task.plan.is_some()
            || task.operation.is_some()
            || task.approval_id.is_some()
            || task.observation.is_some()
            || task.receipt.is_some()
            || task.verification.is_some()
            || task.result_check.is_some()
            || task.knowledge_id.is_some()
            || task.reused_script
            || self
                .tasks
                .values()
                .any(|old| old.problem.incident_id == task.problem.incident_id)
        {
            return Err(RecoveryError::Invalid(
                "initial task must be a unique queued incident".into(),
            ));
        }
        Ok(())
    }

    fn validate_shape(&self, task: &LegacyRecoveryTask) -> Result<(), RecoveryError> {
        use LegacyRecoveryStage::*;
        if task.episode_count == 0
            || task.problem.target_id != self.config.target.target_id
            || task.diagnosis_attempts > self.config.max_diagnoses
        {
            return Err(RecoveryError::Invalid(
                "task target or diagnosis budget differs from configuration".into(),
            ));
        }
        if let Some(note) = &task.note {
            text(note, 64 * 1024)?;
        }
        for id in [&task.approval_id, &task.knowledge_id]
            .into_iter()
            .flatten()
        {
            text(id, 256)?;
        }
        if matches!(
            task.stage,
            AwaitingApproval | Executing | Verifying | Publishing | Completed | Unknown | Paused
        ) && (task.plan.is_none()
            || task.operation.is_none()
            || (task.approval_id.is_none() && task.stage != AwaitingApproval)
            || task.observation.is_none())
        {
            return Err(RecoveryError::Invalid(
                "stage lacks plan, operation, approval or observation".into(),
            ));
        }
        if task.reused_script && (task.plan.is_none() || task.knowledge_id.is_none()) {
            return Err(RecoveryError::Invalid(
                "reused script lacks a knowledge record".into(),
            ));
        }
        if task.approval_id.is_some() != task.operation.is_some()
            && !(task.stage == AwaitingApproval && task.operation.is_some())
        {
            return Err(RecoveryError::Invalid(
                "operation and approval references differ".into(),
            ));
        }
        if let Some(plan) = &task.plan {
            text(&plan.summary, 4096)?;
            let script = &plan.script;
            text(&script.id, 128)?;
            text(&script.source, MAX_SCRIPT_BYTES)?;
            text(&script.generated_by_harness, 128)?;
            text(&script.generated_in_session, 256)?;
            contract::facts(&script.preconditions)?;
            if script.version == 0
                || script.preconditions.is_empty()
                || script.platform != self.config.target.platform
                || !self
                    .config
                    .target
                    .allowed_languages
                    .contains(&script.language)
            {
                return Err(RecoveryError::Invalid(
                    "invalid persisted script contract".into(),
                ));
            }
        }

        if let Some(observation) = &task.observation {
            contract::facts(&observation.facts)?;
            contract::evidence(&observation.evidence_refs)?;
            if observation.target_id != task.problem.target_id
                || observation.observed_at_ms > task.updated_at_ms
            {
                return Err(RecoveryError::Invalid(
                    "persisted observation identity or time mismatch".into(),
                ));
            }
        }
        if let Some(operation) = &task.operation {
            operation.validate()?;
            let plan = task
                .plan
                .as_ref()
                .ok_or_else(|| RecoveryError::Invalid("operation lacks script".into()))?;
            let action = serde_json::json!({"kind":"execute_script","executor_id":self.config.target.executor_id,
                "script":plan.script,"verification_profile":self.config.target.verification_profile,
                "required_facts":self.config.target.required_facts,"timeout_secs":self.config.target.action_timeout_secs,
                "incident_id":task.problem.incident_id,"incident_revision":task.problem.incident_revision});
            if operation.task_id != task.id
                || operation.target != task.problem.target_id
                || operation.task_revision > task.revision
                || operation.action != action
            {
                return Err(RecoveryError::Invalid(
                    "persisted operation does not bind task, policy or script".into(),
                ));
            }
        }
        if let Some(receipt) = &task.receipt {
            contract::evidence(&receipt.evidence_refs)?;
            text(&receipt.summary, 8192)?;
            if receipt.target_id != task.problem.target_id
                || task
                    .operation
                    .as_ref()
                    .is_some_and(|operation| receipt.operation_id != operation.operation_id)
            {
                return Err(RecoveryError::Invalid(
                    "receipt identity differs from task operation".into(),
                ));
            }
        }
        if let Some(verification) = &task.verification {
            contract::evidence(&verification.evidence_refs)?;
            if verification.target_id != task.problem.target_id
                || verification.profile != self.config.target.verification_profile
                || verification.verified_at_ms > task.updated_at_ms
                || task
                    .operation
                    .as_ref()
                    .is_some_and(|operation| verification.operation_id != operation.operation_id)
            {
                return Err(RecoveryError::Invalid(
                    "verification identity or time differs from task".into(),
                ));
            }
        }
        if let Some(record) = &task.result_check {
            let execution = &record.execution;
            text(&record.actor, 128)?;
            contract::evidence(&execution.evidence_refs)?;
            let operation = task.operation.as_ref().ok_or_else(|| {
                RecoveryError::Invalid("execution result check lacks operation".into())
            })?;
            if execution.operation_id != operation.operation_id
                || execution.target_id != operation.target
                || execution.executor_id != self.config.target.executor_id
                || execution.checked_at_ms > task.updated_at_ms
                || execution.checked_at_ms < task.created_at_ms
                || (execution.outcome != CheckedExecution::Unknown && !execution.executor_stopped)
                || task.verification.as_ref().is_none_or(|verification| {
                    verification.executor_stopped != execution.executor_stopped
                })
                || (execution.outcome == CheckedExecution::Unknown && task.stage != Unknown)
                || (execution.outcome == CheckedExecution::NotExecuted
                    && !matches!(task.stage, Unknown | Canceled))
                || task.receipt.as_ref().is_some_and(|receipt| {
                    receipt.executor_stopped
                        && match receipt.outcome {
                            ScriptOutcome::Executed => {
                                execution.outcome != CheckedExecution::Executed
                            }
                            ScriptOutcome::Failed => execution.outcome != CheckedExecution::Failed,
                            ScriptOutcome::Unknown => false,
                        }
                })
            {
                return Err(RecoveryError::Invalid(
                    "persisted execution result check identity, time or state mismatch".into(),
                ));
            }
            if matches!(task.stage, Publishing | Completed | Failed)
                && task.receipt.as_ref().is_none_or(|receipt| {
                    !receipt.executor_stopped
                        || receipt.outcome
                            != match execution.outcome {
                                CheckedExecution::Executed => ScriptOutcome::Executed,
                                CheckedExecution::Failed => ScriptOutcome::Failed,
                                _ => ScriptOutcome::Unknown,
                            }
                        || receipt.evidence_refs != execution.evidence_refs
                })
            {
                return Err(RecoveryError::Invalid(
                    "receipt differs from independent execution result check".into(),
                ));
            }
        }
        if matches!(task.stage, Verifying | Publishing | Completed) && task.receipt.is_none() {
            return Err(RecoveryError::Invalid(
                "post-execution stage lacks receipt".into(),
            ));
        }
        if task.stage == Verifying
            && !task.receipt.as_ref().is_some_and(|receipt| {
                receipt.outcome == ScriptOutcome::Executed && receipt.executor_stopped
            })
        {
            return Err(RecoveryError::Invalid(
                "verification requires confirmed stopped execution".into(),
            ));
        }
        if task.stage == Completed
            && (!task.receipt.as_ref().is_some_and(|receipt| {
                receipt.outcome == ScriptOutcome::Executed && receipt.executor_stopped
            }) || !task.verification.as_ref().is_some_and(|verification| {
                verification.healthy == Some(true) && verification.executor_stopped
            }))
        {
            return Err(RecoveryError::Invalid(
                "completion requires successful independent business verification".into(),
            ));
        }
        Ok(())
    }
}

/// One original committed task revision. Host parses physical records and checks
/// their original configuration; Core validates format progression and all revisions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyRecoveryRevision {
    pub format: u32,
    pub sequence: u64,
    pub task: LegacyRecoveryTask,
}

/// Complete protected input retained in the import event for repeat validation.
/// Its private fields are not a live authority-installation interface.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryImportData {
    config: RecoveryConfig,
    revisions: Vec<LegacyRecoveryRevision>,
    approval_limits: ApprovalLimits,
    approvals: Vec<ApprovalEntry>,
    knowledge_config: KnowledgeConfig,
    knowledge: Vec<KnowledgeRecord>,
}

/// A validated migration proposal, never deserializable and never a permit.
pub struct RecoveryImport(pub(super) RecoveryImportData);
impl RecoveryImport {
    pub fn validate(
        config: RecoveryConfig,
        revisions: Vec<LegacyRecoveryRevision>,
        approvals: &ApprovalLedger,
        knowledge: &KnowledgeState,
    ) -> Result<Self, RecoveryError> {
        let snapshot = knowledge.snapshot();
        let data = RecoveryImportData {
            config: config.clone(),
            revisions,
            approval_limits: approvals.limits().clone(),
            approvals: approvals.entries(),
            knowledge_config: snapshot.config,
            knowledge: snapshot.records,
        };
        let now = data
            .revisions
            .iter()
            .map(|entry| entry.task.updated_at_ms)
            .max()
            .unwrap_or(0);
        validate_import(&data, &config, now, true)?;
        Ok(Self(data))
    }
    /// Exact original approvals whose execution intent must be sealed Unknown
    /// before installing this workflow. This never consumes an execution permit.
    pub fn execution_uncertainties(&self) -> Vec<approval::LegacyExecutionUncertainty> {
        let ledger = ApprovalLedger::restore(self.0.approval_limits.clone(), &self.0.approvals)
            .expect("validated approval history");
        let mut latest = BTreeMap::new();
        for entry in &self.0.revisions {
            latest.insert(&entry.task.id, &entry.task);
        }
        latest
            .values()
            .filter_map(|task| {
                if !matches!(
                    task.stage,
                    LegacyRecoveryStage::Unknown | LegacyRecoveryStage::Executing
                ) {
                    return None;
                }
                let op = task.operation.as_ref()?;
                let record = ledger.find_operation(&op.task_id, &op.operation_id)?;
                (record.state == ApprovalState::Approved).then(|| {
                    approval::LegacyExecutionUncertainty {
                        operation: op.clone(),
                        request_id: record.request.request_id.clone(),
                        revision: record.revision,
                    }
                })
            })
            .collect()
    }
}
impl RecoveryState {
    /// Prepare one atomic initial import. Confirmation yields no external effects.
    /// Persist its complete latest_entry and then commit Recover before new work.
    pub fn prepare_import(
        &self,
        commit_id: impl Into<String>,
        import: RecoveryImport,
        now_ms: u64,
    ) -> Result<Prepared<Self, RecoveryEffect>, RecoveryError> {
        self.prepare(
            commit_id,
            RecoveryCommand::Import(import),
            now_ms,
            &KnowledgeState::new(KnowledgeConfig::default())?,
        )
    }
}

pub(super) struct Imported {
    pub tasks: crate::collections::Map<String, RecoveryTask>,
    pub scripts: crate::collections::Map<(String, u64), ScriptArtifact>,
    pub candidates: crate::collections::Map<String, KnowledgeCandidate>,
    pub quarantined: im::OrdSet<(String, u64)>,
    pub deliveries: crate::collections::Map<String, KnowledgeDelivery>,
    pub delivery_order: im::OrdMap<String, u64>,
}

pub(super) fn validate_import(
    data: &RecoveryImportData,
    config: &RecoveryConfig,
    now: u64,
    allow_unsealed: bool,
) -> Result<Imported, RecoveryError> {
    config.validate()?;
    if &data.config != config || data.revisions.is_empty() || data.revisions.len() > 1_000_000 {
        return Err(invalid(
            "import requires complete bounded history and original configuration",
        ));
    }
    let approvals = ApprovalLedger::restore(data.approval_limits.clone(), &data.approvals)?;
    let knowledge = checked_knowledge(data)?;
    let mut approval_changes: BTreeMap<String, Vec<(String, ApprovalChange, u64)>> =
        BTreeMap::new();
    for change in approvals.historical_changes() {
        approval_changes
            .entry(change.0.clone())
            .or_default()
            .push(change);
    }
    let mut result = Imported {
        tasks: crate::collections::Map::new(),
        scripts: crate::collections::Map::new(),
        candidates: crate::collections::Map::new(),
        quarantined: im::OrdSet::new(),
        deliveries: crate::collections::Map::new(),
        delivery_order: im::OrdMap::new(),
    };
    for record in &data.knowledge {
        retain_script(&mut result, &record.candidate.script)?;
        result
            .candidates
            .insert(record.id.clone(), record.candidate.clone());
        if knowledge.is_quarantined(&record.candidate.script.id, record.candidate.script.version) {
            result.quarantined.insert((
                record.candidate.script.id.clone(),
                record.candidate.script.version,
            ));
        }
    }
    let mut tasks = BTreeMap::new();
    let mut operations = BTreeMap::new();
    let mut format = 1;
    for (index, entry) in data.revisions.iter().enumerate() {
        if entry.sequence != index as u64 + 1
            || !(1..=2).contains(&entry.format)
            || entry.format < format
        {
            return Err(invalid("legacy sequence or format progression is invalid"));
        }
        format = entry.format;
        let mut task = entry.task.clone();
        if task.updated_at_ms > now {
            return Err(invalid("import time precedes committed history"));
        }
        if entry.format == 1 && task.episode_count == 0 {
            task.episode_count = tasks
                .get(&task.id)
                .map(|previous: &LegacyRecoveryTask| previous.episode_count)
                .unwrap_or_else(|| {
                    tasks
                        .values()
                        .filter(|old| {
                            old.problem.target_id == task.problem.target_id
                                && old.problem.fingerprint == task.problem.fingerprint
                                && old.problem.conditions == task.problem.conditions
                        })
                        .count() as u64
                        + 1
                });
        }
        validate_task(&tasks, config, &task)?;
        if !crate::identity::valid_id(&task.id) {
            return Err(invalid("legacy task identity is invalid"));
        }
        if !tasks.contains_key(&task.id)
            && tasks.values().any(|previous| !previous.stage.terminal())
        {
            return Err(RecoveryError::Busy);
        }
        if let Some(plan) = &task.plan {
            retain_script(&mut result, &plan.script)?;
        }
        if let Some(op) = &task.operation {
            if operations
                .get(&op.operation_id)
                .is_some_and(|prior| prior != op)
            {
                return Err(invalid("legacy operation identity content changed"));
            }
            operations.insert(op.operation_id.clone(), op.clone());
            let record = approvals.find_operation(&op.task_id, &op.operation_id);
            if task.approval_id.is_some() && record.is_none() {
                return Err(invalid("legacy task approval history missing"));
            }
            if let Some(record) = record {
                let policy = if task.reused_script {
                    &config.script_approval
                } else {
                    &config.approval
                };
                if record.request.operation != *op
                    || &record.request.policy != policy
                    || task
                        .approval_id
                        .as_ref()
                        .is_some_and(|id| id != &record.request.request_id)
                {
                    return Err(invalid(
                        "legacy approval does not bind exact original operation and policy",
                    ));
                }
                if !policy.allows(op) {
                    return Err(invalid("legacy operation outside original hard policy"));
                }
                validate_execution_history(
                    &task,
                    record,
                    approval_changes
                        .get(&record.request.request_id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                )?;
            }
            if task.reused_script {
                let id = task
                    .knowledge_id
                    .as_ref()
                    .ok_or_else(|| invalid("legacy reuse candidate missing"))?;
                let record = knowledge
                    .get(id)
                    .ok_or_else(|| invalid("legacy reuse knowledge history missing"))?;
                let plan = task
                    .plan
                    .as_ref()
                    .ok_or_else(|| invalid("legacy reuse plan missing"))?;
                if !record.candidate.reusable
                    || record.candidate.script != plan.script
                    || task.episode_count < config.minimum_script_occurrences
                    || !record.cases.iter().any(|case| {
                        case.result.outcome == RepairOutcome::Verified
                            && case.verification.is_some()
                            && case.result.recorded_at_ms <= task.updated_at_ms
                    })
                {
                    return Err(invalid(
                        "legacy reuse lacks prior independent successful case",
                    ));
                }
            }
        }
        // Preserve every failure/Unknown exclusion, including attempts absent from
        // the final task after a bounded alternative diagnosis.
        if let Some(plan) = &task.plan
            && (task.stage == LegacyRecoveryStage::Unknown
                || task
                    .receipt
                    .as_ref()
                    .is_some_and(|r| r.outcome != ScriptOutcome::Executed || !r.executor_stopped)
                || task
                    .verification
                    .as_ref()
                    .is_some_and(|v| v.healthy != Some(true) || !v.executor_stopped))
        {
            result
                .quarantined
                .insert((plan.script.id.clone(), plan.script.version));
        }
        if task.stage == LegacyRecoveryStage::Publishing {
            add_delivery(&mut result, &task, config, &knowledge, entry.sequence, None)?;
        }
        tasks.insert(task.id.clone(), task);
    }
    for record in approvals.list() {
        if operations.get(&record.request.operation.operation_id) != Some(&record.request.operation)
        {
            return Err(invalid(
                "approval history contains an operation absent from complete workflow history",
            ));
        }
    }
    for old in tasks.values() {
        let record = old
            .operation
            .as_ref()
            .and_then(|op| approvals.find_operation(&op.task_id, &op.operation_id));
        let consumed = record.is_some_and(|r| {
            matches!(
                r.state,
                ApprovalState::Executing
                    | ApprovalState::Unknown
                    | ApprovalState::Executed
                    | ApprovalState::Failed
            )
        });
        let stage = match old.stage {
            LegacyRecoveryStage::Queued => RecoveryStage::Queued,
            LegacyRecoveryStage::Diagnosing => RecoveryStage::Diagnosing,
            LegacyRecoveryStage::AwaitingApproval | LegacyRecoveryStage::Paused if !consumed => {
                RecoveryStage::Paused
            }
            LegacyRecoveryStage::AwaitingApproval
            | LegacyRecoveryStage::Paused
            | LegacyRecoveryStage::Executing
            | LegacyRecoveryStage::Verifying
            | LegacyRecoveryStage::Unknown => RecoveryStage::Unknown,
            LegacyRecoveryStage::Publishing => match outcome(old)? {
                RepairOutcome::Verified => RecoveryStage::Completed,
                RepairOutcome::Failed => RecoveryStage::Failed,
                RepairOutcome::Unknown => RecoveryStage::Unknown,
            },
            LegacyRecoveryStage::Completed => RecoveryStage::Completed,
            LegacyRecoveryStage::Failed => RecoveryStage::Failed,
            LegacyRecoveryStage::Denied => RecoveryStage::Denied,
            LegacyRecoveryStage::Canceled => RecoveryStage::Canceled,
        };
        let mut task = convert(old, stage);
        if let Some(record) = record {
            task.approval_id = Some(record.request.request_id.clone());
        }
        if task.stage == RecoveryStage::Unknown {
            if !consumed
                && !(allow_unsealed
                    && record.is_some_and(|r| r.state == ApprovalState::Approved)
                    && matches!(
                        old.stage,
                        LegacyRecoveryStage::Unknown | LegacyRecoveryStage::Executing
                    ))
            {
                return Err(invalid(
                    "legacy Unknown lacks consumed or sealed original approval authority",
                ));
            }
            let op = task
                .operation
                .as_ref()
                .ok_or_else(|| invalid("legacy Unknown lacks original operation"))?;
            if task.receipt.is_none() {
                task.receipt = Some(ScriptReceipt {
                    operation_id: op.operation_id.clone(),
                    target_id: op.target.clone(),
                    outcome: ScriptOutcome::Unknown,
                    executor_stopped: false,
                    evidence_refs: vec![format!(
                        "approval:{}",
                        task.approval_id.as_deref().unwrap_or("missing")
                    )],
                    summary: "legacy execution intent requires independent reconciliation".into(),
                });
            }
            let mut unknown = old.clone();
            unknown.receipt = task.receipt.clone();
            add_delivery(
                &mut result,
                &unknown,
                config,
                &knowledge,
                data.revisions.len() as u64 + 1,
                Some(RepairOutcome::Unknown),
            )?;
            if let Some(plan) = &task.plan {
                result
                    .quarantined
                    .insert((plan.script.id.clone(), plan.script.version));
            }
        }
        // Completed and failed tasks must have a prior publication history; a
        // terminal snapshot alone can never install a successful business fact.
        if matches!(
            old.stage,
            LegacyRecoveryStage::Completed | LegacyRecoveryStage::Failed
        ) && let Some(op) = &old.operation
        {
            let delivery = result
                .deliveries
                .values()
                .find(|d| {
                    d.case.operation_id == op.operation_id
                        && Some(d.case.outcome) == outcome(old).ok()
                })
                .ok_or_else(|| {
                    invalid("legacy terminal execution lacks matching publication history")
                })?;
            if old.knowledge_id.as_ref() != Some(&delivery.candidate.id) {
                return Err(invalid("legacy terminal candidate association changed"));
            }
        }
        if old.stage == LegacyRecoveryStage::Publishing {
            let op = old
                .operation
                .as_ref()
                .ok_or_else(|| invalid("publication operation missing"))?;
            if let Some(delivery) = result.deliveries.values().find(|d| {
                d.case.operation_id == op.operation_id && Some(d.case.outcome) == outcome(old).ok()
            }) {
                task.knowledge_id = Some(delivery.candidate.id.clone());
            }
        }
        result.tasks.insert(task.id.clone(), task);
    }
    if result
        .tasks
        .values()
        .filter(|task| !task.stage.terminal())
        .count()
        > 1
    {
        return Err(RecoveryError::Busy);
    }
    Ok(result)
}

fn checked_knowledge(data: &RecoveryImportData) -> Result<KnowledgeState, RecoveryError> {
    let mut state = KnowledgeState::new(data.knowledge_config.clone())?;
    let mut ids = BTreeSet::new();
    for (i, record) in data.knowledge.iter().enumerate() {
        if !ids.insert(&record.id) {
            return Err(invalid("duplicate imported knowledge identity"));
        }
        let proposal = state.propose(
            format!("legacy-candidate-{i}"),
            KnowledgeCommand::UpsertCandidate(record.candidate.clone()),
        )?;
        let receipt = CommitReceipt::confirmed(proposal.request());
        state = proposal.confirm(receipt)?.state;
        for (j, case) in record.cases.iter().enumerate() {
            let proof = case
                .verification
                .as_ref()
                .map(|v| {
                    TrustedBusinessVerification::attest(
                        &v.operation_id,
                        &v.target_id,
                        &v.script_id,
                        v.script_version,
                        &v.verifier_id,
                        v.evidence_refs.clone(),
                        v.verified_at_ms,
                    )
                })
                .transpose()?;
            let proposal = state.propose(
                format!("legacy-case-{i}-{j}"),
                KnowledgeCommand::RecordOutcome {
                    record_id: record.id.clone(),
                    case: case.result.clone(),
                    verification: proof,
                },
            )?;
            let receipt = CommitReceipt::confirmed(proposal.request());
            state = proposal.confirm(receipt)?.state;
        }
        if let Some(disabled) = &record.disabled {
            let revision = state
                .get(&record.id)
                .ok_or_else(|| invalid("imported knowledge missing"))?
                .revision;
            let proposal = state.propose(
                format!("legacy-disable-{i}"),
                KnowledgeCommand::Disable {
                    record_id: record.id.clone(),
                    expected_revision: revision,
                    actor: disabled.actor.clone(),
                    reason: disabled.reason.clone(),
                },
            )?;
            let receipt = CommitReceipt::confirmed(proposal.request());
            state = proposal.confirm(receipt)?.state;
        }
        if state.get(&record.id).as_ref() != Some(record) {
            return Err(invalid(
                "knowledge evidence differs from validated candidate/cases",
            ));
        }
    }
    Ok(state)
}
fn retain_script(result: &mut Imported, script: &ScriptArtifact) -> Result<(), RecoveryError> {
    KnowledgeState::new(KnowledgeConfig::default())?.validate_script(script)?;
    let key = (script.id.clone(), script.version);
    if result
        .scripts
        .get(&key)
        .is_some_and(|previous| previous != script)
    {
        return Err(invalid("legacy immutable script version changed"));
    }
    result.scripts.insert(key, script.clone());
    Ok(())
}
fn validate_execution_history(
    task: &LegacyRecoveryTask,
    record: &approval::ApprovalRecord,
    entries: &[(String, ApprovalChange, u64)],
) -> Result<(), RecoveryError> {
    let id = &record.request.request_id;
    let changes: Vec<_> = entries
        .iter()
        .filter_map(|(request_id, change, now)| {
            (request_id == id && *now <= task.updated_at_ms / 1000).then_some(change)
        })
        .collect();
    let consumed = changes
        .iter()
        .any(|change| matches!(change, ApprovalChange::Consume));
    if task.approval_id.is_some() && record.request.created_at > task.updated_at_ms / 1000 {
        return Err(invalid("legacy approval association precedes request"));
    }
    if let Some(receipt) = &task.receipt {
        if !consumed && receipt.outcome != ScriptOutcome::Unknown {
            return Err(invalid("legacy receipt lacks original consumed approval"));
        }
        let expected = match receipt.outcome {
            ScriptOutcome::Executed => ExecutionOutcome::Executed,
            ScriptOutcome::Failed => ExecutionOutcome::Failed,
            ScriptOutcome::Unknown => ExecutionOutcome::Unknown,
        };
        if receipt.outcome != ScriptOutcome::Unknown
            && (!receipt.executor_stopped
                || !changes.iter().any(|change| match change {
                    ApprovalChange::Complete { outcome, .. }
                    | ApprovalChange::Reconcile { outcome, .. } => *outcome == expected,
                    _ => false,
                }))
        {
            return Err(invalid(
                "legacy confirmed receipt lacks matching completed approval history",
            ));
        }
    }
    if let Some(check) = &task.result_check
        && check.execution.outcome != CheckedExecution::Unknown
        && !consumed
    {
        return Err(invalid("legacy result check lacks consumed operation"));
    }
    Ok(())
}
fn outcome(task: &LegacyRecoveryTask) -> Result<RepairOutcome, RecoveryError> {
    let receipt = task
        .receipt
        .as_ref()
        .ok_or_else(|| invalid("legacy publication lacks receipt"))?;
    Ok(
        if receipt.outcome == ScriptOutcome::Unknown || !receipt.executor_stopped {
            RepairOutcome::Unknown
        } else if receipt.outcome == ScriptOutcome::Failed {
            RepairOutcome::Failed
        } else {
            match task.verification.as_ref() {
                Some(v) if v.executor_stopped && v.healthy == Some(true) => RepairOutcome::Verified,
                Some(v) if v.executor_stopped && v.healthy == Some(false) => RepairOutcome::Failed,
                _ => RepairOutcome::Unknown,
            }
        },
    )
}
fn add_delivery(
    result: &mut Imported,
    task: &LegacyRecoveryTask,
    config: &RecoveryConfig,
    knowledge: &KnowledgeState,
    order: u64,
    forced: Option<RepairOutcome>,
) -> Result<(), RecoveryError> {
    let outcome = match forced {
        Some(value) => value,
        None => outcome(task)?,
    };
    let op = task
        .operation
        .as_ref()
        .ok_or_else(|| invalid("legacy publication lacks operation"))?;
    let plan = task
        .plan
        .as_ref()
        .ok_or_else(|| invalid("legacy publication lacks plan"))?;
    let id = task
        .knowledge_id
        .clone()
        .unwrap_or_else(|| format!("case-{}-{}", task.id, task.diagnosis_attempts));
    let case_id = format!("{}-{outcome:?}", op.operation_id);
    if result.deliveries.contains_key(&case_id) {
        return Ok(());
    }
    let observation = task
        .observation
        .as_ref()
        .ok_or_else(|| invalid("legacy publication lacks observation"))?;
    let mut conditions = observation.facts.clone();
    if task
        .problem
        .conditions
        .iter()
        .any(|(k, v)| conditions.get(k) != Some(v))
        || config
            .target
            .required_facts
            .iter()
            .any(|(k, v)| conditions.get(k) != Some(v))
    {
        return Err(invalid(
            "legacy publication environment does not match original target/incident",
        ));
    }
    conditions.insert("fault_fingerprint".into(), task.problem.fingerprint.clone());
    conditions.insert("platform".into(), config.target.platform.clone());
    let proposed = KnowledgeCandidate {
        id: id.clone(),
        incident_id: task.problem.incident_id.clone(),
        summary: plan.summary.clone(),
        keywords: task.problem.keywords.clone(),
        conditions,
        script: plan.script.clone(),
        reusable: plan.reusable && task.episode_count >= config.minimum_script_occurrences,
        evidence_refs: task.problem.evidence_refs.clone(),
        created_at_ms: task.created_at_ms,
    };
    let existing = knowledge.get(&id);
    let candidate = if task.reused_script {
        existing
            .as_ref()
            .ok_or_else(|| invalid("legacy reused publication candidate missing"))?
            .candidate
            .clone()
    } else {
        proposed
    };
    if existing
        .as_ref()
        .is_some_and(|record| record.candidate != candidate)
        || result
            .candidates
            .get(&id)
            .is_some_and(|old| old != &candidate)
    {
        return Err(invalid(
            "legacy publication candidate identity content conflict",
        ));
    }
    let refs = task
        .verification
        .as_ref()
        .map(|v| v.evidence_refs.clone())
        .unwrap_or_else(|| {
            task.receipt
                .as_ref()
                .map(|r| r.evidence_refs.clone())
                .unwrap_or_default()
        });
    let case = RepairCase {
        id: case_id.clone(),
        operation_id: op.operation_id.clone(),
        target_id: op.target.clone(),
        script_id: plan.script.id.clone(),
        script_version: plan.script.version,
        outcome,
        evidence_refs: refs.clone(),
        recorded_at_ms: task.updated_at_ms,
    };
    let verification = if outcome == RepairOutcome::Verified {
        let v = task
            .verification
            .as_ref()
            .ok_or_else(|| invalid("legacy business verification missing"))?;
        Some(BusinessVerificationRecord {
            operation_id: op.operation_id.clone(),
            target_id: op.target.clone(),
            script_id: plan.script.id.clone(),
            script_version: plan.script.version,
            verifier_id: format!("{}:{}", config.target.executor_id, v.profile),
            evidence_refs: refs,
            verified_at_ms: v.verified_at_ms,
        })
    } else {
        None
    };
    let known = existing
        .as_ref()
        .and_then(|r| r.cases.iter().find(|c| c.result.id == case_id));
    if let Some(known) = known
        && forced.is_none()
        && (known.result != case || known.verification != verification)
    {
        return Err(invalid(
            "legacy publication conflicts with committed knowledge case",
        ));
    }
    if let Some(known) = known
        && (known.result.operation_id != case.operation_id
            || known.result.target_id != case.target_id
            || known.result.script_id != case.script_id
            || known.result.script_version != case.script_version
            || known.result.outcome != case.outcome
            || (forced.is_some() && known.verification.is_some()))
    {
        return Err(invalid(
            "legacy known case does not bind original operation",
        ));
    }
    let (case, verification) = known
        .map(|known| (known.result.clone(), known.verification.clone()))
        .unwrap_or((case, verification));
    // Validate undelivered commands too; no successful assertion is installed by
    // a task label or snapshot alone.
    let mut scratch = KnowledgeState::new(KnowledgeConfig::default())?;
    let proposal = scratch.propose(
        "import-candidate",
        KnowledgeCommand::UpsertCandidate(candidate.clone()),
    )?;
    scratch = proposal.state().clone();
    let proof = verification
        .as_ref()
        .map(|v| {
            TrustedBusinessVerification::attest(
                &v.operation_id,
                &v.target_id,
                &v.script_id,
                v.script_version,
                &v.verifier_id,
                v.evidence_refs.clone(),
                v.verified_at_ms,
            )
        })
        .transpose()?;
    scratch.propose(
        "import-case",
        KnowledgeCommand::RecordOutcome {
            record_id: id.clone(),
            case: case.clone(),
            verification: proof,
        },
    )?;
    result.candidates.insert(id, candidate.clone());
    result.deliveries.insert(
        case_id.clone(),
        KnowledgeDelivery {
            id: case_id.clone(),
            created_revision: 1,
            candidate,
            case,
            verification,
            delivered: known.is_some(),
        },
    );
    result.delivery_order.insert(case_id, order);
    if outcome != RepairOutcome::Verified {
        result
            .quarantined
            .insert((plan.script.id.clone(), plan.script.version));
    }
    Ok(())
}
fn convert(task: &LegacyRecoveryTask, stage: RecoveryStage) -> RecoveryTask {
    RecoveryTask {
        id: task.id.clone(),
        revision: task.revision,
        problem: task.problem.clone(),
        episode_count: task.episode_count,
        stage,
        diagnosis_attempts: task.diagnosis_attempts,
        plan: task.plan.clone(),
        knowledge_id: task.knowledge_id.clone(),
        reused_script: task.reused_script,
        approval_id: task.approval_id.clone(),
        operation: task.operation.clone(),
        observation: task.observation.clone(),
        receipt: task.receipt.clone(),
        verification: task.verification.clone(),
        result_check: task.result_check.clone(),
        note: task.note.clone(),
        created_at_ms: task.created_at_ms,
        updated_at_ms: task.updated_at_ms,
        diagnosis_call: None,
    }
}
