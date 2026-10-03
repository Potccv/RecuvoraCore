use super::{EngineError, EngineResult, approvals::Approvals};
use crate::recovery::{approval::*, knowledge::*, workflow::*};
use crate::{
    binding,
    operation::{CommitReceipt, CommitRequest, Prepared},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    pub recovery: RecoveryConfig,
    pub approvals: ApprovalLimits,
    pub knowledge: KnowledgeConfig,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEntry {
    pub request: CommitRequest,
    pub now_ms: u64,
    pub command: serde_json::Value,
    pub approvals: Vec<ApprovalEntry>,
    pub workflow: Vec<RecoveryEntry>,
    pub experiences: Vec<ExperienceCommit>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceCommit {
    pub request: CommitRequest,
    pub record: RepairExperience,
}
/// A single recovery aggregate. Mutable internals and unconfirmed effects are private.
#[derive(Clone, Debug)]
pub struct RecoverySession {
    config: SessionConfig,
    workflow: RecoveryState,
    approvals: Approvals,
    knowledge: KnowledgeState,
    revision: u64,
    digest: String,
    commits: im::OrdSet<String>,
    updated_at_ms: u64,
    latest: Option<SessionEntry>,
    workflow_changes: Vec<RecoveryEntry>,
    experience_changes: Vec<ExperienceCommit>,
    recovery_required: bool,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum SessionCommand {
    Register {
        problem: ProblemContext,
        incident: IncidentEvidence,
    },
    Start {
        task_id: String,
        revision: u64,
        observation: TargetObservation,
    },
    Review {
        task_id: String,
        revision: u64,
        observation: Option<TargetObservation>,
    },
    Reviewed {
        task_id: String,
        attempt: ReviewAttempt,
        result: Result<ReviewOutput, String>,
    },
    HumanDecision {
        task_id: String,
        revision: u64,
        decision: ApprovalDecision,
        actor: String,
        reason: String,
    },
    Resume {
        task_id: String,
        revision: u64,
    },
    Authorize {
        task_id: String,
        revision: u64,
        observation: TargetObservation,
        incident: IncidentEvidence,
        authority: TargetAuthority,
    },
    Executed {
        task_id: String,
        #[serde(serialize_with = "serialize_permit")]
        permit: ExecutionPermit,
        result: Result<RepairReceipt, String>,
    },
    Verified {
        task_id: String,
        revision: u64,
        result: Result<BusinessVerification, String>,
    },
    CheckResult {
        task_id: String,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
    },
    Cancel {
        task_id: String,
        revision: u64,
    },
    PrepareAction {
        task_id: String,
        action: RepairArtifact,
    },
    BeginSummary {
        job_id: String,
    },
    Summarized {
        job_id: String,
        call_id: String,
        result: Result<ExperienceReport, String>,
    },
    Deliver,
    Recover,
}
#[derive(Clone, Debug)]
pub struct ReviewInput {
    pub request: ApprovalRequest,
    pub attempt: ReviewAttempt,
    pub observation: TargetObservation,
}
#[derive(Clone, Debug, Serialize)]
pub struct ReviewOutput {
    pub assessment: ModelAssessment,
    pub identity: ReviewerIdentity,
}
#[derive(Clone, Debug)]
pub struct VerificationInput {
    pub target: TargetBinding,
    pub operation: ProposedOperation,
    pub receipt: RepairReceipt,
}
#[derive(Debug)]
pub enum SessionEffect {
    Review(Box<ReviewInput>),
    Execute {
        permit: ExecutionPermit,
        timeout_secs: u64,
    },
    Summarize {
        job: Box<ExperienceJob>,
        call_id: String,
    },
}

impl RecoverySession {
    pub fn new(config: SessionConfig) -> EngineResult<Self> {
        let workflow = RecoveryState::new(config.recovery.clone())?;
        let approvals = Approvals::new(config.approvals.clone())?;
        let knowledge = KnowledgeState::new(config.knowledge.clone())?;
        Ok(Self {
            digest: binding::digest(&("recovery-session-v1", &config)),
            config,
            workflow,
            approvals,
            knowledge,
            revision: 0,
            commits: im::OrdSet::new(),
            updated_at_ms: 0,
            latest: None,
            workflow_changes: Vec::new(),
            experience_changes: Vec::new(),
            recovery_required: false,
        })
    }
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn latest_entry(&self) -> Option<&SessionEntry> {
        self.latest.as_ref()
    }
    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }
    pub fn task(&self, id: &str) -> Option<&RecoveryTask> {
        self.workflow.task(id)
    }
    pub fn tasks(&self) -> impl Iterator<Item = &RecoveryTask> {
        self.workflow.tasks()
    }
    pub fn approval(&self, id: &str) -> Option<&ApprovalRecord> {
        self.task(id)?
            .approval_id
            .as_ref()
            .and_then(|id| self.approvals.get(id))
    }
    pub fn approval_count(&self) -> usize {
        self.approvals.list().len()
    }
    pub fn pending_experiences(&self) -> Vec<ExperienceJob> {
        self.workflow.pending_experiences()
    }
    pub fn experiences(&self, query: &KnowledgeQuery) -> EngineResult<Vec<RepairExperience>> {
        Ok(self.knowledge.search_experiences(query)?)
    }
    pub fn knowledge_snapshot(&self) -> KnowledgeSnapshot {
        self.knowledge.snapshot()
    }
    pub fn is_quarantined(&self, id: &str, version: u64) -> bool {
        self.workflow.is_quarantined(id, version) || self.knowledge.is_quarantined(id, version)
    }
    pub fn repair_action(&self, operation_id: &str) -> Option<&RepairArtifact> {
        self.workflow.repair_action(operation_id)
    }
    pub fn releasable(&self) -> bool {
        self.workflow.tasks().all(|t| t.stage.terminal())
            && !self
                .approvals
                .list()
                .iter()
                .any(|r| matches!(r.state, ApprovalState::Executing | ApprovalState::Unknown))
    }
    fn current(&self, id: &str) -> EngineResult<RecoveryTask> {
        self.task(id)
            .cloned()
            .ok_or_else(|| EngineError::Invalid("task not found".into()))
    }
    fn record(&self, id: &str) -> EngineResult<ApprovalRecord> {
        self.approval(id)
            .cloned()
            .ok_or_else(|| EngineError::Invalid("approval not found".into()))
    }
    pub fn validate_dispatch(
        &self,
        id: &str,
        incident: &IncidentEvidence,
        now: u64,
    ) -> EngineResult<()> {
        let task = self.current(id)?;
        let approval = self.record(id)?;
        if task.stage != RecoveryStage::Executing
            || approval.state != ApprovalState::Executing
            || now / 1000 >= approval.request.expires_at
            || incident.incident_id != task.problem.incident_id
            || incident.revision < task.problem.incident_revision
        {
            return Err(EngineError::Conflict);
        }
        if let Some(action) = task
            .operation
            .as_ref()
            .and_then(|op| self.repair_action(&op.operation_id))
        {
            self.knowledge.validate_artifact(action)?;
            if self.workflow.is_quarantined(&action.id, action.version)
                || self.knowledge.is_quarantined(&action.id, action.version)
            {
                return Err(EngineError::Invalid(
                    "action quarantined before send".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn prepare(
        &self,
        id: String,
        command: SessionCommand,
        now: u64,
    ) -> EngineResult<Prepared<Self, SessionEffect>> {
        if self.commits.contains(&id) || now < self.updated_at_ms {
            return Err(EngineError::Conflict);
        }
        if self.recovery_required && !matches!(&command, SessionCommand::Recover) {
            return Err(EngineError::Invalid(
                "explicit recovery commit required".into(),
            ));
        }
        let command_input = serde_json::to_value(&command)?;
        let mut next = self.clone();
        next.approvals.entries.clear();
        next.workflow_changes.clear();
        next.experience_changes.clear();
        let effects = next.apply(command, now)?;
        let input = serde_json::to_value((
            &self.digest,
            &self.config,
            now,
            &command_input,
            &next.approvals.entries,
            &next.workflow_changes,
            &next.experience_changes,
        ))?;
        let request = CommitRequest::new(
            id.clone(),
            self.revision,
            "recovery-session".into(),
            input.clone(),
        )?;
        next.revision = request.revision;
        next.updated_at_ms = now;
        next.digest = binding::digest(&request);
        next.commits.insert(id.clone());
        next.latest = Some(SessionEntry {
            request,
            now_ms: now,
            command: command_input,
            approvals: std::mem::take(&mut next.approvals.entries),
            workflow: std::mem::take(&mut next.workflow_changes),
            experiences: std::mem::take(&mut next.experience_changes),
        });
        Ok(Prepared::new_bound(
            id,
            self.revision,
            "recovery-session".into(),
            input,
            next,
            effects,
        )?)
    }
    pub fn restore(config: SessionConfig, entries: &[SessionEntry]) -> EngineResult<Self> {
        let mut state = Self::new(config.clone())?;
        let mut approvals = Vec::new();
        let mut workflow = Vec::new();
        let mut knowledge = Vec::new();
        for entry in entries {
            if state.commits.contains(&entry.request.id) || entry.now_ms < state.updated_at_ms {
                return Err(EngineError::Conflict);
            }
            let input = serde_json::to_value((
                &state.digest,
                &config,
                entry.now_ms,
                &entry.command,
                &entry.approvals,
                &entry.workflow,
                &entry.experiences,
            ))?;
            let expected = CommitRequest::new(
                entry.request.id.clone(),
                state.revision,
                "recovery-session".into(),
                input,
            )?;
            if expected != entry.request {
                return Err(EngineError::Invalid(
                    "session history binding differs".into(),
                ));
            }
            approvals.extend(entry.approvals.clone());
            workflow.extend(entry.workflow.clone());
            for item in &entry.experiences {
                knowledge.push(KnowledgeReplayEntry {
                    request: item.request.clone(),
                    command: KnowledgeCommand::RecordExperience(TrustedRepairExperience::attest(
                        item.record.clone(),
                    )?),
                    receipt: CommitReceipt::confirmed(&item.request),
                });
            }
            state.revision = expected.revision;
            state.digest = binding::digest(&expected);
            state.updated_at_ms = entry.now_ms;
            state.commits.insert(expected.id);
            state.latest = Some(entry.clone());
        }
        state.approvals.ledger = ApprovalLedger::restore(config.approvals, approvals)?;
        if !workflow.is_empty() {
            state.workflow = RecoveryState::restore(config.recovery, workflow)?;
        }
        state.knowledge = KnowledgeState::replay(config.knowledge, knowledge)?;
        state.recovery_required = !entries.is_empty();
        Ok(state)
    }
    fn transition(
        &mut self,
        command: RecoveryCommand,
        now: u64,
    ) -> EngineResult<Vec<RecoveryEffect>> {
        let pending = self.workflow.prepare(
            format!("workflow-{}", self.workflow.revision()),
            command,
            now,
            &self.knowledge,
        )?;
        self.workflow_changes.push(
            pending
                .state()
                .latest_entry()
                .ok_or(EngineError::Conflict)?
                .clone(),
        );
        let receipt = CommitReceipt::confirmed(pending.request());
        let committed = pending.confirm(receipt)?;
        self.workflow = committed.state;
        Ok(committed.effects)
    }
    fn event(&mut self, event: RecoveryEvent, now: u64) -> EngineResult<Vec<RecoveryEffect>> {
        self.transition(RecoveryCommand::Event(event), now)
    }
    fn attach(&mut self, id: &str, now: u64) -> EngineResult<()> {
        let task = self.current(id)?;
        if task.approval_id.is_some() {
            return Ok(());
        }
        let op = task.operation.clone().ok_or(EngineError::Conflict)?;
        let record =
            self.approvals
                .request(op, self.config.recovery.approval.clone(), now / 1000)?;
        self.event(
            RecoveryEvent::ApprovalAttached {
                task_id: task.id,
                revision: task.revision,
                record,
            },
            now,
        )?;
        Ok(())
    }
    fn resolve(&mut self, id: &str, now: u64) -> EngineResult<()> {
        let task = self.current(id)?;
        let record = self.record(id)?;
        if task.stage == RecoveryStage::AwaitingApproval
            && !matches!(
                record.state,
                ApprovalState::Pending
                    | ApprovalState::WaitingHuman
                    | ApprovalState::Approved
                    | ApprovalState::Executing
            )
        {
            self.event(
                RecoveryEvent::ApprovalResolved {
                    task_id: task.id,
                    revision: task.revision,
                    record,
                },
                now,
            )?;
        }
        Ok(())
    }
    fn reconcile(
        &mut self,
        id: &str,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
        now: u64,
    ) -> EngineResult<()> {
        let approval = self.record(id)?;
        self.event(
            RecoveryEvent::ResultChecked {
                task_id: id.into(),
                revision,
                execution: execution.clone(),
                verification: verification.clone(),
                actor: actor.clone(),
                approval: approval.clone(),
            },
            now,
        )?;
        if approval.state == ApprovalState::Unknown
            && execution.outcome != CheckedExecution::Unknown
        {
            let outcome = if execution.outcome == CheckedExecution::Executed {
                ExecutionOutcome::Executed
            } else {
                ExecutionOutcome::Failed
            };
            let approval = self.approvals.reconcile_unknown(
                &approval.request.request_id,
                outcome,
                "independent execution evidence".into(),
                actor.clone(),
                now / 1000,
            )?;
            let revision = self.current(id)?.revision;
            self.event(
                RecoveryEvent::ResultChecked {
                    task_id: id.into(),
                    revision,
                    execution,
                    verification,
                    actor,
                    approval,
                },
                now,
            )?;
        }
        Ok(())
    }
    fn apply(&mut self, command: SessionCommand, now: u64) -> EngineResult<Vec<SessionEffect>> {
        let mut effects = Vec::new();
        match command {
            SessionCommand::Register { problem, incident } => {
                self.event(RecoveryEvent::Register { problem, incident }, now)?;
            }
            SessionCommand::Start {
                task_id,
                revision,
                observation,
            } => {
                self.transition(
                    RecoveryCommand::StartRepair {
                        task_id: task_id.clone(),
                        revision,
                        observation,
                    },
                    now,
                )?;
                self.attach(&task_id, now)?;
            }
            SessionCommand::Review {
                task_id,
                revision,
                observation,
            } => {
                let task = self.current(&task_id)?;
                if task.revision != revision || task.stage != RecoveryStage::AwaitingApproval {
                    return Err(EngineError::Conflict);
                }
                let record = self.record(&task_id)?;
                if now / 1000 >= record.request.expires_at {
                    self.approvals
                        .expire(&record.request.request_id, now / 1000)?;
                    self.resolve(&task_id, now)?;
                } else if record.review_stage == ReviewStage::ReviewingHarness {
                    if record.review_deadline.is_some_and(|d| now / 1000 >= d) {
                        self.approvals.expire_review(
                            &record.request.request_id,
                            record.revision,
                            &record.request.policy,
                            now / 1000,
                        )?;
                    }
                } else if !(matches!(record.request.policy.reviewer, ReviewerConfig::Human)
                    || record.review_stage == ReviewStage::NeedsHuman
                    || (record.review_stage == ReviewStage::WaitingHuman
                        && record.human_deadline.is_some_and(|d| now / 1000 < d)))
                {
                    let observed = observation.as_ref().ok_or_else(|| {
                        EngineError::Invalid("review observation required".into())
                    })?;
                    self.workflow.observe(observed, now)?;
                    self.workflow.conditions(&task, observed)?;
                    let timeout = match record.request.policy.reviewer {
                        ReviewerConfig::HumanThenHarness {
                            review_timeout_secs,
                            ..
                        } => self
                            .config
                            .recovery
                            .review_timeout_secs
                            .min(review_timeout_secs),
                        _ => self.config.recovery.review_timeout_secs,
                    };
                    let attempt = self.approvals.begin_harness_review(
                        &record.request.request_id,
                        record.revision,
                        &record.request.policy,
                        timeout,
                        now / 1000,
                    )?;
                    effects.push(SessionEffect::Review(Box::new(ReviewInput {
                        request: record.request,
                        attempt,
                        observation: observation.ok_or_else(|| {
                            EngineError::Invalid("review observation required".into())
                        })?,
                    })));
                }
            }
            SessionCommand::Reviewed {
                task_id,
                attempt,
                result,
            } => {
                let record = self.record(&task_id)?;
                if attempt.request_id != record.request.request_id {
                    return Err(EngineError::Conflict);
                }
                let saved = match result {
                    Ok(output) => self.approvals.assess_attempt(
                        &attempt,
                        output.assessment,
                        output.identity,
                        &record.request.policy,
                        now / 1000,
                    ),
                    Err(reason) => self.approvals.fail_review_attempt(
                        &attempt,
                        reason,
                        &record.request.policy,
                        now / 1000,
                    ),
                };
                if let Err(error) = saved
                    && self
                        .approvals
                        .get(&attempt.request_id)
                        .is_some_and(|r| r.active_review_attempt().as_ref() == Some(&attempt))
                {
                    self.approvals.fail_review_attempt(
                        &attempt,
                        error.to_string(),
                        &record.request.policy,
                        now / 1000,
                    )?;
                }
                self.resolve(&task_id, now)?;
            }
            SessionCommand::HumanDecision {
                task_id,
                revision,
                decision,
                actor,
                reason,
            } => {
                let record = self.record(&task_id)?;
                self.approvals.decide_human_at_revision(
                    &record.request.request_id,
                    revision,
                    ApprovalAssessment {
                        decision,
                        reason,
                        reviewer: AssessmentSource::Human { actor },
                    },
                    &record.request.policy,
                    now / 1000,
                )?;
                self.resolve(&task_id, now)?;
            }
            SessionCommand::Resume { task_id, revision } => {
                self.event(
                    RecoveryEvent::Resume {
                        task_id: task_id.clone(),
                        revision,
                    },
                    now,
                )?;
                self.attach(&task_id, now)?;
                self.resolve(&task_id, now)?;
            }
            SessionCommand::Authorize {
                task_id,
                revision,
                observation,
                incident,
                authority,
            } => {
                let task = self.current(&task_id)?;
                if task.revision != revision {
                    return Err(EngineError::Conflict);
                }
                let record = self.record(&task_id)?;
                let permit = self.approvals.consume(
                    &record.request.request_id,
                    task.operation.as_ref().ok_or(EngineError::Conflict)?,
                    &record.request.policy,
                    now / 1000,
                )?;
                let approval = self.record(&task_id)?;
                let results = self.transition(
                    RecoveryCommand::AuthorizeExecution {
                        task_id,
                        revision,
                        observation,
                        incident,
                        authority,
                        permit,
                        approval,
                    },
                    now,
                )?;
                for effect in results {
                    if let RecoveryEffect::Execute {
                        permit,
                        timeout_secs,
                        ..
                    } = effect
                    {
                        effects.push(SessionEffect::Execute {
                            permit,
                            timeout_secs,
                        });
                    }
                }
            }
            SessionCommand::Executed {
                task_id,
                permit,
                result,
            } => {
                let operation = permit.operation();
                let trace: Vec<_> = self
                    .repair_action(&operation.operation_id)
                    .cloned()
                    .into_iter()
                    .collect();
                let receipt = match result {
                    Ok(receipt)
                        if receipt.operation_id == operation.operation_id
                            && receipt.target_id == operation.target
                            && receipt.execution_trace == trace
                            && (receipt.outcome == RepairExecutionOutcome::Unknown
                                || receipt.executor_stopped)
                            && !receipt.evidence_refs.is_empty()
                            && receipt.evidence_refs.len() <= 32
                            && receipt.summary.len() <= 8192 =>
                    {
                        receipt
                    }
                    result => RepairReceipt {
                        execution_trace: trace,
                        operation_id: operation.operation_id.clone(),
                        target_id: operation.target.clone(),
                        outcome: RepairExecutionOutcome::Unknown,
                        executor_stopped: false,
                        evidence_refs: vec![format!("approval:{}", permit.request_id())],
                        summary: result
                            .err()
                            .unwrap_or_else(|| "invalid or unbound executor receipt".into()),
                    },
                };
                let outcome = match receipt.outcome {
                    RepairExecutionOutcome::Executed => ExecutionOutcome::Executed,
                    RepairExecutionOutcome::Failed => ExecutionOutcome::Failed,
                    RepairExecutionOutcome::Unknown => ExecutionOutcome::Unknown,
                };
                let approval = self.approvals.complete(
                    permit,
                    outcome,
                    receipt.summary.clone(),
                    now / 1000,
                )?;
                let revision = self.current(&task_id)?.revision;
                self.event(
                    RecoveryEvent::ExecutionRecorded {
                        task_id,
                        revision,
                        receipt,
                        approval,
                    },
                    now,
                )?;
            }
            SessionCommand::Verified {
                task_id,
                revision,
                result,
            } => {
                let event = match result {
                    Ok(verification) => RecoveryEvent::VerificationRecorded {
                        task_id,
                        revision,
                        verification,
                    },
                    Err(reason) => RecoveryEvent::VerificationUnavailable {
                        task_id,
                        revision,
                        reason,
                    },
                };
                self.event(event, now)?;
            }
            SessionCommand::CheckResult {
                task_id,
                revision,
                execution,
                verification,
                actor,
            } => {
                self.reconcile(&task_id, revision, execution, verification, actor, now)?;
            }
            SessionCommand::Cancel { task_id, revision } => {
                let task = self.current(&task_id)?;
                if task.revision != revision {
                    return Err(EngineError::Conflict);
                }
                let approval = if let Some(id) = task.approval_id {
                    Some(self.approvals.cancel(
                        &id,
                        "cancel requested before execution".into(),
                        now / 1000,
                    )?)
                } else {
                    None
                };
                self.event(
                    RecoveryEvent::Cancel {
                        task_id,
                        revision,
                        approval,
                    },
                    now,
                )?;
            }
            SessionCommand::PrepareAction { task_id, action } => {
                let revision = self.current(&task_id)?.revision;
                self.event(
                    RecoveryEvent::RepairActionPrepared {
                        task_id,
                        revision,
                        action,
                    },
                    now,
                )?;
            }
            SessionCommand::BeginSummary { job_id } => {
                for effect in self.event(RecoveryEvent::BeginExperience { job_id }, now)? {
                    if let RecoveryEffect::SummarizeExperience { job, call_id } = effect {
                        effects.push(SessionEffect::Summarize { job, call_id });
                    }
                }
            }
            SessionCommand::Summarized {
                job_id,
                call_id,
                result,
            } => {
                let event = match result {
                    Ok(report) => RecoveryEvent::ExperienceSummarized {
                        job_id: job_id.clone(),
                        call_id: call_id.clone(),
                        report,
                    },
                    Err(reason) => RecoveryEvent::ExperienceFailed {
                        job_id: job_id.clone(),
                        call_id: call_id.clone(),
                        reason,
                    },
                };
                if let Err(error) = self.event(event, now) {
                    self.event(
                        RecoveryEvent::ExperienceFailed {
                            job_id,
                            call_id,
                            reason: super::runner::bounded_reason(&error),
                        },
                        now,
                    )?;
                }
            }
            SessionCommand::Deliver => {
                for job in self
                    .pending_experiences()
                    .into_iter()
                    .filter(|j| j.report.is_some())
                {
                    let record = job.record()?;
                    let command = KnowledgeCommand::RecordExperience(
                        TrustedRepairExperience::attest(record.clone())?,
                    );
                    let pending = self
                        .knowledge
                        .propose(format!("knowledge-{}", self.knowledge.revision()), command)?;
                    self.experience_changes.push(ExperienceCommit {
                        request: pending.request().clone(),
                        record,
                    });
                    let receipt = CommitReceipt::confirmed(pending.request());
                    self.knowledge = pending.confirm(receipt)?.state;
                    self.event(RecoveryEvent::ExperienceDelivered { job_id: job.id }, now)?;
                }
            }
            SessionCommand::Recover => {
                self.approvals.recover(now / 1000)?;
                if self.workflow.recovery_required() {
                    self.event(RecoveryEvent::Recover, now)?;
                }
                self.recovery_required = false;
            }
        }
        Ok(effects)
    }
}

fn serialize_permit<S: serde::Serializer>(
    permit: &ExecutionPermit,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    (permit.request_id(), permit.revision(), permit.operation()).serialize(serializer)
}
