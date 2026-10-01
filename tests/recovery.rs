use recuvora_core::{
    operation::Cancellation,
    recovery::{
        approval::{
            ApprovalDecision, ApprovalPolicy, ApprovalState, ModelAssessment, ReviewerConfig,
            ReviewerIdentity,
        },
        knowledge::{
            KnowledgeCandidate, KnowledgeQuery, KnowledgeStatus, KnowledgeStore,
            KnowledgeStoreConfig, RepairOutcome, ScriptArtifact,
        },
        workflow::*,
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[path = "workflow_support.rs"]
mod support;
use support::TestDir;

#[path = "recovery_approval.rs"]
mod approval_recovery_tests;

#[path = "recovery_guards.rs"]
mod guard_tests;

#[path = "recovery_shutdown.rs"]
mod shutdown_tests;

#[path = "recovery_naming.rs"]
mod naming_tests;

struct Clock(AtomicU64);
impl Clock {
    fn new() -> Self {
        Self(AtomicU64::new(1_000_000))
    }
    fn advance(&self, millis: u64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}
impl RecoveryClock for Clock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct FakeState {
    calls: Vec<String>,
    facts: BTreeMap<String, String>,
    outcomes: VecDeque<(ScriptOutcome, bool)>,
    verifications: VecDeque<Option<bool>>,
    review_error: bool,
    wrong_reviewer: bool,
    diagnoses: u64,
    review_timeouts: Vec<u64>,
    knowledge_seen: Vec<usize>,
}
struct Backend {
    clock: Arc<Clock>,
    state: Mutex<FakeState>,
    execution_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    execution_entered: tokio::sync::Notify,
    cancellation_seen: tokio::sync::Notify,
}

impl Backend {
    fn new(clock: Arc<Clock>) -> Self {
        Self {
            clock,
            execution_gate: Mutex::new(None),
            execution_entered: tokio::sync::Notify::new(),
            cancellation_seen: tokio::sync::Notify::new(),
            state: Mutex::new(FakeState {
                calls: vec![],
                facts: facts(),
                outcomes: VecDeque::new(),
                verifications: VecDeque::new(),
                review_error: false,
                wrong_reviewer: false,
                diagnoses: 0,
                review_timeouts: vec![],
                knowledge_seen: vec![],
            }),
        }
    }
    fn count(&self, method: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| call.as_str() == method)
            .count()
    }
}

impl RepairBackend for Backend {
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        _: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.calls.push("inspect".into());
            Ok(TargetObservation {
                target_id: target.target_id.clone(),
                facts: state.facts.clone(),
                evidence_refs: vec!["provider-observation:inspect".into()],
                observed_at_ms: self.clock.now_ms(),
            })
        })
    }
    fn diagnose(&self, input: DiagnosisInput, _: Cancellation) -> RecoveryFuture<'_, RepairPlan> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.calls.push("diagnose".into());
            state.diagnoses += 1;
            state.knowledge_seen.push(input.knowledge.len());
            for candidate in &input.knowledge {
                if candidate.id == "external-case" {
                    assert!(
                        !candidate.reusable,
                        "external knowledge cannot attest reuse"
                    );
                    assert!(
                        candidate
                            .evidence_refs
                            .iter()
                            .any(|reference| reference == "knowledge-source:external-source")
                    );
                }
            }
            Ok(RepairPlan {
                summary: "restore the workload under exact preconditions".into(),
                reusable: true,
                script: ScriptArtifact {
                    id: "workload-repair".into(),
                    version: state.diagnoses,
                    language: "sh".into(),
                    platform: input.config.target.platform,
                    source: "exit 0".into(),
                    preconditions: input.observation.facts,
                    generated_by_harness: input.config.execution_harness,
                    generated_in_session: format!("execution-session-{}", state.diagnoses),
                },
            })
        })
    }
    fn review(&self, input: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.calls.push("review".into());
            state.review_timeouts.push(
                input
                    .attempt
                    .deadline
                    .saturating_sub(self.clock.now_ms() / 1000),
            );
            if state.review_error {
                return Err(RecoveryError::Service("review service unavailable".into()));
            }
            Ok(ReviewOutput {
                assessment: ModelAssessment {
                    request_id: input.request.request_id,
                    decision: ApprovalDecision::Approve,
                    reason: "exact script and current target were reviewed".into(),
                },
                identity: ReviewerIdentity {
                    harness_id: if state.wrong_reviewer {
                        "unconfigured-reviewer".into()
                    } else {
                        input.attempt.harness_id
                    },
                    session_id: format!("review-session-{}", state.calls.len()),
                },
            })
        })
    }
    fn execute<'a>(
        &'a self,
        script: AuthorizedScript<'a>,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'a, ScriptReceipt> {
        Box::pin(async move {
            let (mut outcome, mut executor_stopped) = {
                let mut state = self.state.lock().unwrap();
                state.calls.push("execute".into());
                state
                    .outcomes
                    .pop_front()
                    .unwrap_or((ScriptOutcome::Executed, true))
            };
            let gate = self.execution_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                self.execution_entered.notify_one();
                cancellation.cancelled().await;
                self.cancellation_seen.notify_one();
                gate.notified().await;
                outcome = ScriptOutcome::Unknown;
                executor_stopped = false;
            }
            let operation = script.operation();
            assert_eq!(operation.action["kind"], "execute_script");
            assert!(!script.request_id().is_empty());
            Ok(ScriptReceipt {
                operation_id: operation.operation_id.clone(),
                target_id: operation.target.clone(),
                outcome,
                executor_stopped,
                evidence_refs: vec!["external-action:receipt".into()],
                summary: "bounded provider action result".into(),
            })
        })
    }
    fn verify(
        &self,
        input: VerificationInput,
        _: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.calls.push("verify".into());
            let healthy = state.verifications.pop_front().unwrap_or(Some(true));
            Ok(BusinessVerification {
                operation_id: input.operation.operation_id,
                target_id: input.target.target_id,
                profile: input.target.verification_profile,
                healthy,
                executor_stopped: true,
                evidence_refs: vec!["provider-observation:business-check".into()],
                verified_at_ms: self.clock.now_ms(),
            })
        })
    }
}

impl IncidentGuard for Backend {
    fn with_current(
        &self,
        problem: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError> {
        commit(IncidentReadiness::Active {
            revision: problem.incident_revision,
        })
    }
}

fn open(
    dir: &std::path::Path,
    config: RecoveryConfig,
    backend: Arc<Backend>,
    clock: Arc<Clock>,
) -> Result<Arc<RecoveryService>, RecoveryError> {
    let recovery = RecoveryService::open_with_clock(dir, config, backend.clone(), clock)?;
    recovery.bind_target_ownership(Arc::new(FileTargetOwnership::open(dir.join("ownership"))?))?;
    recovery.bind_incident_guard(backend)?;
    Ok(recovery)
}

fn facts() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("runtime_version".into(), "1".into()),
        ("fault".into(), "not-ready".into()),
    ])
}

fn policy(id: &str, reviewer: ReviewerConfig) -> ApprovalPolicy {
    ApprovalPolicy {
        id: id.into(),
        version: 1,
        reviewer,
        delegation: "review exact bounded workload repair".into(),
        allowed_targets: vec!["target-a".into()],
        allowed_action_kinds: vec!["execute_script".into()],
        ttl_secs: 600,
    }
}

fn config() -> RecoveryConfig {
    RecoveryConfig {
        schema_version: 1,
        execution_harness: "executor".into(),
        target: TargetBinding {
            target_id: "target-a".into(),
            executor_id: "target-node".into(),
            platform: "linux".into(),
            allowed_languages: vec!["sh".into()],
            diagnostic_queries: vec!["observe".into()],
            verification_profile: "workload-ready".into(),
            required_facts: BTreeMap::from([("runtime_version".into(), "1".into())]),
            action_timeout_secs: 30,
        },
        approval: policy(
            "fresh-repair",
            ReviewerConfig::Harness {
                harness_id: "reviewer".into(),
            },
        ),
        script_approval: policy(
            "reuse-repair",
            ReviewerConfig::Harness {
                harness_id: "reviewer".into(),
            },
        ),
        diagnosis_timeout_secs: 30,
        review_timeout_secs: 30,
        max_tool_calls: 8,
        max_diagnoses: 3,
        minimum_script_occurrences: 1,
        max_tasks: 32,
        max_journal_bytes: 1024 * 1024,
    }
}

fn problem(id: &str) -> ProblemContext {
    ProblemContext {
        incident_id: id.into(),
        incident_revision: 1,
        target_id: "target-a".into(),
        fingerprint: "readiness-rule".into(),
        summary: "workload has explicit unhealthy observations".into(),
        occurrences: 2,
        keywords: vec!["readiness".into()],
        conditions: facts(),
        evidence_refs: vec!["provider-observation:incident".into()],
    }
}

async fn drive(recovery: &Arc<RecoveryService>, id: &str) -> RecoveryTask {
    for _ in 0..16 {
        let task = recovery.advance(id, Cancellation::new()).await.unwrap();
        if task.stage.terminal() || task.stage == RecoveryStage::Unknown {
            return task;
        }
    }
    panic!("bounded recovery did not reach a terminal state");
}

fn execution_evidence(
    task: &RecoveryTask,
    outcome: CheckedExecution,
    clock: &Clock,
) -> ExecutionResultCheck {
    let operation = task.operation.as_ref().unwrap();
    ExecutionResultCheck {
        operation_id: operation.operation_id.clone(),
        target_id: operation.target.clone(),
        executor_id: "target-node".into(),
        outcome,
        executor_stopped: true,
        evidence_refs: vec!["executor:durable-operation-status".into()],
        checked_at_ms: clock.now_ms(),
    }
}

fn business_evidence(task: &RecoveryTask, clock: &Clock) -> BusinessVerification {
    let operation = task.operation.as_ref().unwrap();
    BusinessVerification {
        operation_id: operation.operation_id.clone(),
        target_id: operation.target.clone(),
        profile: "workload-ready".into(),
        healthy: Some(true),
        executor_stopped: true,
        evidence_refs: vec!["provider-observation:independent-health".into()],
        verified_at_ms: clock.now_ms(),
    }
}

fn remove_last_journal_entry(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    let length = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    bytes.truncate(length);
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn healthy_target_cannot_promote_unknown_failed_or_unexecuted_operations() {
    for (outcome, stage, authority) in [
        (
            CheckedExecution::Unknown,
            RecoveryStage::Unknown,
            ApprovalState::Unknown,
        ),
        (
            CheckedExecution::Failed,
            RecoveryStage::Failed,
            ApprovalState::Failed,
        ),
        (
            CheckedExecution::NotExecuted,
            RecoveryStage::Canceled,
            ApprovalState::Failed,
        ),
    ] {
        let dir = TestDir::new("recovery-result_check-health");
        let clock = Arc::new(Clock::new());
        let backend = Arc::new(Backend::new(clock.clone()));
        backend
            .state
            .lock()
            .unwrap()
            .outcomes
            .push_back((ScriptOutcome::Unknown, false));
        let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
        let task = recovery
            .submit(problem("incident-independent-execution"))
            .unwrap();
        let unknown = drive(&recovery, &task.id).await;
        let knowledge_id = unknown.knowledge_id.clone().unwrap();
        recovery
            .check_result(
                &task.id,
                unknown.revision,
                execution_evidence(&unknown, outcome, &clock),
                business_evidence(&unknown, &clock),
                "trusted-operator".into(),
            )
            .unwrap();
        let result = drive(&recovery, &task.id).await;
        assert_eq!(result.stage, stage);
        assert_eq!(
            recovery.approval(&task.id).unwrap().unwrap().state,
            authority
        );
        assert_eq!(
            result.result_check.as_ref().unwrap().execution.outcome,
            outcome
        );
        assert_eq!(
            result.result_check.as_ref().unwrap().actor,
            "trusted-operator"
        );
        assert_eq!(backend.count("execute"), 1);
        assert_eq!(backend.count("diagnose"), 1);
        recovery.shutdown().await.unwrap();
        drop(recovery);

        let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
        let recovered = drive(&recovery, &task.id).await;
        assert_eq!(recovered.stage, stage);
        assert_eq!(
            recovered.result_check.as_ref().unwrap().execution.outcome,
            outcome
        );
        assert!(recovery.resume(&task.id, recovered.revision).is_err());
        assert_eq!(backend.count("execute"), 1);
        if outcome == CheckedExecution::Unknown {
            assert!(matches!(
                recovery.submit(problem("another-incident")),
                Err(RecoveryError::Busy)
            ));
        }
        recovery.shutdown().await.unwrap();
        drop(recovery);
        let knowledge = KnowledgeStore::open(
            dir.path.join("knowledge.jsonl"),
            KnowledgeStoreConfig::default(),
        )
        .unwrap();
        let record = knowledge.get(&knowledge_id).unwrap();
        assert!(
            record
                .cases
                .iter()
                .all(|case| case.result.outcome != RepairOutcome::Verified)
        );
        if outcome == CheckedExecution::NotExecuted {
            assert_eq!(
                record.cases.len(),
                1,
                "not executed must not invent a failure case"
            );
        }
    }
}

#[tokio::test]
async fn execution_result_check_rejects_wrong_identity_stale_or_conflicting_evidence() {
    let dir = TestDir::new("recovery-result_check-validation");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    backend
        .state
        .lock()
        .unwrap()
        .outcomes
        .push_back((ScriptOutcome::Unknown, false));
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery
        .submit(problem("incident-result_check-validation"))
        .unwrap();
    let unknown = drive(&recovery, &task.id).await;
    let valid = execution_evidence(&unknown, CheckedExecution::Executed, &clock);
    let mut invalid = Vec::new();
    let mut wrong = valid.clone();
    wrong.operation_id = "other-operation".into();
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.target_id = "other-target".into();
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.executor_id = "other-node".into();
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.checked_at_ms -= 1;
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.checked_at_ms += 1;
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.evidence_refs.clear();
    invalid.push(wrong);
    let mut wrong = valid.clone();
    wrong.executor_stopped = false;
    invalid.push(wrong);
    for execution in invalid {
        assert!(
            recovery
                .check_result(
                    &task.id,
                    unknown.revision,
                    execution,
                    business_evidence(&unknown, &clock),
                    "trusted-operator".into()
                )
                .is_err()
        );
        assert_eq!(
            recovery.query(&task.id).unwrap().unwrap().revision,
            unknown.revision
        );
    }
    let mut health = business_evidence(&unknown, &clock);
    health.executor_stopped = false;
    assert!(
        recovery
            .check_result(
                &task.id,
                unknown.revision,
                valid.clone(),
                health,
                "trusted-operator".into()
            )
            .is_err()
    );
    clock.advance(30_001);
    assert!(
        recovery
            .check_result(
                &task.id,
                unknown.revision,
                valid,
                business_evidence(&unknown, &clock),
                "trusted-operator".into()
            )
            .is_err()
    );
    assert_eq!(
        recovery.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Unknown
    );
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn result_check_recovers_both_commit_boundaries_without_overwriting_execution_facts() {
    for before_authority in [true, false] {
        for outcome in [CheckedExecution::Failed, CheckedExecution::NotExecuted] {
            let dir = TestDir::new("recovery-result_check-commit");
            let clock = Arc::new(Clock::new());
            let backend = Arc::new(Backend::new(clock.clone()));
            backend
                .state
                .lock()
                .unwrap()
                .outcomes
                .push_back((ScriptOutcome::Unknown, false));
            let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
            let task = recovery
                .submit(problem("incident-result_check-commit"))
                .unwrap();
            let unknown = drive(&recovery, &task.id).await;
            recovery
                .check_result(
                    &task.id,
                    unknown.revision,
                    execution_evidence(&unknown, outcome, &clock),
                    business_evidence(&unknown, &clock),
                    "trusted-operator".into(),
                )
                .unwrap();
            recovery.shutdown().await.unwrap();
            drop(recovery);
            // Emulate a stop after the prepared Unknown evidence but before the
            // final task snapshot, with or without the authority commit.
            remove_last_journal_entry(&dir.path.join("recovery.jsonl"));
            if before_authority {
                remove_last_journal_entry(&dir.path.join("approvals/approvals.jsonl"));
            }
            let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
            let recovered = recovery.query(&task.id).unwrap().unwrap();
            assert_eq!(recovered.stage, RecoveryStage::Unknown);
            assert_eq!(
                recovered.result_check.as_ref().unwrap().execution.outcome,
                outcome
            );
            assert!(
                recovery
                    .check_result(
                        &task.id,
                        recovered.revision,
                        execution_evidence(&recovered, CheckedExecution::Executed, &clock),
                        business_evidence(&recovered, &clock),
                        "trusted-operator".into()
                    )
                    .is_err()
            );
            recovery
                .check_result(
                    &task.id,
                    recovered.revision,
                    execution_evidence(&recovered, outcome, &clock),
                    business_evidence(&recovered, &clock),
                    "trusted-operator".into(),
                )
                .unwrap();
            let result = drive(&recovery, &task.id).await;
            assert_eq!(
                result.stage,
                if outcome == CheckedExecution::NotExecuted {
                    RecoveryStage::Canceled
                } else {
                    RecoveryStage::Failed
                }
            );
            assert_eq!(backend.count("execute"), 1);
            assert_eq!(backend.count("diagnose"), 1);
            recovery.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn replay_rejects_changed_execution_result_check_identity_and_outcome() {
    let dir = TestDir::new("recovery-result_check-replay");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    backend
        .state
        .lock()
        .unwrap()
        .outcomes
        .push_back((ScriptOutcome::Unknown, false));
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery
        .submit(problem("incident-result_check-replay"))
        .unwrap();
    let unknown = drive(&recovery, &task.id).await;
    recovery
        .check_result(
            &task.id,
            unknown.revision,
            execution_evidence(&unknown, CheckedExecution::Executed, &clock),
            business_evidence(&unknown, &clock),
            "trusted-operator".into(),
        )
        .unwrap();
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let path = dir.path.join("recovery.jsonl");
    let original = std::fs::read_to_string(&path).unwrap();
    let mut entries: Vec<serde_json::Value> = original
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let last = entries.last().unwrap().clone();
    for (field, value) in [
        ("executor_id", serde_json::json!("other-node")),
        ("operation_id", serde_json::json!("other-operation")),
        ("target_id", serde_json::json!("other-target")),
        ("outcome", serde_json::json!("not_executed")),
        ("executor_stopped", serde_json::json!(false)),
        ("checked_at_ms", serde_json::json!(clock.now_ms() + 1)),
        ("evidence_refs", serde_json::json!([])),
    ] {
        let entry = entries.last_mut().unwrap();
        *entry = last.clone();
        entry["task"]["result_check"]["execution"][field] = value;
        let mut changed = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        changed.push('\n');
        std::fs::write(&path, changed).unwrap();
        assert!(matches!(
            open(&dir.path, config(), backend.clone(), clock.clone()),
            Err(RecoveryError::Corrupt(_))
        ));
    }
    std::fs::write(&path, original).unwrap();
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn result_check_cannot_reclassify_a_confirmed_execution_receipt() {
    let dir = TestDir::new("recovery-result_check-known-execution");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    backend.state.lock().unwrap().verifications.push_back(None);
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery
        .submit(problem("incident-known-execution"))
        .unwrap();
    let unknown = drive(&recovery, &task.id).await;
    assert_eq!(unknown.stage, RecoveryStage::Unknown);
    assert_eq!(
        unknown.receipt.as_ref().unwrap().outcome,
        ScriptOutcome::Executed
    );
    assert_eq!(
        recovery.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Executed
    );
    for outcome in [
        CheckedExecution::Unknown,
        CheckedExecution::Failed,
        CheckedExecution::NotExecuted,
    ] {
        assert!(
            recovery
                .check_result(
                    &task.id,
                    unknown.revision,
                    execution_evidence(&unknown, outcome, &clock),
                    business_evidence(&unknown, &clock),
                    "trusted-operator".into()
                )
                .is_err()
        );
    }
    recovery
        .check_result(
            &task.id,
            unknown.revision,
            execution_evidence(&unknown, CheckedExecution::Executed, &clock),
            business_evidence(&unknown, &clock),
            "trusted-operator".into(),
        )
        .unwrap();
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn result_check_with_unknown_health_stays_recoverable_without_republishing_unknown() {
    for known_receipt in [false, true] {
        let dir = TestDir::new("recovery-result_check-health-pending");
        let clock = Arc::new(Clock::new());
        let backend = Arc::new(Backend::new(clock.clone()));
        if known_receipt {
            backend.state.lock().unwrap().verifications.push_back(None);
        } else {
            backend
                .state
                .lock()
                .unwrap()
                .outcomes
                .push_back((ScriptOutcome::Unknown, false));
        }
        let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
        let task = recovery.submit(problem("incident-health-pending")).unwrap();
        let mut unknown = drive(&recovery, &task.id).await;
        assert_eq!(unknown.stage, RecoveryStage::Unknown);
        let knowledge_id = unknown.knowledge_id.clone().unwrap();
        let knowledge_path = dir.path.join("knowledge.jsonl");
        let original_bytes = std::fs::metadata(&knowledge_path).unwrap().len();
        for attempt in 1..=2 {
            clock.advance(1);
            let mut verification = business_evidence(&unknown, &clock);
            verification.healthy = None;
            verification.evidence_refs =
                vec![format!("provider-observation:still-unknown-{attempt}")];
            unknown = recovery
                .check_result(
                    &task.id,
                    unknown.revision,
                    execution_evidence(&unknown, CheckedExecution::Executed, &clock),
                    verification,
                    "trusted-operator".into(),
                )
                .unwrap();
            assert_eq!(unknown.stage, RecoveryStage::Unknown);
            assert_eq!(
                unknown.result_check.as_ref().unwrap().execution.outcome,
                CheckedExecution::Executed
            );
            assert_eq!(
                recovery.approval(&task.id).unwrap().unwrap().state,
                ApprovalState::Executed
            );
            unknown = drive(&recovery, &task.id).await;
            assert_eq!(unknown.stage, RecoveryStage::Unknown);
            assert_eq!(
                std::fs::metadata(&knowledge_path).unwrap().len(),
                original_bytes
            );
            assert_eq!(backend.count("execute"), 1);
            assert_eq!(backend.count("diagnose"), 1);
        }
        recovery.shutdown().await.unwrap();
        drop(recovery);

        let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
        let recovered = drive(&recovery, &task.id).await;
        assert_eq!(recovered.stage, RecoveryStage::Unknown);
        clock.advance(1);
        recovery
            .check_result(
                &task.id,
                recovered.revision,
                execution_evidence(&recovered, CheckedExecution::Executed, &clock),
                business_evidence(&recovered, &clock),
                "trusted-operator".into(),
            )
            .unwrap();
        assert_eq!(
            drive(&recovery, &task.id).await.stage,
            RecoveryStage::Completed
        );
        assert_eq!(backend.count("execute"), 1);
        assert_eq!(backend.count("diagnose"), 1);
        recovery.shutdown().await.unwrap();
        drop(recovery);
        let knowledge =
            KnowledgeStore::open(knowledge_path, KnowledgeStoreConfig::default()).unwrap();
        let record = knowledge.get(&knowledge_id).unwrap();
        assert_eq!(record.cases.len(), 2);
        assert_eq!(record.cases[0].result.outcome, RepairOutcome::Unknown);
        assert_eq!(record.cases[1].result.outcome, RepairOutcome::Verified);
    }
}

#[tokio::test]
async fn replay_rejects_prepared_result_check_conflicting_with_known_receipt() {
    let dir = TestDir::new("recovery-result_check-prepared-conflict");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    backend.state.lock().unwrap().verifications.push_back(None);
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery
        .submit(problem("incident-prepared-conflict"))
        .unwrap();
    let unknown = drive(&recovery, &task.id).await;
    assert_eq!(
        unknown.receipt.as_ref().unwrap().outcome,
        ScriptOutcome::Executed
    );
    recovery
        .check_result(
            &task.id,
            unknown.revision,
            execution_evidence(&unknown, CheckedExecution::Executed, &clock),
            business_evidence(&unknown, &clock),
            "trusted-operator".into(),
        )
        .unwrap();
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let path = dir.path.join("recovery.jsonl");
    remove_last_journal_entry(&path);
    let original = std::fs::read_to_string(&path).unwrap();
    let entries: Vec<serde_json::Value> = original
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.last().unwrap()["task"]["stage"], "unknown");
    for outcome in ["failed", "not_executed", "unknown"] {
        let mut changed = entries.clone();
        changed.last_mut().unwrap()["task"]["result_check"]["execution"]["outcome"] =
            serde_json::json!(outcome);
        let mut journal = changed
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        journal.push('\n');
        std::fs::write(&path, journal).unwrap();
        assert!(matches!(
            open(&dir.path, config(), backend.clone(), clock.clone()),
            Err(RecoveryError::Corrupt(_))
        ));
    }
    std::fs::write(&path, original).unwrap();
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let recovered = recovery.query(&task.id).unwrap().unwrap();
    assert_eq!(recovered.stage, RecoveryStage::Unknown);
    recovery
        .check_result(
            &task.id,
            recovered.revision,
            execution_evidence(&recovered, CheckedExecution::Executed, &clock),
            business_evidence(&recovered, &clock),
            "trusted-operator".into(),
        )
        .unwrap();
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn verified_repair_is_published_and_restart_reuse_only_calls_independent_reviewer() {
    let dir = TestDir::new("recovery-reuse");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery.submit(problem("incident-one")).unwrap();
    let complete = drive(&recovery, &task.id).await;
    assert_eq!(complete.stage, RecoveryStage::Completed);
    assert!(complete.verification.as_ref().unwrap().healthy == Some(true));
    assert!(complete.knowledge_id.is_some());
    let mut conditions = facts();
    conditions.insert("fault_fingerprint".into(), "readiness-rule".into());
    conditions.insert("platform".into(), "linux".into());
    let knowledge = recovery
        .knowledge(&KnowledgeQuery {
            conditions,
            keywords: vec!["readiness".into()],
            limit: 8,
        })
        .unwrap();
    assert_eq!(knowledge.len(), 1);
    assert_eq!(knowledge[0].status, KnowledgeStatus::Verified);
    assert!(knowledge[0].cases[0].verification.is_some());
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("review"), 1);
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("verify"), 1);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let recovery = open(&dir.path, config(), backend.clone(), clock).unwrap();
    let second = recovery.submit(problem("incident-two")).unwrap();
    let reused = drive(&recovery, &second.id).await;
    assert_eq!(reused.stage, RecoveryStage::Completed);
    assert!(reused.reused_script);
    assert_eq!(
        backend.count("diagnose"),
        1,
        "reusing a verified script must not wake the execution model"
    );
    assert_eq!(
        backend.count("review"),
        2,
        "every reuse requires a fresh independent review"
    );
    assert_eq!(backend.count("execute"), 2);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_stopped_execution_failure_runs_a_new_diagnosis_and_new_approval() {
    let dir = TestDir::new("recovery-failed-script");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery = open(&dir.path, config(), backend.clone(), clock).unwrap();
    let seed = recovery.submit(problem("incident-seed-script")).unwrap();
    assert_eq!(
        drive(&recovery, &seed.id).await.stage,
        RecoveryStage::Completed
    );
    backend
        .state
        .lock()
        .unwrap()
        .outcomes
        .push_back((ScriptOutcome::Failed, true));
    let task = recovery.submit(problem("incident-failed-action")).unwrap();
    let result = drive(&recovery, &task.id).await;
    assert_eq!(result.stage, RecoveryStage::Completed);
    assert_eq!(backend.count("diagnose"), 2);
    assert_eq!(backend.count("review"), 3);
    assert_eq!(backend.count("execute"), 3);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_execution_is_never_replayed_or_rediagnosed_even_after_restart() {
    let dir = TestDir::new("recovery-unknown");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    backend
        .state
        .lock()
        .unwrap()
        .outcomes
        .push_back((ScriptOutcome::Unknown, false));
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery.submit(problem("incident-unknown")).unwrap();
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Unknown
    );
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Unknown
    );
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let restarted = open(&dir.path, config(), backend.clone(), clock).unwrap();
    assert_eq!(
        drive(&restarted, &task.id).await.stage,
        RecoveryStage::Unknown
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("diagnose"), 1);
    let unknown = restarted.query(&task.id).unwrap().unwrap();
    let operation = unknown.operation.as_ref().unwrap();
    restarted
        .check_result(
            &task.id,
            unknown.revision,
            ExecutionResultCheck {
                operation_id: operation.operation_id.clone(),
                target_id: operation.target.clone(),
                executor_id: "target-node".into(),
                outcome: CheckedExecution::Executed,
                executor_stopped: true,
                evidence_refs: vec!["executor:durable-operation-result".into()],
                checked_at_ms: 1_000_000,
            },
            BusinessVerification {
                operation_id: operation.operation_id.clone(),
                target_id: operation.target.clone(),
                profile: "workload-ready".into(),
                healthy: Some(true),
                executor_stopped: true,
                evidence_refs: vec!["provider-observation:independent-result_check".into()],
                verified_at_ms: 1_000_000,
            },
            "trusted-operator".into(),
        )
        .unwrap();
    assert_eq!(
        drive(&restarted, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("diagnose"), 1);
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn business_verification_failure_disqualifies_reuse_and_wakes_diagnosis() {
    let dir = TestDir::new("recovery-verification-failure");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery = open(&dir.path, config(), backend.clone(), clock).unwrap();
    let first = recovery.submit(problem("incident-seed")).unwrap();
    drive(&recovery, &first.id).await;
    backend
        .state
        .lock()
        .unwrap()
        .verifications
        .push_back(Some(false));
    let second = recovery
        .submit(problem("incident-reuse-failed-check"))
        .unwrap();
    let result = drive(&recovery, &second.id).await;
    assert_eq!(result.stage, RecoveryStage::Completed);
    assert_eq!(backend.count("diagnose"), 2);
    assert_eq!(backend.count("execute"), 3);
    assert_eq!(backend.count("review"), 3);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn human_wait_is_persistent_nonblocking_and_falls_back_only_when_due() {
    let dir = TestDir::new("recovery-human-wait");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let mut config = config();
    config.approval.reviewer = ReviewerConfig::HumanThenHarness {
        harness_id: "reviewer".into(),
        human_wait_secs: 2,
        review_timeout_secs: 30,
    };
    let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
    let task = recovery.submit(problem("incident-human-wait")).unwrap();
    let waiting = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(waiting.stage, RecoveryStage::AwaitingApproval);
    let waiting_again = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(waiting_again.stage, RecoveryStage::AwaitingApproval);
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("review"), 0);
    assert_eq!(backend.count("execute"), 0);
    let original_request = recovery.approval(&task.id).unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), recovery.shutdown())
        .await
        .unwrap()
        .unwrap();
    drop(recovery);
    clock.advance(1000);
    let recovery = open(&dir.path, config, backend.clone(), clock.clone()).unwrap();
    let recovered = recovery.approval(&task.id).unwrap().unwrap();
    assert_eq!(
        recovered.request.request_id,
        original_request.request.request_id
    );
    assert_eq!(recovered.human_deadline, original_request.human_deadline);
    assert_eq!(
        recovery
            .advance(&task.id, Cancellation::new())
            .await
            .unwrap()
            .stage,
        RecoveryStage::AwaitingApproval
    );
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("review"), 0);
    clock.advance(1000);
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("review"), 1);
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn wrong_target_and_changed_preconditions_cannot_dispatch_an_action() {
    let dir = TestDir::new("recovery-scope");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let mut config = config();
    config.approval.reviewer = ReviewerConfig::HumanThenHarness {
        harness_id: "reviewer".into(),
        human_wait_secs: 2,
        review_timeout_secs: 30,
    };
    let recovery = open(&dir.path, config, backend.clone(), clock.clone()).unwrap();
    let mut outside = problem("incident-outside");
    outside.target_id = "unconfigured-target".into();
    assert!(recovery.submit(outside).is_err());
    let task = recovery.submit(problem("incident-changed-target")).unwrap();
    recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    backend
        .state
        .lock()
        .unwrap()
        .facts
        .insert("runtime_version".into(), "2".into());
    clock.advance(2000);
    let _ = recovery.advance(&task.id, Cancellation::new()).await;
    assert_eq!(backend.count("execute"), 0);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn review_failure_or_wrong_identity_never_grants_execution() {
    for wrong_identity in [false, true] {
        let dir = TestDir::new("recovery-review-failure");
        let clock = Arc::new(Clock::new());
        let backend = Arc::new(Backend::new(clock.clone()));
        {
            let mut state = backend.state.lock().unwrap();
            state.review_error = !wrong_identity;
            state.wrong_reviewer = wrong_identity;
        }
        let recovery = open(&dir.path, config(), backend.clone(), clock).unwrap();
        let task = recovery
            .submit(problem("incident-review-unavailable"))
            .unwrap();
        let _ = recovery.advance(&task.id, Cancellation::new()).await;
        let _ = recovery.advance(&task.id, Cancellation::new()).await;
        assert_eq!(backend.count("execute"), 0);
        assert_eq!(backend.count("review"), 1);
        recovery.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn recovered_approval_requires_explicit_resume_and_does_not_execute_on_human_decision() {
    let dir = TestDir::new("recovery-approved-restart");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let mut config = config();
    config.approval.reviewer = ReviewerConfig::Human;
    let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
    let task = recovery.submit(problem("incident-human-approved")).unwrap();
    assert_eq!(
        recovery
            .advance(&task.id, Cancellation::new())
            .await
            .unwrap()
            .stage,
        RecoveryStage::AwaitingApproval
    );
    let approval = recovery.approval(&task.id).unwrap().unwrap();
    recovery
        .decide_human(
            &task.id,
            approval.revision,
            ApprovalDecision::Approve,
            "operator".into(),
            "reviewed exact action".into(),
        )
        .unwrap();
    assert_eq!(backend.count("execute"), 0);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let restarted = open(&dir.path, config, backend.clone(), clock).unwrap();
    let paused = restarted
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(paused.stage, RecoveryStage::Paused);
    assert_eq!(backend.count("execute"), 0);
    assert!(restarted.resume(&task.id, paused.revision - 1).is_err());
    restarted.resume(&task.id, paused.revision).unwrap();
    assert_eq!(
        drive(&restarted, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("execute"), 1);
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropped_advance_cancels_and_shutdown_drains_the_owned_execution() {
    let dir = TestDir::new("recovery-drop-drain");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let gate = Arc::new(tokio::sync::Notify::new());
    *backend.execution_gate.lock().unwrap() = Some(gate.clone());
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery.submit(problem("incident-drain")).unwrap();
    let caller = tokio::spawn({
        let recovery = recovery.clone();
        let id = task.id.clone();
        async move { recovery.advance(&id, Cancellation::new()).await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        backend.execution_entered.notified(),
    )
    .await
    .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        backend.cancellation_seen.notified(),
    )
    .await
    .unwrap();
    let shutdown = tokio::spawn({
        let recovery = recovery.clone();
        async move { recovery.shutdown().await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown must retain ownership until execution cleanup finishes"
    );
    gate.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(recovery);
    let restarted = open(&dir.path, config(), backend.clone(), clock).unwrap();
    assert_eq!(
        drive(&restarted, &task.id).await.stage,
        RecoveryStage::Unknown
    );
    assert_eq!(backend.count("execute"), 1);
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn knowledge_publication_conflict_retries_only_publication_even_after_restart() {
    let dir = TestDir::new("recovery-publish-conflict");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = recovery
        .submit(problem("incident-publish-conflict"))
        .unwrap();
    recovery.shutdown().await.unwrap();
    drop(recovery);
    // Seed an existing, incompatible candidate using the public knowledge API.
    // It deterministically rejects this workflow's publication after execution.
    let mut knowledge = KnowledgeStore::open(
        dir.path.join("knowledge.jsonl"),
        KnowledgeStoreConfig::default(),
    )
    .unwrap();
    knowledge
        .upsert_candidate(KnowledgeCandidate {
            id: format!("case-{}-1", task.id),
            incident_id: "another-incident".into(),
            summary: "existing unrelated candidate".into(),
            reusable: false,
            keywords: vec!["unrelated".into()],
            conditions: facts(),
            script: ScriptArtifact {
                id: "other-script".into(),
                version: 1,
                language: "sh".into(),
                platform: "linux".into(),
                source: "exit 1".into(),
                preconditions: facts(),
                generated_by_harness: "executor".into(),
                generated_in_session: "prior-session".into(),
            },
            evidence_refs: vec!["provider-observation:prior-candidate".into()],
            created_at_ms: clock.now_ms(),
        })
        .unwrap();
    knowledge.close().unwrap();
    drop(knowledge);
    let recovery = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    assert!(
        recovery
            .advance(&task.id, Cancellation::new())
            .await
            .is_err()
    );
    assert_eq!(
        recovery.query(&task.id).unwrap().unwrap().stage,
        RecoveryStage::Publishing
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("verify"), 1);
    assert!(
        recovery
            .advance(&task.id, Cancellation::new())
            .await
            .is_err()
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("diagnose"), 1);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let restarted = open(&dir.path, config(), backend.clone(), clock).unwrap();
    assert!(
        restarted
            .advance(&task.id, Cancellation::new())
            .await
            .is_err()
    );
    assert_eq!(
        restarted.query(&task.id).unwrap().unwrap().stage,
        RecoveryStage::Publishing
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("verify"), 1);
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn human_handoff_respects_the_shorter_policy_review_timeout() {
    let dir = TestDir::new("recovery-policy-timeout");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let mut config = config();
    config.review_timeout_secs = 60;
    config.approval.reviewer = ReviewerConfig::HumanThenHarness {
        harness_id: "reviewer".into(),
        human_wait_secs: 15,
        review_timeout_secs: 15,
    };
    let recovery = open(&dir.path, config, backend.clone(), clock.clone()).unwrap();
    let task = recovery
        .submit(problem("incident-policy-deadline"))
        .unwrap();
    assert_eq!(
        recovery
            .advance(&task.id, Cancellation::new())
            .await
            .unwrap()
            .stage,
        RecoveryStage::AwaitingApproval
    );
    clock.advance(14_999);
    assert_eq!(
        recovery
            .advance(&task.id, Cancellation::new())
            .await
            .unwrap()
            .stage,
        RecoveryStage::AwaitingApproval
    );
    assert_eq!(backend.count("review"), 0);
    clock.advance(1);
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.state.lock().unwrap().review_timeouts, vec![15]);
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn every_solution_is_published_but_reuse_waits_for_repeated_episodes() {
    let dir = TestDir::new("recovery-episode-frequency");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let mut config = config();
    config.minimum_script_occurrences = 2;
    let recovery = open(&dir.path, config, backend.clone(), clock).unwrap();
    let mut first = problem("incident-frequency-first");
    first.occurrences = 1;
    let first = recovery.submit(first).unwrap();
    let result = drive(&recovery, &first.id).await;
    assert_eq!(result.stage, RecoveryStage::Completed);
    assert_eq!(result.problem.occurrences, 1);
    assert_eq!(result.episode_count, 1);
    assert!(result.knowledge_id.is_some());
    let mut conditions = facts();
    conditions.insert("fault_fingerprint".into(), "readiness-rule".into());
    conditions.insert("platform".into(), "linux".into());
    let query = KnowledgeQuery {
        conditions,
        keywords: vec!["readiness".into()],
        limit: 8,
    };
    let first_cases = recovery.knowledge(&query).unwrap();
    assert_eq!(first_cases.len(), 1);
    assert!(!first_cases[0].candidate.reusable);
    let mut second = problem("incident-frequency-second");
    second.occurrences = 1;
    let second = recovery.submit(second).unwrap();
    assert_eq!(second.problem.occurrences, 1);
    assert_eq!(second.episode_count, 2);
    let result = drive(&recovery, &second.id).await;
    assert_eq!(result.stage, RecoveryStage::Completed);
    assert!(result.knowledge_id.is_some());
    assert_eq!(backend.count("diagnose"), 2);
    assert_eq!(backend.state.lock().unwrap().knowledge_seen, vec![0, 1]);
    let cases = recovery.knowledge(&query).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(
        cases.iter().filter(|case| case.candidate.reusable).count(),
        1
    );
    let mut third = problem("incident-frequency-third");
    third.occurrences = 1;
    let third = recovery.submit(third).unwrap();
    let result = drive(&recovery, &third.id).await;
    assert_eq!(result.stage, RecoveryStage::Completed);
    assert_eq!(result.problem.occurrences, 1);
    assert_eq!(result.episode_count, 3);
    assert!(result.reused_script);
    assert_eq!(
        backend.count("diagnose"),
        2,
        "third occurrence should reuse the verified script"
    );
    assert_eq!(backend.count("review"), 3);
    assert_eq!(backend.count("execute"), 3);
    recovery.shutdown().await.unwrap();
}
