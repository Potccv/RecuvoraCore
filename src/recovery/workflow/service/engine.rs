use super::contract::{evidence, facts};
use super::storage::TaskStore;
use super::*;
use approval::{ApprovalState, ApprovalStore, ApprovalStoreConfig, ExecutionOutcome, ReviewStage};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

fn cached_script_denial_context(record: &approval::ApprovalRecord) -> Option<String> {
    let assessment = record.assessment.as_ref()?;
    if record.state != ApprovalState::Denied
        || assessment.decision != approval::ApprovalDecision::Deny
        || !matches!(
            assessment.reviewer,
            approval::AssessmentSource::Harness { .. }
        )
    {
        return None;
    }
    Some(
        serde_json::json!({
            "kind": "cached_script_rejected",
            "request_id": record.request.request_id,
            "assessment": assessment,
            "next_step": "diagnose an alternative within the original delegated scope"
        })
        .to_string(),
    )
}

struct State {
    tasks: TaskStore,
    approvals: ApprovalStore,
    knowledge: KnowledgeStore,
}

struct BoundKnowledgeSource {
    identity: String,
    source: Arc<dyn KnowledgeSource>,
}

/// Complete a durable approval intent using the original operation identity.
/// A repeated request preserves the authority's decision, deadline and permit state.
fn attach_approval(
    tasks: &mut TaskStore,
    approvals: &mut ApprovalStore,
    config: &RecoveryConfig,
    mut task: RecoveryTask,
    now_ms: u64,
) -> Result<RecoveryTask, RecoveryError> {
    let operation = task
        .operation
        .clone()
        .ok_or_else(|| service("missing durable approval intent"))?;
    let policy = if task.reused_script {
        &config.script_approval
    } else {
        &config.approval
    };
    let record = approvals.request(operation, policy.clone(), now_ms / 1000)?;
    task.approval_id = Some(record.request.request_id);
    tasks.save(task, config, now_ms)
}

/// One configured target, with durable tasks and one authority for its approvals.
/// Applications authenticate callers of management methods and protect runtime files.
pub struct RecoveryService {
    config: RecoveryConfig,
    backend: Arc<dyn RepairBackend>,
    clock: Arc<dyn RecoveryClock>,
    state: Mutex<Option<State>>,
    active: Mutex<BTreeSet<String>>,
    calls: CallScope,
    accepting: AtomicBool,
    incident_guard: OnceLock<Arc<dyn IncidentGuard>>,
    state_directory: PathBuf,
    ownership: Mutex<Option<Box<dyn TargetLease>>>,
    knowledge_source: OnceLock<BoundKnowledgeSource>,
}

impl RecoveryService {
    pub fn open(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_store_configs(
            dir,
            config,
            backend,
            ApprovalStoreConfig::default(),
            KnowledgeStoreConfig::default(),
        )
    }
    pub fn open_with_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        approval_store_config: ApprovalStoreConfig,
        knowledge_store_config: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            Arc::new(SystemRecoveryClock),
            approval_store_config,
            knowledge_store_config,
        )
    }
    pub fn open_with_clock(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            clock,
            ApprovalStoreConfig::default(),
            KnowledgeStoreConfig::default(),
        )
    }
    pub fn open_with_clock_and_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
        approval_store_config: ApprovalStoreConfig,
        knowledge_store_config: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        config.validate()?;
        approval_store_config.validate()?;
        knowledge_store_config.validate().map_err(service)?;
        let dir = dir.as_ref();
        let mut tasks = TaskStore::open(dir, &config)?;
        let mut approvals = ApprovalStore::open(
            dir.join("approvals"),
            approval_store_config,
            clock.now_ms() / 1000,
        )?;
        let knowledge = KnowledgeStore::open(dir.join("knowledge.jsonl"), knowledge_store_config)
            .map_err(service)?;
        let recovered: Vec<_> = tasks.tasks.values().cloned().collect();
        for mut task in recovered {
            if task.stage == RecoveryStage::AwaitingApproval && task.approval_id.is_none() {
                task = attach_approval(&mut tasks, &mut approvals, &config, task, clock.now_ms())?;
            }
            let authority = task
                .approval_id
                .as_ref()
                .and_then(|id| approvals.get(id))
                .map(|r| r.state);
            if task.stage == RecoveryStage::Executing || authority == Some(ApprovalState::Unknown) {
                if authority == Some(ApprovalState::Approved) {
                    task.stage = RecoveryStage::Paused;
                    task.note =
                        Some("execution intent was not consumed; explicit resume required".into());
                } else {
                    task.stage = RecoveryStage::Unknown;
                    task.note = Some("interrupted execution; check target and executor results before continuing".into());
                }
                tasks.save(task, &config, clock.now_ms())?;
            } else if task.stage == RecoveryStage::AwaitingApproval
                && task
                    .approval_id
                    .as_ref()
                    .and_then(|id| approvals.get(id))
                    .is_some_and(|r| r.state == ApprovalState::Approved)
            {
                task.stage = RecoveryStage::Paused;
                task.note = Some(
                    "recovered approval; trusted explicit resume required before dispatch".into(),
                );
                tasks.save(task, &config, clock.now_ms())?;
            }
        }
        Ok(Arc::new(Self {
            config,
            backend,
            clock,
            state: Mutex::new(Some(State {
                tasks,
                approvals,
                knowledge,
            })),
            active: Mutex::new(BTreeSet::new()),
            calls: CallScope::default(),
            accepting: AtomicBool::new(true),
            incident_guard: OnceLock::new(),
            state_directory: dir.to_path_buf(),
            ownership: Mutex::new(None),
            knowledge_source: OnceLock::new(),
        }))
    }
    pub fn config(&self) -> &RecoveryConfig {
        &self.config
    }
    /// Host must use one ownership authority for every alias of this target.
    /// Opening a store alone enables recovery/querying, never dispatch.
    pub fn bind_target_ownership(
        &self,
        authority: Arc<dyn TargetOwnership>,
    ) -> Result<(), RecoveryError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RecoveryError::Stopped);
        }
        let mut owner = lock(&self.ownership)?;
        if owner.is_some() {
            return Err(RecoveryError::Invalid(
                "target ownership already bound".into(),
            ));
        }
        let target = CanonicalTarget::new(self.config.target.target_id.clone())?;
        let lease = authority.acquire(&target, &self.state_directory)?;
        if lease.target() != &target
            || lease.recovery_directory() != self.state_directory.canonicalize()?
        {
            return Err(RecoveryError::Invalid(
                "ownership target or store mismatch".into(),
            ));
        }
        lease.validate()?;
        *owner = Some(lease);
        Ok(())
    }
    fn check_ownership(&self) -> Result<(), RecoveryError> {
        lock(&self.ownership)?
            .as_ref()
            .ok_or_else(|| service("canonical target ownership required"))?
            .validate()
    }
    /// A source returns bounded proposals only; its claims cannot attest success.
    pub fn bind_knowledge_source(
        &self,
        source: Arc<dyn KnowledgeSource>,
    ) -> Result<(), RecoveryError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RecoveryError::Stopped);
        }
        let identity = source.identity();
        if !crate::identity::valid_id(identity) {
            return Err(RecoveryError::Invalid(
                "invalid knowledge source identity".into(),
            ));
        }
        let identity = identity.to_owned();
        self.knowledge_source
            .set(BoundKnowledgeSource { identity, source })
            .map_err(|_| RecoveryError::Invalid("knowledge source already bound".into()))
    }
    /// Binds a trusted incident authority for this runtime, exactly once.
    /// Reopened workflows must bind again; missing authority never allows execution.
    pub fn bind_incident_guard(&self, guard: Arc<dyn IncidentGuard>) -> Result<(), RecoveryError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RecoveryError::Stopped);
        }
        self.incident_guard.set(guard).map_err(|_| {
            RecoveryError::Invalid("incident guard is already bound for this runtime".into())
        })
    }
    fn state<T>(
        &self,
        action: impl FnOnce(&mut State) -> Result<T, RecoveryError>,
    ) -> Result<T, RecoveryError> {
        let mut state = lock(&self.state)?;
        action(state.as_mut().ok_or(RecoveryError::Stopped)?)
    }
    fn save(&self, task: RecoveryTask) -> Result<RecoveryTask, RecoveryError> {
        self.state(|s| s.tasks.save(task, &self.config, self.clock.now_ms()))
    }
    fn task(&self, id: &str) -> Result<RecoveryTask, RecoveryError> {
        self.query(id)?
            .ok_or_else(|| RecoveryError::Invalid("task not found".into()))
    }
    fn claim(&self, id: &str) -> Result<Active<'_>, RecoveryError> {
        if !lock(&self.active)?.insert(id.to_owned()) {
            return Err(RecoveryError::Busy);
        }
        Ok(Active {
            active: &self.active,
            id: id.to_owned(),
        })
    }
    pub fn query(&self, id: &str) -> Result<Option<RecoveryTask>, RecoveryError> {
        self.state(|s| Ok(s.tasks.tasks.get(id).cloned()))
    }
    pub fn tasks(&self) -> Result<Vec<RecoveryTask>, RecoveryError> {
        self.state(|s| Ok(s.tasks.tasks.values().cloned().collect()))
    }
    pub fn inspect_tasks(
        &self,
        query: &RecoveryQuery,
    ) -> Result<Vec<RecoveryTaskSummary>, RecoveryError> {
        query.validate()?;
        self.state(|s| {
            Ok(s.tasks
                .tasks
                .values()
                .filter(|task| {
                    (query.stages.is_empty() || query.stages.contains(&task.stage))
                        && query.after_id.as_ref().is_none_or(|id| &task.id > id)
                })
                .take(query.limit)
                .map(RecoveryTaskSummary::from)
                .collect())
        })
    }
    pub fn overview(&self) -> Result<RecoveryOverview, RecoveryError> {
        self.state(|s| {
            s.tasks.ensure_current()?;
            let (journal_bytes, task_count, journal_limit_bytes, task_limit) = s.tasks.usage();
            let mut stages = BTreeMap::new();
            for task in s.tasks.tasks.values() {
                *stages.entry(task.stage.clone()).or_insert(0) += 1;
            }
            Ok(RecoveryOverview {
                task_count,
                task_limit,
                journal_bytes,
                journal_limit_bytes,
                stages,
                unknown_approvals: s
                    .approvals
                    .list()
                    .iter()
                    .filter(|record| record.state == ApprovalState::Unknown)
                    .count(),
            })
        })
    }
    pub fn knowledge(&self, query: &KnowledgeQuery) -> Result<Vec<KnowledgeRecord>, RecoveryError> {
        self.state(|s| s.knowledge.search(query).map_err(service))
    }
    pub fn submit(&self, problem: ProblemContext) -> Result<RecoveryTask, RecoveryError> {
        self.check_ownership()?;
        problem.validate()?;
        if problem.target_id != self.config.target.target_id {
            return Err(RecoveryError::Invalid("target binding mismatch".into()));
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RecoveryError::Stopped);
        }
        // Repeated delivery remains a read of the original durable episode,
        // including after the incident has been resolved.
        if let Some(old) = self.state(|s| {
            Ok(s.tasks
                .tasks
                .values()
                .find(|task| task.problem.incident_id == problem.incident_id)
                .cloned())
        })? {
            if old.problem.target_id != problem.target_id
                || old.problem.fingerprint != problem.fingerprint
            {
                return Err(RecoveryError::Invalid("incident identity changed".into()));
            }
            return Ok(old);
        }
        let guard = self
            .incident_guard
            .get()
            .ok_or_else(|| service("incident authority required for episode registration"))?;
        if !matches!(guard.check(&problem)?, IncidentReadiness::Active { revision }
            if revision == problem.incident_revision)
        {
            return Err(RecoveryError::Invalid(
                "current authoritative incident episode required".into(),
            ));
        }
        self.state(|s| {
            // Repeated notifications and changing incident revisions never create duplicate repairs.
            if let Some(old) = s
                .tasks
                .tasks
                .values()
                .find(|t| t.problem.incident_id == problem.incident_id)
            {
                if old.problem.target_id != problem.target_id
                    || old.problem.fingerprint != problem.fingerprint
                {
                    return Err(RecoveryError::Invalid("incident identity changed".into()));
                }
                return Ok(old.clone());
            }
            if s.tasks
                .tasks
                .values()
                .any(|t| t.problem.target_id == problem.target_id && !t.stage.terminal())
            {
                return Err(RecoveryError::Busy);
            }
            let episodes = s
                .tasks
                .tasks
                .values()
                .filter(|t| {
                    t.problem.target_id == problem.target_id
                        && t.problem.fingerprint == problem.fingerprint
                        && t.problem.conditions == problem.conditions
                })
                .count() as u64
                + 1;
            let now = self.clock.now_ms();
            s.tasks.save(
                RecoveryTask {
                    id: crate::identity::call_id(),
                    revision: 0,
                    problem,
                    episode_count: episodes,
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
                },
                &self.config,
                now,
            )
        })
    }
    pub fn approval(
        &self,
        task_id: &str,
    ) -> Result<Option<approval::ApprovalRecord>, RecoveryError> {
        let task = self.task(task_id)?;
        self.state(|s| {
            Ok(task
                .approval_id
                .as_ref()
                .and_then(|id| s.approvals.get(id))
                .cloned())
        })
    }
    /// Records a human decision only. It never dispatches the action itself.
    pub fn decide_human(
        &self,
        task_id: &str,
        revision: u64,
        decision: approval::ApprovalDecision,
        actor: String,
        reason: String,
    ) -> Result<approval::ApprovalRecord, RecoveryError> {
        let task = self.task(task_id)?;
        let id = task
            .approval_id
            .as_ref()
            .ok_or_else(|| service("task has no approval"))?;
        self.state(|s| {
            Ok(s.approvals.decide_human_at_revision(
                id,
                revision,
                approval::ApprovalAssessment {
                    decision,
                    reason,
                    reviewer: approval::AssessmentSource::Human { actor },
                },
                self.policy(&task),
                self.clock.now_ms() / 1000,
            )?)
        })
    }
    pub fn resume(&self, task_id: &str, revision: u64) -> Result<RecoveryTask, RecoveryError> {
        self.check_ownership()?;
        let mut task = self.task(task_id)?;
        if task.revision != revision || task.stage != RecoveryStage::Paused {
            return Err(RecoveryError::Busy);
        }
        task.stage = RecoveryStage::AwaitingApproval;
        self.save(task)
    }
    pub async fn advance(
        self: &Arc<Self>,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.check_ownership()?;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RecoveryError::Stopped);
        }
        let recovery = self.clone();
        let id = id.to_owned();
        self.calls
            .run(cancellation.clone(), async move {
                let _active = recovery.claim(&id)?;
                recovery.drive(&id, cancellation).await
            })
            .await
            .map_err(service)?
    }
    pub async fn shutdown(&self) -> Result<(), RecoveryError> {
        self.accepting.store(false, Ordering::Release);
        self.calls.shutdown().await.map_err(service)?;
        let mut state = lock(&self.state)?;
        if let Some(s) = state.as_mut() {
            s.tasks.ensure_current()?;
            s.approvals.ensure_current()?;
            s.knowledge.close().map_err(service)?;
            let releasable = s.tasks.tasks.values().all(|task| task.stage.terminal())
                && s.approvals.list().iter().all(|record| {
                    !matches!(
                        record.state,
                        ApprovalState::Executing | ApprovalState::Unknown
                    )
                });
            if releasable && let Some(lease) = lock(&self.ownership)?.as_mut() {
                lease.release()?;
            }
        }
        state.take();
        lock(&self.ownership)?.take();
        Ok(())
    }
    fn policy<'a>(&'a self, task: &RecoveryTask) -> &'a approval::ApprovalPolicy {
        if task.reused_script {
            &self.config.script_approval
        } else {
            &self.config.approval
        }
    }
    async fn inspect(
        &self,
        cancellation: Cancellation,
    ) -> Result<TargetObservation, RecoveryError> {
        let observation = self
            .backend
            .inspect(&self.config.target, cancellation)
            .await?;
        facts(&observation.facts)?;
        evidence(&observation.evidence_refs)?;
        let now = self.clock.now_ms();
        if observation.target_id != self.config.target.target_id
            || observation.observed_at_ms > now
            || now.saturating_sub(observation.observed_at_ms) > 30_000
            || self
                .config
                .target
                .required_facts
                .iter()
                .any(|(k, v)| observation.facts.get(k) != Some(v))
        {
            return Err(RecoveryError::Invalid(
                "target observation stale or outside trusted conditions".into(),
            ));
        }
        Ok(observation)
    }
    fn conditions(
        &self,
        task: &RecoveryTask,
        observed: &TargetObservation,
    ) -> Result<BTreeMap<String, String>, RecoveryError> {
        let mut values = observed.facts.clone();
        for (key, value) in &task.problem.conditions {
            if values.get(key) != Some(value) {
                return Err(RecoveryError::Invalid(
                    "incident environment changed or required fact is missing".into(),
                ));
            }
            values.insert(key.clone(), value.clone());
        }
        values.insert("fault_fingerprint".into(), task.problem.fingerprint.clone());
        values.insert("platform".into(), self.config.target.platform.clone());
        facts(&values)?;
        Ok(values)
    }
    fn validate_plan(
        &self,
        plan: &RepairPlan,
        observation: &TargetObservation,
    ) -> Result<(), RecoveryError> {
        text(&plan.summary, 4096)?;
        let script = &plan.script;
        self.state(|s| s.knowledge.validate_script(script).map_err(service))?;
        text(&script.source, MAX_SCRIPT_BYTES)?;
        text(&script.id, 128)?;
        facts(&script.preconditions)?;
        if script.version == 0
            || script.preconditions.is_empty()
            || script.platform != self.config.target.platform
            || !self
                .config
                .target
                .allowed_languages
                .contains(&script.language)
            || script
                .preconditions
                .iter()
                .any(|(k, v)| observation.facts.get(k) != Some(v))
        {
            return Err(RecoveryError::Invalid(
                "script platform, language or preconditions do not match current target".into(),
            ));
        }
        Ok(())
    }
    async fn drive(
        &self,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<RecoveryTask, RecoveryError> {
        for _ in 0..64 {
            self.check_ownership()?;
            let mut task = self.task(id)?;
            // A request or association failure must retry this saved intent,
            // including cancellation, rather than consume another diagnosis.
            if task.stage == RecoveryStage::AwaitingApproval && task.approval_id.is_none() {
                task = self.state(|s| {
                    attach_approval(
                        &mut s.tasks,
                        &mut s.approvals,
                        &self.config,
                        task,
                        self.clock.now_ms(),
                    )
                })?;
            }
            if task.stage.terminal()
                || matches!(task.stage, RecoveryStage::Unknown | RecoveryStage::Paused)
            {
                return Ok(task);
            }
            if cancellation.is_cancelled()
                && !matches!(
                    task.stage,
                    RecoveryStage::Verifying | RecoveryStage::Publishing | RecoveryStage::Executing
                )
            {
                if let Some(approval_id) = &task.approval_id {
                    self.state(|s| {
                        if s.approvals.get(approval_id).is_some_and(|r| {
                            matches!(
                                r.state,
                                ApprovalState::Pending
                                    | ApprovalState::WaitingHuman
                                    | ApprovalState::Approved
                            )
                        }) {
                            s.approvals.cancel(
                                approval_id,
                                "workflow canceled before execution".into(),
                                self.clock.now_ms() / 1000,
                            )?;
                        }
                        Ok(())
                    })?;
                }
                task.stage = RecoveryStage::Canceled;
                return self.save(task);
            }
            match task.stage {
                RecoveryStage::Queued => {
                    let observed = self.inspect(cancellation.clone()).await?;
                    let conditions = self.conditions(&task, &observed)?;
                    let candidates = self.state(|s| {
                        if task.episode_count < self.config.minimum_script_occurrences {
                            return Ok(Vec::new());
                        }
                        s.knowledge
                            .search_reusable(&KnowledgeQuery {
                                conditions,
                                keywords: task.problem.keywords.clone(),
                                limit: 1,
                            })
                            .map_err(service)
                    })?;
                    task.observation = Some(observed.clone());
                    if let Some(candidate) = candidates.first() {
                        let plan = RepairPlan {
                            summary: candidate.candidate.summary.clone(),
                            script: candidate.candidate.script.clone(),
                            reusable: true,
                        };
                        self.validate_plan(&plan, &observed)?;
                        task.plan = Some(plan);
                        task.knowledge_id = Some(candidate.id.clone());
                        task.reused_script = true;
                        self.request_plan(task)?;
                    } else {
                        task.stage = RecoveryStage::Diagnosing;
                        self.save(task)?;
                    }
                }
                RecoveryStage::Diagnosing => {
                    if task.diagnosis_attempts >= self.config.max_diagnoses {
                        task.stage = RecoveryStage::Failed;
                        task.note = Some("diagnosis budget exhausted".into());
                        return self.save(task);
                    }
                    let observed = self.inspect(cancellation.clone()).await?;
                    let conditions = self.conditions(&task, &observed)?;
                    let query = KnowledgeQuery {
                        conditions,
                        keywords: task.problem.keywords.clone(),
                        limit: 4,
                    };
                    let mut knowledge: Vec<_> = self
                        .state(|s| s.knowledge.search(&query).map_err(service))?
                        .into_iter()
                        .map(|record| record.candidate)
                        .collect();
                    if let Some(source) = self.knowledge_source.get() {
                        let proposals = source
                            .source
                            .query(query.clone(), cancellation.clone())
                            .await;
                        if cancellation.is_cancelled() {
                            continue;
                        }
                        let candidates = match proposals {
                            Ok(proposals) => self.state(|s| {
                                s.knowledge
                                    .validate_external_candidates(
                                        &source.identity,
                                        &query,
                                        proposals,
                                    )
                                    .map_err(service)
                            }),
                            Err(_) => Err(service("knowledge source request failed")),
                        };
                        match candidates {
                            Ok(candidates) => {
                                for candidate in candidates {
                                    if !knowledge.iter().any(|old| old.id == candidate.id) {
                                        knowledge.push(candidate);
                                    }
                                }
                            }
                            Err(error) => {
                                task.note =
                                    Some(format!("external knowledge unavailable: {error}"));
                            }
                        }
                    }
                    task.diagnosis_attempts += 1;
                    task.observation = Some(observed.clone());
                    task = self.save(task)?;
                    let plan = self
                        .backend
                        .diagnose(
                            DiagnosisInput {
                                task: task.clone(),
                                config: self.config.clone(),
                                observation: observed.clone(),
                                knowledge,
                            },
                            cancellation.clone(),
                        )
                        .await?;
                    self.validate_plan(&plan, &observed)?;
                    if plan.script.generated_by_harness != self.config.execution_harness {
                        return Err(RecoveryError::Invalid(
                            "script producer differs from execution Harness".into(),
                        ));
                    }
                    text(&plan.script.generated_in_session, 256)?;
                    task.plan = Some(plan);
                    task.reused_script = false;
                    task.knowledge_id = None;
                    task.receipt = None;
                    task.verification = None;
                    self.request_plan(task)?;
                }
                RecoveryStage::AwaitingApproval => {
                    let record = self
                        .approval(id)?
                        .ok_or_else(|| service("missing durable approval"))?;
                    if record.state == ApprovalState::Approved {
                        self.execute_task(task, cancellation.clone()).await?;
                    } else if matches!(
                        record.state,
                        ApprovalState::Denied
                            | ApprovalState::Expired
                            | ApprovalState::Revoked
                            | ApprovalState::Canceled
                    ) {
                        let alternative = if task.reused_script {
                            cached_script_denial_context(&record)
                        } else {
                            None
                        };
                        if let Some(context) = alternative {
                            task.note = Some(context);
                            task.reused_script = false;
                            task.knowledge_id = None;
                            task.approval_id = None;
                            task.operation = None;
                            task.stage = RecoveryStage::Diagnosing;
                            self.save(task)?;
                        } else {
                            task.stage = RecoveryStage::Denied;
                            return self.save(task);
                        }
                    } else if record.state == ApprovalState::Unknown
                        || record.state == ApprovalState::Executing
                    {
                        task.stage = RecoveryStage::Unknown;
                        return self.save(task);
                    } else {
                        let now = self.clock.now_ms() / 1000;
                        if now >= record.request.expires_at {
                            match self.state(|s| {
                                Ok(s.approvals.expire_review(
                                    &record.request.request_id,
                                    record.revision,
                                    self.policy(&task),
                                    now,
                                )?)
                            }) {
                                Ok(_)
                                | Err(RecoveryError::Approval(approval::ApprovalError::Expired)) => {
                                }
                                Err(error) => return Err(error),
                            }
                            task.stage = RecoveryStage::Denied;
                            return self.save(task);
                        }
                        if record.review_stage == ReviewStage::NeedsHuman {
                            return Ok(task);
                        }
                        if record.review_stage == ReviewStage::WaitingHuman
                            && record.human_deadline.is_some_and(|deadline| now < deadline)
                        {
                            return Ok(task);
                        }
                        if record.review_stage == ReviewStage::ReviewingHarness {
                            if record
                                .review_deadline
                                .is_some_and(|deadline| now >= deadline)
                            {
                                self.state(|s| {
                                    Ok(s.approvals.expire_review(
                                        &record.request.request_id,
                                        record.revision,
                                        self.policy(&task),
                                        now,
                                    )?)
                                })?;
                            }
                            return Ok(task);
                        }
                        let observed = self.inspect(cancellation.clone()).await?;
                        self.validate_plan(
                            task.plan.as_ref().ok_or_else(|| service("missing plan"))?,
                            &observed,
                        )?;
                        let timeout = match self.policy(&task).reviewer {
                            approval::ReviewerConfig::HumanThenHarness {
                                review_timeout_secs,
                                ..
                            } => self.config.review_timeout_secs.min(review_timeout_secs),
                            _ => self.config.review_timeout_secs,
                        };
                        let attempt = self.state(|s| {
                            Ok(s.approvals.begin_harness_review(
                                &record.request.request_id,
                                record.revision,
                                self.policy(&task),
                                timeout,
                                self.clock.now_ms() / 1000,
                            )?)
                        })?;
                        let result = self
                            .backend
                            .review(
                                ReviewInput {
                                    request: record.request,
                                    attempt: attempt.clone(),
                                    observation: observed,
                                    reused_script: task.reused_script,
                                },
                                cancellation.clone(),
                            )
                            .await;
                        let saved = self.state(|s| match result {
                            Ok(response) => Ok(s.approvals.assess_attempt(
                                &attempt,
                                response.assessment,
                                response.identity,
                                self.policy(&task),
                                self.clock.now_ms() / 1000,
                            )?),
                            Err(error) => Ok(s.approvals.fail_review_attempt(
                                &attempt,
                                error.to_string(),
                                self.policy(&task),
                                self.clock.now_ms() / 1000,
                            )?),
                        });
                        match saved {
                            Ok(_)
                            | Err(RecoveryError::Approval(
                                approval::ApprovalError::Expired
                                | approval::ApprovalError::ReviewTimedOut,
                            )) => (),
                            Err(RecoveryError::Approval(approval::ApprovalError::Conflict)) => {
                                self.state(|s| {
                                    if s.approvals
                                        .get(&attempt.request_id)
                                        .and_then(|r| r.active_review_attempt())
                                        .as_ref()
                                        == Some(&attempt)
                                    {
                                        s.approvals.fail_review_attempt(
                                            &attempt,
                                            "review response identity or request mismatch".into(),
                                            self.policy(&task),
                                            self.clock.now_ms() / 1000,
                                        )?;
                                    }
                                    Ok(())
                                })?;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
                RecoveryStage::Executing => {
                    task.stage = RecoveryStage::Unknown;
                    return self.save(task);
                }
                RecoveryStage::Verifying => {
                    let operation = task
                        .operation
                        .clone()
                        .ok_or_else(|| service("missing operation"))?;
                    let receipt = task
                        .receipt
                        .clone()
                        .ok_or_else(|| service("missing receipt"))?;
                    // Verification has its own read-only invocation; cancellation never implies health.
                    let result = self
                        .backend
                        .verify(
                            VerificationInput {
                                target: self.config.target.clone(),
                                operation: operation.clone(),
                                receipt,
                            },
                            cancellation.clone(),
                        )
                        .await;
                    match result {
                        Ok(verification) => {
                            match self.validate_verification(&task, &verification) {
                                Ok(()) => task.verification = Some(verification),
                                Err(error) => {
                                    task.note =
                                        Some(format!("business verification rejected: {error}"))
                                }
                            }
                        }
                        Err(error) => {
                            task.note = Some(format!("business verification unavailable: {error}"));
                        }
                    }
                    task.stage = RecoveryStage::Publishing;
                    self.save(task)?;
                }
                RecoveryStage::Publishing => {
                    self.publish(task)?;
                }
                _ => return Ok(task),
            }
        }
        Err(service("recovery transition budget exhausted"))
    }
    fn request_plan(&self, mut task: RecoveryTask) -> Result<RecoveryTask, RecoveryError> {
        let plan = task.plan.as_ref().ok_or_else(|| service("missing plan"))?;
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
            action: serde_json::json!({"kind":"execute_script","executor_id":self.config.target.executor_id,
                "script":plan.script,"verification_profile":self.config.target.verification_profile,
                "required_facts":self.config.target.required_facts,"timeout_secs":self.config.target.action_timeout_secs,
                "incident_id":task.problem.incident_id,"incident_revision":task.problem.incident_revision}),
        };
        task.operation = Some(operation);
        task.approval_id = None;
        task.result_check = None;
        task.stage = RecoveryStage::AwaitingApproval;
        // The complete plan and stable operation precede the approval commit.
        // Neither request retries nor restart recovery rebuild this operation.
        let task = self.save(task)?;
        self.state(|s| {
            attach_approval(
                &mut s.tasks,
                &mut s.approvals,
                &self.config,
                task,
                self.clock.now_ms(),
            )
        })
    }
    async fn execute_task(
        &self,
        mut task: RecoveryTask,
        cancellation: Cancellation,
    ) -> Result<(), RecoveryError> {
        if !self.check_incident_before_execution(&task)? {
            return Ok(());
        }
        let observed = self.inspect(cancellation.clone()).await?;
        self.conditions(&task, &observed)?;
        self.validate_plan(
            task.plan.as_ref().ok_or_else(|| service("missing plan"))?,
            &observed,
        )?;
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let operation = task
            .operation
            .clone()
            .ok_or_else(|| service("missing operation"))?;
        let id = task
            .approval_id
            .clone()
            .ok_or_else(|| service("missing approval"))?;
        // Final observation and one-use authorization share the source's gate.
        // No remote await or action runs while that gate is held.
        let problem = task.problem.clone();
        let guard = self
            .incident_guard
            .get()
            .ok_or_else(|| service("missing incident guard"))?;
        let mut checked = false;
        let mut permit = None;
        guard.with_current(&problem, &mut |readiness| {
            self.check_ownership()?;
            if checked {
                return Err(service(
                    "incident guard repeated its authorization callback",
                ));
            }
            checked = true;
            if !self.apply_incident_readiness(&task, readiness)? || cancellation.is_cancelled() {
                return Ok(());
            }
            // Persist intent before authority; a crash never replays the action.
            task.stage = RecoveryStage::Executing;
            task.observation = Some(observed.clone());
            task = self.save(task.clone())?;
            match self.state(|s| {
                Ok(s.approvals.consume(
                    &id,
                    &operation,
                    self.policy(&task),
                    self.clock.now_ms() / 1000,
                )?)
            }) {
                Ok(consumed) => permit = Some(consumed),
                Err(error) => {
                    task.stage = if matches!(
                        error,
                        RecoveryError::Approval(
                            approval::ApprovalError::Io(_)
                                | approval::ApprovalError::Unavailable
                                | approval::ApprovalError::Corrupt(_)
                        )
                    ) {
                        RecoveryStage::Unknown
                    } else {
                        RecoveryStage::Denied
                    };
                    task.note = Some(error.to_string());
                    task = self.save(task.clone())?;
                }
            }
            Ok(())
        })?;
        if !checked {
            return Err(service("incident guard did not check current facts"));
        }
        let Some(permit) = permit else {
            return Ok(());
        };
        let result = self
            .backend
            .execute(
                AuthorizedScript {
                    permit: &permit,
                    timeout_secs: self.config.target.action_timeout_secs,
                },
                cancellation,
            )
            .await;
        let mut receipt = match result {
            Ok(receipt) => receipt,
            Err(error) => ScriptReceipt {
                operation_id: operation.operation_id.clone(),
                target_id: operation.target.clone(),
                outcome: ScriptOutcome::Unknown,
                executor_stopped: false,
                evidence_refs: vec![format!("approval:{id}")],
                summary: error.to_string(),
            },
        };
        if receipt.operation_id != operation.operation_id
            || receipt.target_id != operation.target
            || evidence(&receipt.evidence_refs).is_err()
            || receipt.summary.len() > 8192
            || !receipt.executor_stopped
        {
            receipt = ScriptReceipt {
                operation_id: operation.operation_id.clone(),
                target_id: operation.target.clone(),
                outcome: ScriptOutcome::Unknown,
                executor_stopped: false,
                evidence_refs: vec![format!("approval:{id}")],
                summary: "invalid receipt or executor not confirmed stopped".into(),
            };
        }
        let outcome = match receipt.outcome {
            ScriptOutcome::Executed => ExecutionOutcome::Executed,
            ScriptOutcome::Failed => ExecutionOutcome::Failed,
            ScriptOutcome::Unknown => ExecutionOutcome::Unknown,
        };
        self.state(|s| {
            Ok(s.approvals.complete(
                permit,
                outcome,
                receipt.summary.clone(),
                self.clock.now_ms() / 1000,
            )?)
        })?;
        task.stage = if receipt.outcome == ScriptOutcome::Executed {
            RecoveryStage::Verifying
        } else {
            RecoveryStage::Publishing
        };
        task.receipt = Some(receipt);
        self.save(task)?;
        Ok(())
    }
    fn check_incident_before_execution(&self, task: &RecoveryTask) -> Result<bool, RecoveryError> {
        let guard = self.incident_guard.get().ok_or_else(|| {
            RecoveryError::Invalid("trusted incident guard required before execution".into())
        })?;
        self.apply_incident_readiness(task, guard.check(&task.problem)?)
    }
    fn apply_incident_readiness(
        &self,
        task: &RecoveryTask,
        readiness: IncidentReadiness,
    ) -> Result<bool, RecoveryError> {
        match readiness {
            IncidentReadiness::Active { revision }
                if revision >= task.problem.incident_revision =>
            {
                Ok(true)
            }
            IncidentReadiness::Resolved { revision }
                if revision >= task.problem.incident_revision =>
            {
                let request_id = task
                    .approval_id
                    .as_ref()
                    .ok_or_else(|| service("missing approval"))?;
                self.state(|s| {
                    let record = s
                        .approvals
                        .get(request_id)
                        .ok_or_else(|| service("missing approval"))?;
                    if matches!(
                        record.state,
                        ApprovalState::Executing | ApprovalState::Unknown
                    ) {
                        return Err(RecoveryError::Invalid(
                            "dispatched operation requires result_check".into(),
                        ));
                    }
                    if matches!(
                        record.state,
                        ApprovalState::Pending
                            | ApprovalState::WaitingHuman
                            | ApprovalState::Approved
                    ) {
                        s.approvals.cancel(
                            request_id,
                            "triggering incident resolved before execution".into(),
                            self.clock.now_ms() / 1000,
                        )?;
                    }
                    Ok(())
                })?;
                let mut canceled = task.clone();
                canceled.stage = RecoveryStage::Canceled;
                canceled.note = Some(format!(
                    "triggering incident resolved at revision {revision}; unconsumed approval canceled"
                ));
                self.save(canceled)?;
                Ok(false)
            }
            IncidentReadiness::Unavailable { reason } => {
                text(&reason, 8192)?;
                Err(service(format!(
                    "incident is not currently eligible for execution: {reason}"
                )))
            }
            _ => Err(RecoveryError::Invalid(
                "incident authority returned a stale revision".into(),
            )),
        }
    }
    fn validate_verification(
        &self,
        task: &RecoveryTask,
        v: &BusinessVerification,
    ) -> Result<(), RecoveryError> {
        evidence(&v.evidence_refs)?;
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| service("missing operation"))?;
        if v.operation_id != op.operation_id
            || v.target_id != op.target
            || v.profile != self.config.target.verification_profile
            || v.verified_at_ms < task.updated_at_ms
            || v.verified_at_ms > self.clock.now_ms()
            || self.clock.now_ms().saturating_sub(v.verified_at_ms) > 30_000
        {
            return Err(RecoveryError::Invalid(
                "verification identity, profile or freshness mismatch".into(),
            ));
        }
        Ok(())
    }
    fn publish(&self, mut task: RecoveryTask) -> Result<RecoveryTask, RecoveryError> {
        let plan = task.plan.as_ref().ok_or_else(|| service("missing plan"))?;
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| service("missing operation"))?;
        let receipt = task
            .receipt
            .as_ref()
            .ok_or_else(|| service("missing receipt"))?;
        let outcome = if receipt.outcome == ScriptOutcome::Unknown || !receipt.executor_stopped {
            RepairOutcome::Unknown
        } else if receipt.outcome == ScriptOutcome::Failed {
            RepairOutcome::Failed
        } else {
            match task.verification.as_ref() {
                Some(v) if v.executor_stopped && v.healthy == Some(true) => RepairOutcome::Verified,
                Some(v) if v.executor_stopped && v.healthy == Some(false) => RepairOutcome::Failed,
                _ => RepairOutcome::Unknown,
            }
        };
        let reusable =
            plan.reusable && task.episode_count >= self.config.minimum_script_occurrences;
        {
            let id = task
                .knowledge_id
                .clone()
                .unwrap_or_else(|| format!("case-{}-{}", task.id, task.diagnosis_attempts));
            let mut refs = receipt.evidence_refs.clone();
            if let Some(v) = &task.verification {
                refs = v.evidence_refs.clone();
            }
            let timestamp = task.updated_at_ms;
            let case = RepairCase {
                id: format!("{}-{outcome:?}", op.operation_id),
                operation_id: op.operation_id.clone(),
                target_id: op.target.clone(),
                script_id: plan.script.id.clone(),
                script_version: plan.script.version,
                outcome,
                evidence_refs: refs.clone(),
                recorded_at_ms: timestamp,
            };
            let proof = if outcome == RepairOutcome::Verified {
                let v = task
                    .verification
                    .as_ref()
                    .ok_or_else(|| service("missing verification"))?;
                Some(
                    TrustedBusinessVerification::attest(
                        &op.operation_id,
                        &op.target,
                        &plan.script.id,
                        plan.script.version,
                        format!("{}:{}", self.config.target.executor_id, v.profile),
                        refs,
                        v.verified_at_ms,
                    )
                    .map_err(service)?,
                )
            } else {
                None
            };
            let conditions = self.conditions(
                &task,
                task.observation
                    .as_ref()
                    .ok_or_else(|| service("missing environment"))?,
            )?;
            self.state(|s| {
                if task.knowledge_id.is_none() {
                    s.knowledge
                        .upsert_candidate(KnowledgeCandidate {
                            id: id.clone(),
                            incident_id: task.problem.incident_id.clone(),
                            summary: plan.summary.clone(),
                            keywords: task.problem.keywords.clone(),
                            conditions,
                            script: plan.script.clone(),
                            reusable,
                            evidence_refs: task.problem.evidence_refs.clone(),
                            created_at_ms: task.created_at_ms,
                        })
                        .map_err(service)?;
                }
                s.knowledge
                    .record_outcome(&id, case, proof)
                    .map_err(service)?;
                Ok(())
            })?;
            task.knowledge_id = Some(id);
        }
        match outcome {
            RepairOutcome::Verified => task.stage = RecoveryStage::Completed,
            RepairOutcome::Unknown => task.stage = RecoveryStage::Unknown,
            RepairOutcome::Failed
                if task.reused_script && task.diagnosis_attempts < self.config.max_diagnoses =>
            {
                // Failure is terminal for this approved script. A NEW diagnosis creates a NEW approval.
                task.note = Some(format!(
                    "reused script failed; executor stopped. Receipt: {}. Verification: {:?}",
                    receipt.summary, task.verification
                ));
                task.reused_script = false;
                task.knowledge_id = None;
                task.approval_id = None;
                task.operation = None;
                task.result_check = None;
                task.stage = RecoveryStage::Diagnosing;
            }
            RepairOutcome::Failed => task.stage = RecoveryStage::Failed,
        }
        self.save(task)
    }
    /// Check operation status separately from current business health.
    /// The trusted caller must obtain execution evidence from the bound executor;
    /// health alone cannot prove execution or renew the consumed permit.
    pub fn check_result(
        &self,
        id: &str,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.check_ownership()?;
        text(&actor, 128)?;
        let _active = self.claim(id)?;
        let mut task = self.task(id)?;
        if task.revision != revision || task.stage != RecoveryStage::Unknown {
            return Err(RecoveryError::Busy);
        }
        self.validate_result_check(&task, &execution)?;
        self.validate_verification(&task, &verification)?;
        if verification.executor_stopped != execution.executor_stopped {
            return Err(RecoveryError::Invalid(
                "execution and business evidence disagree about executor termination".into(),
            ));
        }
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| service("missing operation"))?
            .clone();
        let request_id = task
            .approval_id
            .as_ref()
            .ok_or_else(|| service("missing approval"))?
            .clone();
        let authority = self.state(|s| {
            s.approvals
                .get(&request_id)
                .map(|record| record.state)
                .ok_or_else(|| service("missing approval"))
        })?;
        if !matches!(
            authority,
            ApprovalState::Unknown | ApprovalState::Executed | ApprovalState::Failed
        ) || (authority == ApprovalState::Executed
            && execution.outcome != CheckedExecution::Executed)
            || (authority == ApprovalState::Failed
                && execution.outcome != CheckedExecution::Failed
                && !(execution.outcome == CheckedExecution::NotExecuted
                    && task.result_check.as_ref().is_some_and(|record| {
                        record.execution.outcome == CheckedExecution::NotExecuted
                    })))
        {
            return Err(RecoveryError::Invalid(
                "execution result check conflicts with durable approval outcome".into(),
            ));
        }
        // Persist the independent facts before finalizing authority. An interrupted
        // commit remains Unknown with its evidence available for explicit recovery.
        task.result_check = Some(ResultCheckRecord {
            execution: execution.clone(),
            actor: actor.clone(),
        });
        task.verification = Some(verification);
        task = self.save(task)?;
        if execution.outcome == CheckedExecution::Unknown {
            return Ok(task);
        }
        let outcome = if execution.outcome == CheckedExecution::Executed {
            ExecutionOutcome::Executed
        } else {
            ExecutionOutcome::Failed
        };
        self.state(|s| {
            if s.approvals
                .get(&request_id)
                .is_some_and(|r| r.state == ApprovalState::Unknown)
            {
                s.approvals.reconcile_unknown(
                    &request_id,
                    outcome,
                    if execution.outcome == CheckedExecution::NotExecuted {
                        "executor confirmed operation was not executed".into()
                    } else {
                        format!("independent executor evidence: {:?}", execution.outcome)
                    },
                    actor,
                    self.clock.now_ms() / 1000,
                )?;
            }
            Ok(())
        })?;
        if execution.outcome == CheckedExecution::Executed
            && task
                .verification
                .as_ref()
                .is_some_and(|verification| verification.healthy.is_none())
        {
            // Execution is known, but recovery is not. Keep the prepared evidence
            // available for another result_check without republishing Unknown.
            return Ok(task);
        }
        if execution.outcome == CheckedExecution::NotExecuted {
            task.stage = RecoveryStage::Canceled;
            task.note = Some(
                "executor confirmed operation was not executed; old permit remains consumed".into(),
            );
            return self.save(task);
        }
        task.receipt = Some(ScriptReceipt {
            operation_id: op.operation_id.clone(),
            target_id: op.target.clone(),
            outcome: if execution.outcome == CheckedExecution::Executed {
                ScriptOutcome::Executed
            } else {
                ScriptOutcome::Failed
            },
            executor_stopped: true,
            evidence_refs: execution.evidence_refs,
            summary: "independent execution result check".into(),
        });
        task.stage = RecoveryStage::Publishing;
        self.save(task)
    }

    fn validate_result_check(
        &self,
        task: &RecoveryTask,
        execution: &ExecutionResultCheck,
    ) -> Result<(), RecoveryError> {
        evidence(&execution.evidence_refs)?;
        let operation = task
            .operation
            .as_ref()
            .ok_or_else(|| service("missing operation"))?;
        let now = self.clock.now_ms();
        if execution.operation_id != operation.operation_id
            || execution.target_id != operation.target
            || execution.executor_id != self.config.target.executor_id
            || execution.checked_at_ms < task.updated_at_ms
            || execution.checked_at_ms > now
            || now.saturating_sub(execution.checked_at_ms) > 30_000
            || (execution.outcome != CheckedExecution::Unknown && !execution.executor_stopped)
            || task.receipt.as_ref().is_some_and(|receipt| {
                receipt.executor_stopped
                    && match receipt.outcome {
                        ScriptOutcome::Executed => execution.outcome != CheckedExecution::Executed,
                        ScriptOutcome::Failed => execution.outcome != CheckedExecution::Failed,
                        ScriptOutcome::Unknown => false,
                    }
            })
            || task.result_check.as_ref().is_some_and(|previous| {
                previous.execution.outcome != CheckedExecution::Unknown
                    && previous.execution.outcome != execution.outcome
            })
        {
            return Err(RecoveryError::Invalid(
                "execution result check identity, freshness, termination or outcome mismatch"
                    .into(),
            ));
        }
        Ok(())
    }
}

struct Active<'a> {
    active: &'a Mutex<BTreeSet<String>>,
    id: String,
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.id);
        }
    }
}
impl Drop for RecoveryService {
    fn drop(&mut self) {
        let _ = self.calls.close();
    }
}
