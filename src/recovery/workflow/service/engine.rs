//! Deterministic workflow. All I/O, clocks, scheduling and commit CAS belong to Host.
use super::contract::{evidence, facts};
use super::*;
use crate::operation::{CommitRequest, Prepared};
use approval::{ApprovalRecord, ApprovalState, ExecutionPermit};

/// Host must hold this logical ownership epoch through commit and dispatch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetAuthority {
    pub target_id: String,
    pub epoch: String,
}

/// Current trusted incident fact. Host must check its revision atomically with
/// execution authorization, or hold its incident gate through that commit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentEvidence {
    pub incident_id: String,
    pub revision: u64,
    pub active: bool,
}

/// Stable, independently retryable experience delivery. Persist with task result.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeDelivery {
    pub id: String,
    pub created_revision: u64,
    pub candidate: KnowledgeCandidate,
    pub case: RepairCase,
    pub verification: Option<BusinessVerificationRecord>,
    pub delivered: bool,
}

/// Data Host persists. Loading it never dispatches an external operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryEvent {
    Register {
        problem: ProblemContext,
        incident: IncidentEvidence,
    },
    SelectPlan {
        task_id: String,
        revision: u64,
        observation: TargetObservation,
        candidate: Option<KnowledgeRecord>,
    },
    RetryDiagnosis {
        task_id: String,
        revision: u64,
        observation: TargetObservation,
    },
    DiagnosisCompleted {
        task_id: String,
        revision: u64,
        call_id: String,
        plan: RepairPlan,
    },
    DiagnosisFailed {
        task_id: String,
        revision: u64,
        call_id: String,
        reason: String,
    },
    ApprovalAttached {
        task_id: String,
        revision: u64,
        record: ApprovalRecord,
    },
    ApprovalResolved {
        task_id: String,
        revision: u64,
        record: ApprovalRecord,
    },
    /// Only replay accepts this directly; live preparation requires an owned permit.
    ExecutionAuthorized {
        task_id: String,
        revision: u64,
        approval: ApprovalRecord,
        observation: TargetObservation,
        incident: IncidentEvidence,
        authority: TargetAuthority,
    },
    ExecutionRecorded {
        task_id: String,
        revision: u64,
        receipt: ScriptReceipt,
        approval: ApprovalRecord,
    },
    VerificationRecorded {
        task_id: String,
        revision: u64,
        verification: BusinessVerification,
    },
    VerificationUnavailable {
        task_id: String,
        revision: u64,
        reason: String,
    },
    ResultChecked {
        task_id: String,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
        approval: ApprovalRecord,
    },
    Resume {
        task_id: String,
        revision: u64,
    },
    Cancel {
        task_id: String,
        revision: u64,
        approval: Option<ApprovalRecord>,
    },
    Recover,
    DeliveryConfirmed {
        delivery_id: String,
    },
}

pub enum RecoveryCommand {
    Event(RecoveryEvent),
    AuthorizeExecution {
        task_id: String,
        revision: u64,
        permit: ExecutionPermit,
        approval: ApprovalRecord,
        observation: TargetObservation,
        incident: IncidentEvidence,
        authority: TargetAuthority,
    },
}

/// Effects are available only after confirming the whole workflow commit.
#[derive(Debug)]
pub enum RecoveryEffect {
    Diagnose {
        task: Box<RecoveryTask>,
        call_id: String,
        timeout_secs: u64,
        max_tool_calls: usize,
    },
    RequestApproval {
        operation: approval::ProposedOperation,
        policy: approval::ApprovalPolicy,
    },
    Execute {
        permit: ExecutionPermit,
        authority: TargetAuthority,
        timeout_secs: u64,
    },
    Verify {
        task_id: String,
        operation: approval::ProposedOperation,
        receipt: ScriptReceipt,
    },
    DeliverKnowledge(Box<KnowledgeDelivery>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEntry {
    pub request: CommitRequest,
    pub now_ms: u64,
    pub event: RecoveryEvent,
}

/// Authority reconstructed through the same validated transitions used live.
/// No unchecked Deserialize implementation and no ambient clock or I/O.
#[derive(Clone, Debug)]
pub struct RecoveryState {
    config: RecoveryConfig,
    revision: u64,
    updated_at_ms: u64,
    tasks: BTreeMap<String, RecoveryTask>,
    deliveries: BTreeMap<String, KnowledgeDelivery>,
    quarantined: BTreeSet<(String, u64)>,
    scripts: BTreeMap<(String, u64), ScriptArtifact>,
    candidates: BTreeMap<String, KnowledgeCandidate>,
    entries: Vec<RecoveryEntry>,
    recovery_required: bool,
}

impl RecoveryState {
    pub fn new(config: RecoveryConfig) -> Result<Self, RecoveryError> {
        config.validate()?;
        Ok(Self {
            config,
            revision: 0,
            updated_at_ms: 0,
            tasks: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            quarantined: BTreeSet::new(),
            scripts: BTreeMap::new(),
            candidates: BTreeMap::new(),
            entries: Vec::new(),
            recovery_required: false,
        })
    }
    pub fn tasks(&self) -> impl Iterator<Item = &RecoveryTask> {
        self.tasks.values()
    }
    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }
    pub fn config(&self) -> &RecoveryConfig {
        &self.config
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn entries(&self) -> &[RecoveryEntry] {
        &self.entries
    }
    pub fn task(&self, id: &str) -> Option<&RecoveryTask> {
        self.tasks.get(id)
    }
    pub fn pending_deliveries(&self) -> Vec<KnowledgeDelivery> {
        let mut deliveries: Vec<_> = self
            .deliveries
            .values()
            .filter(|d| !d.delivered)
            .cloned()
            .collect();
        deliveries.sort_by_key(|delivery| (delivery.created_revision, delivery.id.clone()));
        deliveries
    }
    pub fn is_quarantined(&self, id: &str, version: u64) -> bool {
        self.quarantined.contains(&(id.into(), version))
    }

    /// Host persists the new entry and compares request.expected_revision, then
    /// confirms. Commit failure/uncertainty grants no effect; reload before retry.
    pub fn prepare(
        &self,
        commit_id: impl Into<String>,
        command: RecoveryCommand,
        now_ms: u64,
        knowledge: &KnowledgeState,
    ) -> Result<Prepared<Self, RecoveryEffect>, RecoveryError> {
        if self.recovery_required
            && !matches!(&command, RecoveryCommand::Event(RecoveryEvent::Recover))
        {
            return Err(invalid(
                "restored workflow requires a committed recovery transition",
            ));
        }
        let (event, permit) = match command {
            RecoveryCommand::Event(event) => {
                if matches!(event, RecoveryEvent::ExecutionAuthorized { .. }) {
                    return Err(invalid("execution requires an owned committed permit"));
                }
                (event, None)
            }
            RecoveryCommand::AuthorizeExecution {
                task_id,
                revision,
                permit,
                approval,
                observation,
                incident,
                authority,
            } => {
                if permit.request_id() != approval.request.request_id
                    || permit.operation() != &approval.request.operation
                    || permit.revision() != approval.revision
                {
                    return Err(invalid("permit and durable approval differ"));
                }
                (
                    RecoveryEvent::ExecutionAuthorized {
                        task_id,
                        revision,
                        approval,
                        observation,
                        incident,
                        authority,
                    },
                    Some(permit),
                )
            }
        };
        // Current knowledge authority is checked live; historical replay validates
        // the original evidence without retroactively applying newer isolation.
        match &event {
            RecoveryEvent::SelectPlan {
                task_id,
                observation,
                candidate: Some(record),
                ..
            } => {
                let task = self
                    .tasks
                    .get(task_id)
                    .ok_or_else(|| invalid("task not found"))?;
                let matches = knowledge.search_reusable(&KnowledgeQuery {
                    conditions: self.conditions(task, observation)?,
                    keywords: task.problem.keywords.clone(),
                    limit: 100,
                })?;
                if !matches.iter().any(|item| item == record) {
                    return Err(invalid("candidate is not current reusable knowledge"));
                }
            }
            RecoveryEvent::DiagnosisCompleted { plan, .. } => {
                knowledge.validate_script(&plan.script)?
            }
            RecoveryEvent::ExecutionAuthorized { task_id, .. } => {
                let plan = self
                    .tasks
                    .get(task_id)
                    .and_then(|t| t.plan.as_ref())
                    .ok_or_else(|| invalid("missing plan"))?;
                knowledge.validate_script(&plan.script)?;
                if knowledge.is_quarantined(&plan.script.id, plan.script.version) {
                    return Err(invalid("script version quarantined"));
                }
            }
            _ => {}
        }
        let id = commit_id.into();
        if self.entries.iter().any(|entry| entry.request.id == id) {
            return Err(invalid(
                "commit identity already applied; read prior result",
            ));
        }
        let input = self.commit_input(&event, now_ms)?;
        let mut next = self.clone();
        let effects = next.apply(&event, now_ms, permit)?;
        next.revision = self
            .revision
            .checked_add(1)
            .ok_or(RecoveryError::Capacity)?;
        next.updated_at_ms = now_ms;
        let request =
            CommitRequest::new(id.clone(), self.revision, "recovery".into(), input.clone())?;
        next.entries.push(RecoveryEntry {
            request,
            now_ms,
            event,
        });
        Ok(Prepared::new_bound(
            id,
            self.revision,
            "recovery".into(),
            input,
            next,
            effects,
        )?)
    }

    /// Host supplies complete, committed history from protected storage. Replay
    /// never returns effects/permits. Persist Recover before resuming any work.
    pub fn restore(
        config: RecoveryConfig,
        entries: &[RecoveryEntry],
    ) -> Result<Self, RecoveryError> {
        let mut state = Self::new(config)?;
        let mut ids = BTreeSet::new();
        for entry in entries {
            if !crate::identity::valid_id(&entry.request.id)
                || !ids.insert(entry.request.id.clone())
                || entry.request.expected_revision != state.revision
                || Some(entry.request.revision) != state.revision.checked_add(1)
            {
                return Err(invalid("invalid recovery history order or identity"));
            }
            let expected = CommitRequest::new(
                entry.request.id.clone(),
                state.revision,
                "recovery".into(),
                state.commit_input(&entry.event, entry.now_ms)?,
            )?;
            if entry.request != expected {
                return Err(invalid("recovery commit content differs from history"));
            }
            state.apply(&entry.event, entry.now_ms, None)?;
            state.revision = entry.request.revision;
            state.updated_at_ms = entry.now_ms;
            state.entries.push(entry.clone());
        }
        state.recovery_required = true;
        Ok(state)
    }

    fn commit_input(
        &self,
        event: &RecoveryEvent,
        now: u64,
    ) -> Result<serde_json::Value, RecoveryError> {
        serde_json::to_value((
            &self.config,
            self.revision,
            self.updated_at_ms,
            &self.tasks,
            &self.deliveries,
            &self.quarantined,
            self.scripts.values().collect::<Vec<_>>(),
            &self.candidates,
            event,
            now,
        ))
        .map_err(|_| invalid("cannot encode bounded domain commit"))
    }
    fn mark_unknown(
        &mut self,
        task: &mut RecoveryTask,
        now: u64,
    ) -> Result<Option<RecoveryEffect>, RecoveryError> {
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing original operation"))?;
        let delivery_id = format!("{}-Unknown", op.operation_id);
        if task.receipt.is_none() {
            task.receipt = Some(ScriptReceipt {
                operation_id: op.operation_id.clone(),
                target_id: op.target.clone(),
                outcome: ScriptOutcome::Unknown,
                executor_stopped: false,
                evidence_refs: vec![format!(
                    "approval:{}",
                    task.approval_id.as_deref().unwrap_or("unknown")
                )],
                summary: "interrupted or consumed execution; independent evidence required".into(),
            });
        }
        task.stage = RecoveryStage::Unknown;
        if self.deliveries.contains_key(&delivery_id) {
            return Ok(None);
        }
        Ok(Some(self.finish(task, RepairOutcome::Unknown, now)?))
    }
    fn current(&self, id: &str, revision: u64) -> Result<RecoveryTask, RecoveryError> {
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| invalid("task not found"))?;
        if task.revision != revision {
            return Err(RecoveryError::Conflict);
        }
        Ok(task.clone())
    }
    fn save_task(&mut self, mut task: RecoveryTask, now: u64) -> Result<(), RecoveryError> {
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or(RecoveryError::Capacity)?;
        task.updated_at_ms = now;
        self.tasks.insert(task.id.clone(), task);
        Ok(())
    }
    fn observe(&self, observed: &TargetObservation, now: u64) -> Result<(), RecoveryError> {
        facts(&observed.facts)?;
        evidence(&observed.evidence_refs)?;
        if observed.target_id != self.config.target.target_id
            || observed.observed_at_ms > now
            || now - observed.observed_at_ms > 30_000
            || self
                .config
                .target
                .required_facts
                .iter()
                .any(|(k, v)| observed.facts.get(k) != Some(v))
        {
            return Err(invalid("stale observation or target conditions changed"));
        }
        Ok(())
    }
    fn conditions(
        &self,
        task: &RecoveryTask,
        observed: &TargetObservation,
    ) -> Result<BTreeMap<String, String>, RecoveryError> {
        let mut values = observed.facts.clone();
        if task
            .problem
            .conditions
            .iter()
            .any(|(k, v)| values.get(k) != Some(v))
        {
            return Err(invalid("incident environment changed"));
        }
        values.insert("fault_fingerprint".into(), task.problem.fingerprint.clone());
        values.insert("platform".into(), self.config.target.platform.clone());
        facts(&values)?;
        Ok(values)
    }
    fn plan(
        &mut self,
        plan: &RepairPlan,
        observation: &TargetObservation,
    ) -> Result<(), RecoveryError> {
        text(&plan.summary, 4096)?;
        let script = &plan.script;
        // Reuse the knowledge domain's bounded immutable script validator.
        KnowledgeState::new(KnowledgeConfig::default())?.validate_script(script)?;
        if script.platform != self.config.target.platform
            || !self
                .config
                .target
                .allowed_languages
                .contains(&script.language)
            || script
                .preconditions
                .iter()
                .any(|(k, v)| observation.facts.get(k) != Some(v))
            || self.is_quarantined(&script.id, script.version)
        {
            return Err(invalid("script conditions or version not eligible"));
        }
        let key = (script.id.clone(), script.version);
        if self.scripts.get(&key).is_some_and(|old| old != script) {
            return Err(invalid("script version content changed"));
        }
        self.scripts.insert(key, script.clone());
        Ok(())
    }
    fn policy(&self, task: &RecoveryTask) -> &approval::ApprovalPolicy {
        if task.reused_script {
            &self.config.script_approval
        } else {
            &self.config.approval
        }
    }
    fn bind_approval(
        &self,
        task: &RecoveryTask,
        record: &ApprovalRecord,
    ) -> Result<(), RecoveryError> {
        if task.operation.as_ref() != Some(&record.request.operation)
            || self.policy(task) != &record.request.policy
            || task
                .approval_id
                .as_ref()
                .is_some_and(|id| id != &record.request.request_id)
        {
            return Err(invalid(
                "approval does not bind original intent and current policy",
            ));
        }
        record.request.operation.validate()?;
        record.request.policy.validate()?;
        if !record.request.policy.allows(&record.request.operation) {
            return Err(invalid("operation outside hard policy"));
        }
        Ok(())
    }
    fn request_plan(&self, task: &mut RecoveryTask) -> Result<RecoveryEffect, RecoveryError> {
        let plan = task.plan.as_ref().ok_or_else(|| invalid("missing plan"))?;
        let operation = approval::ProposedOperation {
            task_id: task.id.clone(),
            task_revision: task.revision,
            operation_id: format!(
                "{}-{}-{}",
                task.id,
                task.diagnosis_attempts,
                if task.reused_script {
                    "reuse"
                } else {
                    "repair"
                }
            ),
            target: task.problem.target_id.clone(),
            action: serde_json::json!({"kind":"execute_script", "executor_id":self.config.target.executor_id, "script":plan.script, "verification_profile":self.config.target.verification_profile, "required_facts":self.config.target.required_facts, "timeout_secs":self.config.target.action_timeout_secs, "incident_id":task.problem.incident_id, "incident_revision":task.problem.incident_revision}),
        };
        operation.validate()?;
        task.operation = Some(operation.clone());
        task.approval_id = None;
        task.result_check = None;
        task.stage = RecoveryStage::AwaitingApproval;
        Ok(RecoveryEffect::RequestApproval {
            operation,
            policy: self.policy(task).clone(),
        })
    }
    fn diagnose(
        &self,
        task: &mut RecoveryTask,
        now: u64,
    ) -> Result<Option<RecoveryEffect>, RecoveryError> {
        if task.diagnosis_attempts >= self.config.max_diagnoses {
            task.stage = RecoveryStage::Failed;
            task.note = Some("diagnosis budget exhausted".into());
            return Ok(None);
        }
        task.diagnosis_attempts += 1;
        task.stage = RecoveryStage::Diagnosing;
        let call_id = format!("{}-diagnosis-{}", task.id, task.diagnosis_attempts);
        task.diagnosis_call = Some(call_id.clone());
        let mut callback_task = task.clone();
        callback_task.revision = callback_task
            .revision
            .checked_add(1)
            .ok_or(RecoveryError::Capacity)?;
        callback_task.updated_at_ms = now;
        Ok(Some(RecoveryEffect::Diagnose {
            task: Box::new(callback_task),
            call_id,
            timeout_secs: self.config.diagnosis_timeout_secs,
            max_tool_calls: self.config.max_tool_calls,
        }))
    }
    fn apply(
        &mut self,
        event: &RecoveryEvent,
        now: u64,
        permit: Option<ExecutionPermit>,
    ) -> Result<Vec<RecoveryEffect>, RecoveryError> {
        if now < self.updated_at_ms {
            return Err(invalid("clock moved backwards"));
        }
        let mut effects = Vec::new();
        match event {
            RecoveryEvent::Register { problem, incident } => {
                problem.validate()?;
                if problem.target_id != self.config.target.target_id {
                    return Err(invalid("wrong target"));
                }
                if let Some(old) = self
                    .tasks
                    .values()
                    .find(|t| t.problem.incident_id == problem.incident_id)
                {
                    if old.problem.target_id != problem.target_id
                        || old.problem.fingerprint != problem.fingerprint
                    {
                        return Err(invalid("incident identity changed"));
                    }
                    return Ok(effects);
                }
                if incident.incident_id != problem.incident_id
                    || incident.revision != problem.incident_revision
                    || !incident.active
                {
                    return Err(invalid("current active incident required"));
                }
                if self.tasks.values().any(|task| !task.stage.terminal()) {
                    return Err(RecoveryError::Busy);
                }
                if self.tasks.len() >= self.config.max_tasks {
                    return Err(RecoveryError::Capacity);
                }
                let episode_count = self
                    .tasks
                    .values()
                    .filter(|t| {
                        t.problem.fingerprint == problem.fingerprint
                            && t.problem.conditions == problem.conditions
                    })
                    .count() as u64
                    + 1;
                let task = RecoveryTask {
                    id: format!(
                        "task-{:016x}",
                        self.revision
                            .checked_add(1)
                            .ok_or(RecoveryError::Capacity)?
                    ),
                    revision: 0,
                    problem: problem.clone(),
                    episode_count,
                    stage: RecoveryStage::Queued,
                    diagnosis_attempts: 0,
                    plan: None,
                    knowledge_id: None,
                    reused_script: false,
                    approval_id: None,
                    operation: None,
                    observation: None,
                    receipt: None,
                    verification: None,
                    result_check: None,
                    note: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                    diagnosis_call: None,
                };
                self.save_task(task, now)?;
            }
            RecoveryEvent::SelectPlan {
                task_id,
                revision,
                observation,
                candidate,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Queued {
                    return Err(RecoveryError::Conflict);
                }
                self.observe(observation, now)?;
                let conditions = self.conditions(&task, observation)?;
                task.observation = Some(observation.clone());
                if let Some(record) = candidate {
                    if task.episode_count < self.config.minimum_script_occurrences
                        || !record.candidate.reusable
                        || record.status != KnowledgeStatus::Verified
                        || record.disabled.is_some()
                        || record.cases.is_empty()
                        || record.cases.iter().any(|c| {
                            c.result.outcome != RepairOutcome::Verified || c.verification.is_none()
                        })
                        || record
                            .candidate
                            .conditions
                            .iter()
                            .any(|(k, v)| conditions.get(k) != Some(v))
                        || !task
                            .problem
                            .keywords
                            .iter()
                            .all(|word| record.candidate.keywords.contains(word))
                    {
                        return Err(invalid("knowledge evidence not reusable"));
                    }
                    // Revalidate complete candidate/case provenance through the knowledge reducer.
                    let mut checked = KnowledgeState::new(KnowledgeConfig {
                        max_records: 1,
                        max_cases_per_record: 1024,
                    })?;
                    let pending = checked.propose(
                        "validate-candidate",
                        KnowledgeCommand::UpsertCandidate(record.candidate.clone()),
                    )?;
                    checked = pending.state().clone();
                    for (index, case) in record.cases.iter().enumerate() {
                        let v = case
                            .verification
                            .as_ref()
                            .ok_or_else(|| invalid("missing trusted verification"))?;
                        let proof = TrustedBusinessVerification::attest(
                            &v.operation_id,
                            &v.target_id,
                            &v.script_id,
                            v.script_version,
                            &v.verifier_id,
                            v.evidence_refs.clone(),
                            v.verified_at_ms,
                        )?;
                        checked = checked
                            .propose(
                                format!("validate-case-{index}"),
                                KnowledgeCommand::RecordOutcome {
                                    record_id: record.id.clone(),
                                    case: case.result.clone(),
                                    verification: Some(proof),
                                },
                            )?
                            .state()
                            .clone();
                    }
                    if record.id != record.candidate.id {
                        return Err(invalid("candidate identity mismatch"));
                    }
                    let plan = RepairPlan {
                        summary: record.candidate.summary.clone(),
                        script: record.candidate.script.clone(),
                        reusable: true,
                    };
                    self.plan(&plan, observation)?;
                    self.candidates
                        .insert(record.id.clone(), record.candidate.clone());
                    task.plan = Some(plan);
                    task.knowledge_id = Some(record.id.clone());
                    task.reused_script = true;
                    effects.push(self.request_plan(&mut task)?);
                } else if let Some(effect) = self.diagnose(&mut task, now)? {
                    effects.push(effect);
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::RetryDiagnosis {
                task_id,
                revision,
                observation,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Diagnosing || task.diagnosis_call.is_some() {
                    return Err(RecoveryError::Conflict);
                }
                self.observe(observation, now)?;
                self.conditions(&task, observation)?;
                task.observation = Some(observation.clone());
                if let Some(effect) = self.diagnose(&mut task, now)? {
                    effects.push(effect);
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::DiagnosisCompleted {
                task_id,
                revision,
                call_id,
                plan,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Diagnosing
                    || task.diagnosis_call.as_ref() != Some(call_id)
                {
                    return Err(RecoveryError::Conflict);
                }
                self.plan(
                    plan,
                    task.observation
                        .as_ref()
                        .ok_or_else(|| invalid("missing observation"))?,
                )?;
                if plan.script.generated_by_harness != self.config.execution_harness {
                    return Err(invalid("unexpected diagnosis Harness"));
                }
                task.diagnosis_call = None;
                task.plan = Some(plan.clone());
                task.reused_script = false;
                task.knowledge_id = None;
                task.receipt = None;
                task.verification = None;
                effects.push(self.request_plan(&mut task)?);
                self.save_task(task, now)?;
            }
            RecoveryEvent::DiagnosisFailed {
                task_id,
                revision,
                call_id,
                reason,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Diagnosing
                    || task.diagnosis_call.as_ref() != Some(call_id)
                {
                    return Err(RecoveryError::Conflict);
                }
                text(reason, 8192)?;
                task.note = Some(reason.clone());
                task.diagnosis_call = None;
                if task.diagnosis_attempts >= self.config.max_diagnoses {
                    task.stage = RecoveryStage::Failed;
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::ApprovalAttached {
                task_id,
                revision,
                record,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::AwaitingApproval || task.approval_id.is_some() {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, record)?;
                task.approval_id = Some(record.request.request_id.clone());
                if matches!(
                    record.state,
                    ApprovalState::Executing
                        | ApprovalState::Unknown
                        | ApprovalState::Executed
                        | ApprovalState::Failed
                ) && let Some(effect) = self.mark_unknown(&mut task, now)?
                {
                    effects.push(effect);
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::ApprovalResolved {
                task_id,
                revision,
                record,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::AwaitingApproval || task.approval_id.is_none() {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, record)?;
                match record.state {
                    ApprovalState::Denied
                    | ApprovalState::Expired
                    | ApprovalState::Canceled
                    | ApprovalState::Revoked => {
                        let alternative = task.reused_script
                            && record.state == ApprovalState::Denied
                            && record.assessment.as_ref().is_some_and(|a| {
                                a.decision == approval::ApprovalDecision::Deny
                                    && matches!(
                                        a.reviewer,
                                        approval::AssessmentSource::Harness { .. }
                                    )
                            });
                        if alternative {
                            task.note = record.assessment.as_ref().map(|a| a.reason.clone());
                            self.reset_attempt(&mut task);
                            task.stage = RecoveryStage::Diagnosing;
                        } else {
                            task.stage = RecoveryStage::Denied;
                        }
                    }
                    ApprovalState::Executing
                    | ApprovalState::Unknown
                    | ApprovalState::Executed
                    | ApprovalState::Failed => {
                        if let Some(effect) = self.mark_unknown(&mut task, now)? {
                            effects.push(effect);
                        }
                    }

                    _ => {}
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::ExecutionAuthorized {
                task_id,
                revision,
                approval,
                observation,
                incident,
                authority,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::AwaitingApproval || task.approval_id.is_none() {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, approval)?;
                if approval.state != ApprovalState::Executing
                    || now / 1000 >= approval.request.expires_at
                    || approval.updated_at > now / 1000
                {
                    return Err(invalid("current consumed approval required"));
                }
                if authority.target_id != task.problem.target_id
                    || !crate::identity::valid_id(&authority.epoch)
                    || incident.incident_id != task.problem.incident_id
                    || incident.revision < task.problem.incident_revision
                    || !incident.active
                {
                    return Err(invalid("current incident and target ownership required"));
                }
                self.observe(observation, now)?;
                self.conditions(&task, observation)?;
                self.plan(
                    task.plan.as_ref().ok_or_else(|| invalid("missing plan"))?,
                    observation,
                )?;
                task.stage = RecoveryStage::Executing;
                task.observation = Some(observation.clone());
                self.save_task(task, now)?;
                if let Some(permit) = permit {
                    effects.push(RecoveryEffect::Execute {
                        permit,
                        authority: authority.clone(),
                        timeout_secs: self.config.target.action_timeout_secs,
                    });
                }
            }
            RecoveryEvent::ExecutionRecorded {
                task_id,
                revision,
                receipt,
                approval,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Executing {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, approval)?;
                let op = task
                    .operation
                    .as_ref()
                    .ok_or_else(|| invalid("missing operation"))?;
                evidence(&receipt.evidence_refs)?;
                text(&receipt.summary, 8192)?;
                if receipt.operation_id != op.operation_id
                    || receipt.target_id != op.target
                    || (receipt.outcome != ScriptOutcome::Unknown && !receipt.executor_stopped)
                {
                    return Err(invalid(
                        "execution receipt identity or stopping state mismatch",
                    ));
                }
                let expected = match receipt.outcome {
                    ScriptOutcome::Executed => ApprovalState::Executed,
                    ScriptOutcome::Failed => ApprovalState::Failed,
                    ScriptOutcome::Unknown => ApprovalState::Unknown,
                };
                if approval.state != expected {
                    return Err(invalid("execution and approval facts differ"));
                }
                task.receipt = Some(receipt.clone());
                if receipt.outcome == ScriptOutcome::Executed {
                    task.stage = RecoveryStage::Verifying;
                    effects.push(RecoveryEffect::Verify {
                        task_id: task.id.clone(),
                        operation: op.clone(),
                        receipt: receipt.clone(),
                    });
                } else {
                    let outcome = if receipt.outcome == ScriptOutcome::Failed {
                        RepairOutcome::Failed
                    } else {
                        RepairOutcome::Unknown
                    };
                    effects.push(self.finish(&mut task, outcome, now)?);
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::VerificationRecorded {
                task_id,
                revision,
                verification,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Verifying {
                    return Err(RecoveryError::Conflict);
                }
                self.verification(&task, verification, now)?;
                task.verification = Some(verification.clone());
                let outcome = match (verification.healthy, verification.executor_stopped) {
                    (Some(true), true) => RepairOutcome::Verified,
                    (Some(false), true) => RepairOutcome::Failed,
                    _ => RepairOutcome::Unknown,
                };
                effects.push(self.finish(&mut task, outcome, now)?);
                self.save_task(task, now)?;
            }
            RecoveryEvent::VerificationUnavailable {
                task_id,
                revision,
                reason,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Verifying {
                    return Err(RecoveryError::Conflict);
                }
                text(reason, 8192)?;
                task.note = Some(reason.clone());
                effects.push(self.finish(&mut task, RepairOutcome::Unknown, now)?);
                self.save_task(task, now)?;
            }
            RecoveryEvent::ResultChecked {
                task_id,
                revision,
                execution,
                verification,
                actor,
                approval,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Unknown {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, approval)?;
                self.verification(&task, verification, now)?;
                text(actor, 128)?;
                evidence(&execution.evidence_refs)?;
                let op = task
                    .operation
                    .as_ref()
                    .ok_or_else(|| invalid("missing operation"))?;
                if execution.operation_id != op.operation_id
                    || execution.target_id != op.target
                    || execution.executor_id != self.config.target.executor_id
                    || (execution.checked_at_ms < task.updated_at_ms
                        && !task
                            .result_check
                            .as_ref()
                            .is_some_and(|r| &r.execution == execution))
                    || execution.checked_at_ms > now
                    || now - execution.checked_at_ms > 30_000
                    || execution.executor_stopped != verification.executor_stopped
                    || (execution.outcome != CheckedExecution::Unknown
                        && !execution.executor_stopped)
                {
                    return Err(invalid("invalid independent execution evidence"));
                }
                let known = task
                    .result_check
                    .as_ref()
                    .map(|r| r.execution.outcome)
                    .filter(|v| *v != CheckedExecution::Unknown)
                    .or_else(|| {
                        task.receipt.as_ref().and_then(|r| match r.outcome {
                            ScriptOutcome::Executed => Some(CheckedExecution::Executed),
                            ScriptOutcome::Failed => Some(CheckedExecution::Failed),
                            ScriptOutcome::Unknown => None,
                        })
                    });
                if known.is_some_and(|old| old != execution.outcome) {
                    return Err(invalid("cannot overwrite confirmed execution facts"));
                }
                let allowed = match approval.state {
                    ApprovalState::Unknown => true,
                    ApprovalState::Executed => execution.outcome == CheckedExecution::Executed,
                    ApprovalState::Failed => {
                        execution.outcome == CheckedExecution::Failed
                            || (execution.outcome == CheckedExecution::NotExecuted
                                && task.result_check.as_ref().is_some_and(|r| {
                                    r.execution.outcome == CheckedExecution::NotExecuted
                                }))
                    }
                    _ => false,
                };
                if !allowed {
                    return Err(invalid("result check conflicts with approval"));
                }
                // The prepared event carries evidence. Host must reconcile the
                // approval first or in the same atomic transaction before release.

                task.result_check = Some(ResultCheckRecord {
                    execution: execution.clone(),
                    actor: actor.clone(),
                });
                task.verification = Some(verification.clone());
                if approval.state == ApprovalState::Unknown {
                    self.save_task(task, now)?;
                    return Ok(effects);
                }
                match execution.outcome {
                    CheckedExecution::Unknown => {}
                    CheckedExecution::NotExecuted => {
                        task.stage = RecoveryStage::Canceled;
                    }
                    CheckedExecution::Executed | CheckedExecution::Failed => {
                        task.receipt = Some(ScriptReceipt {
                            operation_id: op.operation_id.clone(),
                            target_id: op.target.clone(),
                            outcome: if execution.outcome == CheckedExecution::Executed {
                                ScriptOutcome::Executed
                            } else {
                                ScriptOutcome::Failed
                            },
                            executor_stopped: true,
                            evidence_refs: execution.evidence_refs.clone(),
                            summary: "independent execution result check".into(),
                        });
                        if execution.outcome == CheckedExecution::Failed {
                            effects.push(self.finish(&mut task, RepairOutcome::Failed, now)?);
                        } else if let Some(healthy) = verification.healthy {
                            effects.push(self.finish(
                                &mut task,
                                if healthy {
                                    RepairOutcome::Verified
                                } else {
                                    RepairOutcome::Failed
                                },
                                now,
                            )?);
                        }
                    }
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::Resume { task_id, revision } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Paused {
                    return Err(RecoveryError::Conflict);
                }
                task.stage = RecoveryStage::AwaitingApproval;
                if task.approval_id.is_none() {
                    effects.push(RecoveryEffect::RequestApproval {
                        operation: task
                            .operation
                            .clone()
                            .ok_or_else(|| invalid("missing original intent"))?,
                        policy: self.policy(&task).clone(),
                    });
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::Cancel {
                task_id,
                revision,
                approval,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage.terminal()
                    || matches!(
                        task.stage,
                        RecoveryStage::Executing
                            | RecoveryStage::Unknown
                            | RecoveryStage::Verifying
                    )
                {
                    return Err(invalid(
                        "dispatched work requires independent result evidence",
                    ));
                }
                if task.approval_id.is_some() {
                    let record = approval
                        .as_ref()
                        .ok_or_else(|| invalid("cancel linked approval first"))?;
                    self.bind_approval(&task, record)?;
                    if !matches!(
                        record.state,
                        ApprovalState::Canceled
                            | ApprovalState::Denied
                            | ApprovalState::Expired
                            | ApprovalState::Revoked
                    ) {
                        return Err(invalid("approval still grants authority"));
                    }
                } else if task.operation.is_some() {
                    return Err(invalid(
                        "resolve pending approval association before cancellation",
                    ));
                }
                task.stage = RecoveryStage::Canceled;
                task.diagnosis_call = None;
                self.save_task(task, now)?;
            }
            RecoveryEvent::Recover => {
                self.recovery_required = false;
                let tasks: Vec<_> = self.tasks.values().cloned().collect();
                for mut task in tasks {
                    match task.stage {
                        RecoveryStage::Executing => {
                            let op = task
                                .operation
                                .as_ref()
                                .ok_or_else(|| invalid("missing execution intent"))?;
                            task.receipt = Some(ScriptReceipt {
                                operation_id: op.operation_id.clone(),
                                target_id: op.target.clone(),
                                outcome: ScriptOutcome::Unknown,
                                executor_stopped: false,
                                evidence_refs: vec![format!(
                                    "approval:{}",
                                    task.approval_id.as_deref().unwrap_or("unknown")
                                )],
                                summary: "interrupted execution; independent evidence required"
                                    .into(),
                            });
                            effects.push(self.finish(&mut task, RepairOutcome::Unknown, now)?);
                            task.note =
                                Some("interrupted execution; explicit evidence required".into());
                        }
                        RecoveryStage::AwaitingApproval => {
                            task.stage = RecoveryStage::Paused;
                        }
                        RecoveryStage::Diagnosing if task.diagnosis_call.is_some() => {
                            task.diagnosis_call = None;
                            task.note =
                                Some("interrupted diagnosis remains charged to budget".into());
                        }
                        _ => continue,
                    }
                    self.save_task(task, now)?;
                }
            }
            RecoveryEvent::DeliveryConfirmed { delivery_id } => {
                let current = self
                    .deliveries
                    .get(delivery_id)
                    .ok_or_else(|| invalid("delivery not found"))?;
                if !current.delivered
                    && self.deliveries.values().any(|older| {
                        !older.delivered
                            && older.candidate.id == current.candidate.id
                            && older.created_revision < current.created_revision
                    })
                {
                    return Err(invalid("earlier knowledge outcome must be delivered first"));
                }
                let delivery = self
                    .deliveries
                    .get_mut(delivery_id)
                    .ok_or_else(|| invalid("delivery not found"))?;
                delivery.delivered = true;
            }
        }
        Ok(effects)
    }

    fn verification(
        &self,
        task: &RecoveryTask,
        v: &BusinessVerification,
        now: u64,
    ) -> Result<(), RecoveryError> {
        evidence(&v.evidence_refs)?;
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing operation"))?;
        if v.operation_id != op.operation_id
            || v.target_id != op.target
            || v.profile != self.config.target.verification_profile
            || (v.verified_at_ms < task.updated_at_ms && task.verification.as_ref() != Some(v))
            || v.verified_at_ms > now
            || now - v.verified_at_ms > 30_000
        {
            return Err(invalid(
                "verification identity, profile or freshness mismatch",
            ));
        }
        Ok(())
    }
    fn reset_attempt(&self, task: &mut RecoveryTask) {
        task.plan = None;
        task.operation = None;
        task.approval_id = None;
        task.knowledge_id = None;
        task.reused_script = false;
        task.receipt = None;
        task.verification = None;
        task.result_check = None;
        task.diagnosis_call = None;
    }
    fn finish(
        &mut self,
        task: &mut RecoveryTask,
        outcome: RepairOutcome,
        now: u64,
    ) -> Result<RecoveryEffect, RecoveryError> {
        let plan = task.plan.as_ref().ok_or_else(|| invalid("missing plan"))?;
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing operation"))?;
        let receipt = task
            .receipt
            .as_ref()
            .ok_or_else(|| invalid("missing execution receipt"))?;
        let id = task
            .knowledge_id
            .clone()
            .unwrap_or_else(|| format!("case-{}-{}", task.id, task.diagnosis_attempts));
        let candidate = match self.candidates.get(&id) {
            Some(candidate) => candidate.clone(),
            None => KnowledgeCandidate {
                id: id.clone(),
                incident_id: task.problem.incident_id.clone(),
                summary: plan.summary.clone(),
                keywords: task.problem.keywords.clone(),
                conditions: self.conditions(
                    task,
                    task.observation
                        .as_ref()
                        .ok_or_else(|| invalid("missing observation"))?,
                )?,
                script: plan.script.clone(),
                reusable: plan.reusable
                    && task.episode_count >= self.config.minimum_script_occurrences,
                evidence_refs: task.problem.evidence_refs.clone(),
                created_at_ms: task.created_at_ms,
            },
        };
        let refs = task.verification.as_ref().map_or_else(
            || receipt.evidence_refs.clone(),
            |v| v.evidence_refs.clone(),
        );
        let verification = if outcome == RepairOutcome::Verified {
            let v = task
                .verification
                .as_ref()
                .ok_or_else(|| invalid("missing verified evidence"))?;
            Some(BusinessVerificationRecord {
                operation_id: op.operation_id.clone(),
                target_id: op.target.clone(),
                script_id: plan.script.id.clone(),
                script_version: plan.script.version,
                verifier_id: format!("{}:{}", self.config.target.executor_id, v.profile),
                evidence_refs: refs.clone(),
                verified_at_ms: v.verified_at_ms,
            })
        } else {
            None
        };
        let case = RepairCase {
            id: format!("{}-{outcome:?}", op.operation_id),
            operation_id: op.operation_id.clone(),
            target_id: op.target.clone(),
            script_id: plan.script.id.clone(),
            script_version: plan.script.version,
            outcome,
            evidence_refs: refs,
            recorded_at_ms: now,
        };
        let delivery = KnowledgeDelivery {
            id: case.id.clone(),
            created_revision: self
                .revision
                .checked_add(1)
                .ok_or(RecoveryError::Capacity)?,
            candidate: candidate.clone(),
            case,
            verification,
            delivered: false,
        };
        if self.deliveries.contains_key(&delivery.id) {
            return Err(invalid("result delivery already exists"));
        }
        self.candidates.insert(id.clone(), candidate);
        self.deliveries
            .insert(delivery.id.clone(), delivery.clone());
        if outcome != RepairOutcome::Verified {
            self.quarantined
                .insert((plan.script.id.clone(), plan.script.version));
        }
        task.knowledge_id = Some(id);
        task.stage = match outcome {
            RepairOutcome::Verified => RecoveryStage::Completed,
            RepairOutcome::Unknown => RecoveryStage::Unknown,
            RepairOutcome::Failed => RecoveryStage::Failed,
        };
        if outcome == RepairOutcome::Failed
            && task.reused_script
            && task.diagnosis_attempts < self.config.max_diagnoses
        {
            self.reset_attempt(task);
            task.stage = RecoveryStage::Diagnosing;
            task.note = Some("reused script failed; fresh diagnosis and approval required".into());
        }
        let earliest = self
            .pending_deliveries()
            .into_iter()
            .find(|item| item.candidate.id == delivery.candidate.id)
            .ok_or_else(|| invalid("missing pending delivery"))?;
        Ok(RecoveryEffect::DeliverKnowledge(Box::new(earliest)))
    }
}
