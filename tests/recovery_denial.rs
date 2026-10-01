use recuvora_core::{
    operation::Cancellation,
    recovery::{
        approval::{
            ApprovalDecision, ApprovalError, ApprovalPolicy, ApprovalState, AssessmentSource,
            ModelAssessment, ReviewerConfig, ReviewerIdentity,
        },
        knowledge::ScriptArtifact,
        workflow::*,
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

#[path = "workflow_support.rs"]
mod support;
use support::TestDir;

struct Clock;
impl RecoveryClock for Clock {
    fn now_ms(&self) -> u64 {
        1_000_000
    }
}

enum ReviewStep {
    Approve,
    Deny,
    Unavailable,
    LateApprove,
}

const INAPPLICABLE: &str = "cached script assumes a service layout absent from current evidence";

#[derive(Default)]
struct State {
    diagnoses: Vec<Option<String>>,
    executions: Vec<String>,
    reviews: Vec<(String, bool, String)>,
    steps: VecDeque<ReviewStep>,
}

#[derive(Default)]
struct Backend {
    state: Mutex<State>,
    review_entered: tokio::sync::Notify,
    review_release: tokio::sync::Notify,
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

impl RepairBackend for Backend {
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        _: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        Box::pin(async move {
            Ok(TargetObservation {
                target_id: target.target_id.clone(),
                facts: facts(),
                evidence_refs: vec!["provider-observation:inspection".into()],
                observed_at_ms: 1_000_000,
            })
        })
    }

    fn diagnose(&self, input: DiagnosisInput, _: Cancellation) -> RecoveryFuture<'_, RepairPlan> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.diagnoses.push(input.task.note);
            let version = state.diagnoses.len() as u64;
            Ok(RepairPlan {
                summary: "restore the workload under the original delegation".into(),
                reusable: true,
                script: ScriptArtifact {
                    id: "workload-repair".into(),
                    version,
                    language: "sh".into(),
                    platform: input.config.target.platform,
                    source: "exit 0".into(),
                    preconditions: input.observation.facts,
                    generated_by_harness: input.config.execution_harness,
                    generated_in_session: format!("execution-session-{version}"),
                },
            })
        })
    }

    fn review(&self, input: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        Box::pin(async move {
            let (step, session_id) = {
                let mut state = self.state.lock().unwrap();
                let session = format!("review-session-{}", state.reviews.len() + 1);
                state.reviews.push((
                    input.request.request_id.clone(),
                    input.reused_script,
                    session.clone(),
                ));
                (
                    state.steps.pop_front().unwrap_or(ReviewStep::Approve),
                    session,
                )
            };
            if matches!(step, ReviewStep::Unavailable) {
                return Err(RecoveryError::Service("reviewer is unavailable".into()));
            }
            if matches!(step, ReviewStep::LateApprove) {
                self.review_entered.notify_one();
                self.review_release.notified().await;
            }
            let denied = matches!(step, ReviewStep::Deny);
            Ok(ReviewOutput {
                assessment: ModelAssessment {
                    request_id: input.request.request_id,
                    decision: if denied {
                        ApprovalDecision::Deny
                    } else {
                        ApprovalDecision::Approve
                    },
                    reason: if denied {
                        INAPPLICABLE
                    } else {
                        "reviewed exact plan"
                    }
                    .into(),
                },
                identity: ReviewerIdentity {
                    harness_id: input.attempt.harness_id,
                    session_id,
                },
            })
        })
    }

    fn execute<'a>(
        &'a self,
        script: AuthorizedScript<'a>,
        _: Cancellation,
    ) -> RecoveryFuture<'a, ScriptReceipt> {
        Box::pin(async move {
            let operation = script.operation();
            self.state
                .lock()
                .unwrap()
                .executions
                .push(operation.operation_id.clone());
            Ok(ScriptReceipt {
                operation_id: operation.operation_id.clone(),
                target_id: operation.target.clone(),
                outcome: ScriptOutcome::Executed,
                executor_stopped: true,
                evidence_refs: vec!["external-action:receipt".into()],
                summary: "bounded action completed".into(),
            })
        })
    }

    fn verify(
        &self,
        input: VerificationInput,
        _: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        Box::pin(async move {
            Ok(BusinessVerification {
                operation_id: input.operation.operation_id,
                target_id: input.target.target_id,
                profile: input.target.verification_profile,
                healthy: Some(true),
                executor_stopped: true,
                evidence_refs: vec!["provider-observation:business-verification".into()],
                verified_at_ms: 1_000_000,
            })
        })
    }
}

fn facts() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("runtime_version".into(), "1".into()),
        ("fault".into(), "not-ready".into()),
    ])
}

fn policy(id: &str) -> ApprovalPolicy {
    ApprovalPolicy {
        id: id.into(),
        version: 1,
        reviewer: ReviewerConfig::Harness {
            harness_id: "reviewer".into(),
        },
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
        approval: policy("fresh-repair"),
        script_approval: policy("reuse-repair"),
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
        summary: "workload is explicitly unhealthy".into(),
        occurrences: 2,
        keywords: vec!["readiness".into()],
        conditions: facts(),
        evidence_refs: vec!["provider-observation:incident".into()],
    }
}

fn open(dir: &TestDir, backend: &Arc<Backend>) -> Arc<RecoveryService> {
    let recovery =
        RecoveryService::open_with_clock(&dir.path, config(), backend.clone(), Arc::new(Clock))
            .unwrap();
    recovery.bind_incident_guard(backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    recovery
}

async fn seeded(dir: &TestDir, backend: &Arc<Backend>) -> Arc<RecoveryService> {
    let recovery = open(dir, backend);
    let task = recovery.submit(problem("incident-original")).unwrap();
    let completed = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(completed.stage, RecoveryStage::Completed);
    recovery
}

fn deny_human(recovery: &RecoveryService, id: &str) {
    let record = recovery.approval(id).unwrap().unwrap();
    recovery
        .decide_human(
            id,
            record.revision,
            ApprovalDecision::Deny,
            "operator".into(),
            "do not repair this incident".into(),
        )
        .unwrap();
}

fn assert_human_denial(recovery: &RecoveryService, id: &str, backend: &Backend) {
    let record = recovery.approval(id).unwrap().unwrap();
    assert_eq!(record.state, ApprovalState::Denied);
    let assessment = record.assessment.unwrap();
    assert_eq!(assessment.decision, ApprovalDecision::Deny);
    assert_eq!(assessment.reason, "do not repair this incident");
    assert_eq!(
        assessment.reviewer,
        AssessmentSource::Human {
            actor: "operator".into()
        }
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(
        state.diagnoses.len(),
        1,
        "human denial must not launch another diagnosis"
    );
    assert_eq!(
        state.executions.len(),
        1,
        "human denial must not launch another action"
    );
    assert_eq!(
        state.reviews.len(),
        2,
        "human denial must not request another approval"
    );
}

#[tokio::test]
async fn human_denial_of_reused_script_is_terminal_and_survives_restart() {
    let dir = TestDir::new("recovery-human-denial");
    let backend = Arc::new(Backend::default());
    let recovery = seeded(&dir, &backend).await;
    backend
        .state
        .lock()
        .unwrap()
        .steps
        .push_back(ReviewStep::Unavailable);
    let task = recovery.submit(problem("incident-repeat")).unwrap();
    let waiting = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(waiting.stage, RecoveryStage::AwaitingApproval);
    assert!(waiting.reused_script);
    deny_human(&recovery, &task.id);
    let denied = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(denied.stage, RecoveryStage::Denied);
    assert_eq!(denied.approval_id, waiting.approval_id);
    assert_human_denial(&recovery, &task.id, &backend);
    recovery.shutdown().await.unwrap();
    let reopened = open(&dir, &backend);
    let still_denied = reopened
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(still_denied.stage, RecoveryStage::Denied);
    assert_human_denial(&reopened, &task.id, &backend);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn late_harness_approval_cannot_undo_human_denial_or_restart_diagnosis() {
    let dir = TestDir::new("recovery-late-review-denial");
    let backend = Arc::new(Backend::default());
    let recovery = seeded(&dir, &backend).await;
    backend
        .state
        .lock()
        .unwrap()
        .steps
        .push_back(ReviewStep::LateApprove);
    let task = recovery.submit(problem("incident-repeat")).unwrap();
    let run_recovery = recovery.clone();
    let id = task.id.clone();
    let running = tokio::spawn(async move { run_recovery.advance(&id, Cancellation::new()).await });
    tokio::time::timeout(Duration::from_secs(5), backend.review_entered.notified())
        .await
        .unwrap();
    assert!(recovery.query(&task.id).unwrap().unwrap().reused_script);
    deny_human(&recovery, &task.id);
    backend.review_release.notify_one();
    let late = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        late,
        Err(RecoveryError::Approval(ApprovalError::InvalidState(
            ApprovalState::Denied
        )))
    ));
    assert_human_denial(&recovery, &task.id, &backend);
    let denied = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(denied.stage, RecoveryStage::Denied);
    assert_human_denial(&recovery, &task.id, &backend);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn harness_inapplicability_preserves_assessment_and_requires_fresh_approval() {
    let dir = TestDir::new("recovery-harness-denial");
    let backend = Arc::new(Backend::default());
    let recovery = seeded(&dir, &backend).await;
    backend
        .state
        .lock()
        .unwrap()
        .steps
        .push_back(ReviewStep::Deny);
    let task = recovery.submit(problem("incident-repeat")).unwrap();
    let completed = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(completed.stage, RecoveryStage::Completed);
    assert!(!completed.reused_script);
    let (diagnoses, executions, reviews) = {
        let state = backend.state.lock().unwrap();
        (
            state.diagnoses.clone(),
            state.executions.clone(),
            state.reviews.clone(),
        )
    };
    assert_eq!(diagnoses.len(), 2);
    assert_eq!(executions.len(), 2);
    assert_eq!(reviews.len(), 3);
    assert!(reviews[1].1);
    assert!(!reviews[2].1);
    assert_ne!(reviews[1].0, reviews[2].0);
    assert_eq!(completed.approval_id.as_ref(), Some(&reviews[2].0));
    let context: serde_json::Value = serde_json::from_str(diagnoses[1].as_ref().unwrap()).unwrap();
    assert_eq!(context["kind"], "cached_script_rejected");
    assert_eq!(context["request_id"], reviews[1].0);
    assert_eq!(context["assessment"]["decision"], "deny");
    assert_eq!(context["assessment"]["reason"], INAPPLICABLE);
    assert_eq!(context["assessment"]["reviewer"]["source"], "harness");
    assert_eq!(context["assessment"]["reviewer"]["harness_id"], "reviewer");
    assert_eq!(
        context["assessment"]["reviewer"]["session_id"],
        reviews[1].2
    );
    let authority = recovery.approval(&task.id).unwrap().unwrap();
    assert_eq!(
        authority.assessment.unwrap().decision,
        ApprovalDecision::Approve
    );
    recovery.shutdown().await.unwrap();
}
