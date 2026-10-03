use recuvora_core::operation::CommitReceipt;
use recuvora_core::recovery::{approval::*, engine::*, knowledge::*, workflow::*};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::Mutex,
    task::{Context, Poll, Waker},
};
fn policy() -> ApprovalPolicy {
    ApprovalPolicy {
        id: "policy".into(),
        version: 1,
        reviewer: ReviewerConfig::Harness {
            harness_id: "reviewer".into(),
        },
        delegation: "bounded repair".into(),
        allowed_targets: vec!["target".into()],
        allowed_action_kinds: vec!["repair_with_harness".into()],
        ttl_secs: 600,
    }
}
fn config() -> RecoveryConfig {
    RecoveryConfig {
        schema_version: 2,
        execution_harness: "diagnoser".into(),
        target: TargetBinding {
            target_id: "target".into(),
            executor_id: "executor".into(),
            allowed_action_kinds: vec!["restart_workload".into()],
            verification_profile: "business".into(),
            required_facts: facts(),
            action_timeout_secs: 30,
        },
        approval: policy(),
        summary_timeout_secs: 30,
        review_timeout_secs: 20,
        max_tool_calls: 4,
        max_tasks: 100,
    }
}
fn facts() -> BTreeMap<String, String> {
    BTreeMap::from([("environment".into(), "test".into())])
}
fn problem(id: &str) -> ProblemContext {
    ProblemContext {
        origin: Default::default(),
        report: None,
        incident_id: id.into(),
        incident_revision: 1,
        target_id: "target".into(),
        fingerprint: "fault".into(),
        summary: "unhealthy workload".into(),
        occurrences: 100,
        keywords: vec!["workload".into()],
        conditions: facts(),
        evidence_refs: vec!["observation:1".into()],
    }
}
fn observation() -> TargetObservation {
    TargetObservation {
        target_id: "target".into(),
        facts: facts(),
        evidence_refs: vec!["observation:current".into()],
        observed_at_ms: 100_000,
    }
}

fn run<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = Box::pin(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("fixture capabilities must be ready"),
    }
}
struct Memory {
    session: RecoverySession,
    entries: Vec<SessionEntry>,
    calls: Vec<&'static str>,
}
struct Platform {
    config: RecoveryConfig,
    memory: Mutex<Memory>,
    denied: bool,
    unknown: bool,
    summary_error: bool,
    now: u64,
}
impl Platform {
    fn new(human: bool) -> Self {
        let mut config = config();
        if human {
            config.approval.reviewer = ReviewerConfig::Human;
        }
        let session = RecoverySession::new(SessionConfig {
            recovery: config.clone(),
            approvals: ApprovalLimits::default(),
            knowledge: KnowledgeConfig::default(),
        })
        .unwrap();
        Self {
            config,
            memory: Mutex::new(Memory {
                session,
                entries: vec![],
                calls: vec![],
            }),
            denied: false,
            unknown: false,
            summary_error: false,
            now: 100_000,
        }
    }
    fn register(&self) -> String {
        self.commit(SessionCommand::Register {
            problem: problem("incident"),
            incident: incident(),
        })
        .unwrap();
        self.memory
            .lock()
            .unwrap()
            .session
            .tasks()
            .next()
            .unwrap()
            .id
            .clone()
    }
    fn log(&self, name: &'static str) {
        self.memory.lock().unwrap().calls.push(name);
    }
    fn start_approved(&self) -> String {
        let id = self.register();
        run(RecoveryEngine::advance(self, &id)).unwrap();
        let approval = self.approval(&id).unwrap().unwrap();
        self.commit(SessionCommand::HumanDecision {
            task_id: id.clone(),
            revision: approval.revision,
            decision: ApprovalDecision::Approve,
            actor: "operator".into(),
            reason: "scoped".into(),
        })
        .unwrap();
        id
    }
}
fn incident() -> IncidentEvidence {
    IncidentEvidence {
        incident_id: "incident".into(),
        revision: 1,
        active: true,
        received: false,
    }
}
fn authorization(task: &RecoveryTask, observed: TargetObservation) -> SessionCommand {
    SessionCommand::Authorize {
        task_id: task.id.clone(),
        revision: task.revision,
        observation: observed,
        incident: incident(),
        authority: TargetAuthority {
            target_id: "target".into(),
            epoch: "owner".into(),
        },
    }
}
impl RecoveryPlatform for Platform {
    fn config(&self) -> &RecoveryConfig {
        &self.config
    }
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn cancelled(&self) -> bool {
        false
    }
    fn task(&self, id: &str) -> EngineResult<RecoveryTask> {
        self.memory
            .lock()
            .unwrap()
            .session
            .task(id)
            .cloned()
            .ok_or(EngineError::Conflict)
    }
    fn approval(&self, id: &str) -> EngineResult<Option<ApprovalRecord>> {
        Ok(self.memory.lock().unwrap().session.approval(id).cloned())
    }
    fn pending_experiences(&self) -> EngineResult<Vec<ExperienceJob>> {
        Ok(self.memory.lock().unwrap().session.pending_experiences())
    }
    fn commit(&self, command: SessionCommand) -> EngineResult<Vec<SessionEffect>> {
        let mut memory = self.memory.lock().unwrap();
        let pending = memory.session.prepare(
            format!("commit-{}", memory.session.revision()),
            command,
            self.now_ms(),
        )?;
        let entry = pending.state().latest_entry().unwrap().clone();
        let receipt = CommitReceipt::confirmed(pending.request());
        let committed = pending.confirm(receipt)?;
        memory.entries.push(entry);
        memory.session = committed.state;
        Ok(committed.effects)
    }
    fn inspect(&self, _: u64) -> CapabilityFuture<'_, TargetObservation> {
        Box::pin(async {
            self.log("inspect");
            Ok(observation())
        })
    }
    fn review(&self, input: ReviewInput, _: u64) -> CapabilityFuture<'_, ReviewOutput> {
        Box::pin(async move {
            self.log("review");
            Ok(ReviewOutput {
                assessment: ModelAssessment {
                    request_id: input.request.request_id,
                    decision: if self.denied {
                        ApprovalDecision::Deny
                    } else {
                        ApprovalDecision::Approve
                    },
                    reason: "scoped review".into(),
                },
                identity: ReviewerIdentity {
                    harness_id: input.attempt.harness_id,
                    session_id: "review-session".into(),
                },
            })
        })
    }
    fn acquire_execution<'a>(
        &'a self,
        task: &'a RecoveryTask,
    ) -> CapabilityFuture<'a, ExecutionContext> {
        Box::pin(async move {
            self.log("acquire");
            Ok(ExecutionContext {
                incident: intake_evidence(&task.problem),
                authority: TargetAuthority {
                    target_id: "target".into(),
                    epoch: "owner".into(),
                },
            })
        })
    }
    fn execute<'a>(
        &'a self,
        permit: &'a ExecutionPermit,
        _: u64,
    ) -> CapabilityFuture<'a, RepairReceipt> {
        Box::pin(async move {
            self.log("execute");
            Ok(RepairReceipt {
                operation_id: permit.operation().operation_id.clone(),
                target_id: "target".into(),
                outcome: if self.unknown {
                    RepairExecutionOutcome::Unknown
                } else {
                    RepairExecutionOutcome::Executed
                },
                executor_stopped: !self.unknown,
                evidence_refs: vec!["executor:receipt".into()],
                summary: "result".into(),
                execution_trace: vec![],
            })
        })
    }
    fn release_execution(&self) {
        self.log("release");
    }
    fn verify(
        &self,
        input: VerificationInput,
        _: u64,
    ) -> CapabilityFuture<'_, BusinessVerification> {
        Box::pin(async move {
            self.log("verify");
            Ok(BusinessVerification {
                operation_id: input.operation.operation_id,
                target_id: "target".into(),
                profile: "business".into(),
                healthy: Some(true),
                executor_stopped: true,
                evidence_refs: vec!["provider:independent".into()],
                verified_at_ms: 100_000,
            })
        })
    }
    fn summarize(&self, _: ExperienceJob, _: u64) -> CapabilityFuture<'_, ExperienceReport> {
        Box::pin(async {
            self.log("summarize");
            if self.summary_error {
                return Err(EngineError::Port("summary unavailable".into()));
            }
            Ok(ExperienceReport {
                summary: "repaired".into(),
                lessons: "check readiness".into(),
                related_experience_ids: vec![],
                scriptability: Scriptability::Undetermined {
                    reason: "scriptless fixture".into(),
                },
            })
        })
    }
}

fn error_report() -> ProblemContext {
    ProblemContext {
        origin: ProblemOrigin::ErrorLog,
        report: Some(ErrorLogEvidence {
            source_id: "source-a".into(),
            generation: "generation-1".into(),
            record_id: "record-1".into(),
            sequence: 1,
            age_ms: 86_400_000,
            evidence: serde_json::json!({"exit_code": 17, "original": {"worker": "worker-42"}}),
        }),
        summary: "ERROR workload exited\n堆栈: worker(42)\n  caused by: timeout".into(),
        occurrences: 1,
        keywords: Vec::new(),
        evidence_refs: vec!["error-source:source-a:generation-1:record-1".into()],
        ..problem("error-record")
    }
}

fn intake_evidence(problem: &ProblemContext) -> IncidentEvidence {
    IncidentEvidence {
        incident_id: problem.incident_id.clone(),
        revision: problem.incident_revision,
        active: problem.origin == ProblemOrigin::Incident,
        received: problem.origin == ProblemOrigin::ErrorLog,
    }
}

fn register_report(platform: &Platform) -> String {
    let report = error_report();
    platform
        .commit(SessionCommand::Register {
            incident: intake_evidence(&report),
            problem: report,
        })
        .unwrap();
    platform
        .memory
        .lock()
        .unwrap()
        .session
        .tasks()
        .find(|task| task.problem.origin == ProblemOrigin::ErrorLog)
        .unwrap()
        .id
        .clone()
}

#[test]
fn error_report_validation_enforces_raw_byte_and_original_evidence_limits() {
    let mut boundary = error_report();
    boundary.summary = format!("{}ab", "错".repeat(2730));
    boundary.report.as_mut().unwrap().evidence = serde_json::json!({"x": "a".repeat(4088)});
    assert_eq!(boundary.summary.len(), 8192);
    assert_eq!(
        serde_json::to_vec(&boundary.report.as_ref().unwrap().evidence)
            .unwrap()
            .len(),
        4096
    );
    assert!(boundary.validate().is_ok());
    boundary.summary.push('x');
    assert!(boundary.validate().is_err());
    boundary.summary.pop();
    boundary.report.as_mut().unwrap().evidence["x"] = "a".repeat(4089).into();
    assert!(boundary.validate().is_err());

    for case in 0..6 {
        let mut invalid = error_report();
        let report = invalid.report.as_mut().unwrap();
        match case {
            0 => report.sequence = 0,
            1 => report.source_id.clear(),
            2 => report.generation = "a".repeat(129),
            3 => report.record_id = "record\0id".into(),
            4 => report.evidence = serde_json::json!([]),
            _ => invalid.report = None,
        }
        assert!(
            invalid.validate().is_err(),
            "reject malformed report case {case}"
        );
    }
    let mut nested = error_report();
    let mut value = serde_json::Value::Null;
    for _ in 0..24 {
        value = serde_json::json!({"child": value});
    }
    nested.report.as_mut().unwrap().evidence = value.clone();
    assert!(nested.validate().is_ok());
    nested.report.as_mut().unwrap().evidence = serde_json::json!({"child": value});
    assert!(nested.validate().is_err());
}

#[test]
fn received_error_report_preserves_raw_context_and_requires_approval_and_verification() {
    let platform = Platform::new(true);
    let id = register_report(&platform);
    let task = run(RecoveryEngine::advance(&platform, &id)).unwrap();
    assert_eq!(task.stage, RecoveryStage::AwaitingApproval);
    assert_eq!(task.problem, error_report());
    assert_eq!(
        task.operation.as_ref().unwrap().action["request"]["problem"],
        serde_json::to_value(error_report()).unwrap()
    );
    assert_eq!(platform.memory.lock().unwrap().calls, ["inspect"]);
    let approval = platform.approval(&id).unwrap().unwrap();
    platform
        .commit(SessionCommand::HumanDecision {
            task_id: id.clone(),
            revision: approval.revision,
            decision: ApprovalDecision::Approve,
            actor: "operator".into(),
            reason: "received report requires independently verified repair".into(),
        })
        .unwrap();
    let task = run(RecoveryEngine::advance(&platform, &id)).unwrap();
    assert_eq!(task.stage, RecoveryStage::Completed);
    let memory = platform.memory.lock().unwrap();
    assert_eq!(
        memory
            .calls
            .iter()
            .filter(|call| **call == "execute")
            .count(),
        1
    );
    assert_eq!(
        memory
            .calls
            .iter()
            .filter(|call| **call == "verify")
            .count(),
        1
    );
    let restored =
        RecoverySession::restore(memory.session.config().clone(), &memory.entries).unwrap();
    assert_eq!(restored.task(&id).unwrap().problem, error_report());
}

#[test]
fn receipt_acknowledgement_can_advance_evidence_revision_before_initial_registration() {
    let report = error_report();
    for (revision, active, received, valid) in [
        (report.incident_revision, false, true, true),
        (report.incident_revision + 1, false, true, true),
        (report.incident_revision - 1, false, true, false),
        (report.incident_revision + 1, true, false, true),
        (report.incident_revision + 1, true, true, true),
        (report.incident_revision + 1, false, false, true),
    ] {
        let platform = Platform::new(true);
        let evidence = IncidentEvidence {
            revision,
            active,
            received,
            ..intake_evidence(&report)
        };
        let result = platform.commit(SessionCommand::Register {
            problem: report.clone(),
            incident: evidence,
        });
        assert_eq!(
            result.is_ok(),
            valid,
            "revision {revision}, active {active}, received {received}"
        );
        let memory = platform.memory.lock().unwrap();
        if valid {
            assert_eq!(memory.session.tasks().next().unwrap().problem, report);
        } else {
            assert_eq!(memory.session.tasks().count(), 0);
        }
    }
    let platform = Platform::new(true);
    let active = problem("current-incident");
    let mut evidence = intake_evidence(&active);
    evidence.revision += 1;
    assert!(
        platform
            .commit(SessionCommand::Register {
                problem: active,
                incident: evidence
            })
            .is_err()
    );
    let platform = Platform::new(true);
    let mut evidence = intake_evidence(&report);
    evidence.revision += 1;
    evidence.incident_id = "another-receipt".into();
    assert!(
        platform
            .commit(SessionCommand::Register {
                problem: report,
                incident: evidence
            })
            .is_err()
    );
}

#[test]
fn legacy_liveness_flags_do_not_gate_matching_intake_authorization_or_dispatch() {
    for report in [problem("next-incident"), error_report()] {
        for (active, received) in [(true, true), (false, false), (true, false), (false, true)] {
            let platform = Platform::new(true);
            let previous = platform.start_approved();
            assert_eq!(
                run(RecoveryEngine::advance(&platform, &previous))
                    .unwrap()
                    .stage,
                RecoveryStage::Completed
            );
            let mut proof = intake_evidence(&report);
            proof.active = active;
            proof.received = received;
            for invalid_identity in [true, false] {
                let mut invalid = proof.clone();
                if invalid_identity {
                    invalid.incident_id = "wrong-report".into();
                } else {
                    invalid.revision = 0;
                }
                assert!(
                    platform
                        .commit(SessionCommand::Register {
                            problem: report.clone(),
                            incident: invalid,
                        })
                        .is_err()
                );
            }
            platform
                .commit(SessionCommand::Register {
                    problem: report.clone(),
                    incident: proof.clone(),
                })
                .unwrap();
            let id = platform
                .memory
                .lock()
                .unwrap()
                .session
                .tasks()
                .find(|task| task.problem.incident_id == report.incident_id)
                .unwrap()
                .id
                .clone();
            let task = run(RecoveryEngine::advance(&platform, &id)).unwrap();
            assert_eq!(task.stage, RecoveryStage::AwaitingApproval);
            let request = &task.operation.as_ref().unwrap().action["request"];
            assert_eq!(request["matched_experience_count"], 1);
            assert_eq!(request["experiences"].as_array().unwrap().len(), 1);
            assert_eq!(request["problem"], serde_json::to_value(&report).unwrap());
            let authorize = |task: &RecoveryTask, incident| SessionCommand::Authorize {
                task_id: id.clone(),
                revision: task.revision,
                observation: observation(),
                incident,
                authority: TargetAuthority {
                    target_id: task.problem.target_id.clone(),
                    epoch: "owner".into(),
                },
            };
            assert!(platform.commit(authorize(&task, proof.clone())).is_err());
            let approval = platform.approval(&id).unwrap().unwrap();
            platform
                .commit(SessionCommand::HumanDecision {
                    task_id: id.clone(),
                    revision: approval.revision,
                    decision: ApprovalDecision::Approve,
                    actor: "operator".into(),
                    reason: "scoped".into(),
                })
                .unwrap();
            let task = platform.task(&id).unwrap();
            let mut wrong_identity = proof.clone();
            wrong_identity.incident_id = "wrong-report".into();
            let mut stale_revision = proof.clone();
            stale_revision.revision = 0;
            for invalid in [&wrong_identity, &stale_revision] {
                assert!(platform.commit(authorize(&task, invalid.clone())).is_err());
                assert_eq!(
                    platform.approval(&id).unwrap().unwrap().state,
                    ApprovalState::Approved
                );
            }
            platform.commit(authorize(&task, proof.clone())).unwrap();
            let memory = platform.memory.lock().unwrap();
            for (send_active, send_received) in
                [(true, true), (false, false), (true, false), (false, true)]
            {
                let send = IncidentEvidence {
                    active: send_active,
                    received: send_received,
                    ..proof.clone()
                };
                assert!(
                    memory
                        .session
                        .validate_dispatch(&id, &send, platform.now)
                        .is_ok()
                );
            }
            for invalid in [&wrong_identity, &stale_revision] {
                assert!(
                    memory
                        .session
                        .validate_dispatch(&id, invalid, platform.now)
                        .is_err()
                );
            }
            assert!(
                memory
                    .session
                    .validate_dispatch(&id, &proof, approval.request.expires_at * 1000)
                    .is_err()
            );
            let restored =
                RecoverySession::restore(memory.session.config().clone(), &memory.entries).unwrap();
            assert_eq!(restored.task(&id).unwrap().problem, report);
        }
    }
}

#[test]
fn received_error_identity_binds_body_and_source_without_replacing_unknown_work() {
    let mut platform = Platform::new(false);
    platform.unknown = true;
    let id = register_report(&platform);
    let original = error_report();
    platform
        .commit(SessionCommand::Register {
            problem: original.clone(),
            incident: intake_evidence(&original),
        })
        .unwrap();
    assert_eq!(platform.memory.lock().unwrap().session.tasks().count(), 1);
    for mutate in [0, 1, 2, 3] {
        let mut changed = original.clone();
        match mutate {
            0 => changed.summary.push_str(" changed"),
            1 => changed.evidence_refs = vec!["another-source:record-1".into()],
            2 => changed.report.as_mut().unwrap().evidence["exit_code"] = 18.into(),
            _ => {
                changed.origin = ProblemOrigin::Incident;
                changed.report = None;
            }
        }
        assert!(
            platform
                .commit(SessionCommand::Register {
                    incident: intake_evidence(&changed),
                    problem: changed,
                })
                .is_err()
        );
    }
    assert_eq!(
        run(RecoveryEngine::advance(&platform, &id)).unwrap().stage,
        RecoveryStage::Unknown
    );
    run(RecoveryEngine::advance(&platform, &id)).unwrap();
    let mut next = original;
    next.incident_id = "next-error".into();
    for (active, received) in [(true, true), (false, false), (true, false), (false, true)] {
        assert!(matches!(
            platform.commit(SessionCommand::Register {
                incident: IncidentEvidence {
                    active,
                    received,
                    ..intake_evidence(&next)
                },
                problem: next.clone(),
            }),
            Err(EngineError::Recovery(RecoveryError::Busy))
        ));
        assert!(
            platform
                .memory
                .lock()
                .unwrap()
                .session
                .validate_dispatch(
                    &id,
                    &IncidentEvidence {
                        active,
                        received,
                        ..intake_evidence(&error_report())
                    },
                    platform.now
                )
                .is_err()
        );
    }
    let memory = platform.memory.lock().unwrap();
    assert_eq!(
        memory
            .calls
            .iter()
            .filter(|call| **call == "execute")
            .count(),
        1
    );
    assert!(!memory.session.releasable());
}

#[test]
fn incident_defaults_do_not_change_existing_serialized_intake_bindings() {
    let value = serde_json::to_value(problem("incident")).unwrap();
    assert!(value.get("origin").is_none());
    assert_eq!(
        serde_json::from_value::<ProblemContext>(value)
            .unwrap()
            .origin,
        ProblemOrigin::Incident
    );
    let proof = serde_json::to_value(incident()).unwrap();
    assert_eq!(proof["active"], true);
    assert!(proof.get("received").is_none());
    assert!(
        !serde_json::from_value::<IncidentEvidence>(proof)
            .unwrap()
            .received
    );
    let omitted = serde_json::from_value::<IncidentEvidence>(serde_json::json!({
        "incident_id": "incident", "revision": 1
    }))
    .unwrap();
    assert!(!omitted.active && !omitted.received);
    for encoded in [
        r#"{"incident_id":"incident","revision":1,"active":true}"#,
        r#"{"incident_id":"incident","revision":1,"active":false}"#,
        r#"{"incident_id":"incident","revision":1,"active":true,"received":true}"#,
        r#"{"incident_id":"incident","revision":1,"active":false,"received":true}"#,
    ] {
        let decoded: IncidentEvidence = serde_json::from_str(encoded).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }
}
#[test]
fn engine_owns_review_execution_verification_and_summary_sequence() {
    let p = Platform::new(false);
    let id = p.register();
    let task = run(RecoveryEngine::advance(&p, &id)).unwrap();
    assert_eq!(task.stage, RecoveryStage::Completed);
    let memory = p.memory.lock().unwrap();
    let calls: Vec<_> = memory
        .calls
        .iter()
        .filter(|v| **v != "release")
        .copied()
        .collect();
    assert_eq!(
        calls,
        vec![
            "inspect",
            "inspect",
            "review",
            "inspect",
            "acquire",
            "execute",
            "verify",
            "summarize"
        ]
    );
    assert_eq!(memory.session.knowledge_snapshot().experiences.len(), 1);
    let start = memory
        .entries
        .iter()
        .find(|e| e.command["command"] == "start")
        .unwrap();
    assert_eq!(start.approvals.len(), 1);
    assert_eq!(start.workflow.len(), 2);
    let authorize = memory
        .entries
        .iter()
        .find(|e| e.command["command"] == "authorize")
        .unwrap();
    assert_eq!(authorize.approvals.len(), 1);
    assert_eq!(authorize.workflow.len(), 1);
    let deliver = memory.entries.last().unwrap();
    assert_eq!(deliver.experiences.len(), 1);
    assert_eq!(deliver.workflow.len(), 1);
}
#[test]
fn human_wait_has_no_harness_review_and_denial_never_executes() {
    let p = Platform::new(true);
    let id = p.register();
    assert_eq!(
        run(RecoveryEngine::advance(&p, &id)).unwrap().stage,
        RecoveryStage::AwaitingApproval
    );
    let before = p.memory.lock().unwrap().calls.clone();
    run(RecoveryEngine::advance(&p, &id)).unwrap();
    assert_eq!(p.memory.lock().unwrap().calls, before);
    let approval = p.approval(&id).unwrap().unwrap();
    p.commit(SessionCommand::HumanDecision {
        task_id: id.clone(),
        revision: approval.revision,
        decision: ApprovalDecision::Deny,
        actor: "operator".into(),
        reason: "denied".into(),
    })
    .unwrap();
    assert_eq!(p.task(&id).unwrap().stage, RecoveryStage::Denied);
    assert!(!p.memory.lock().unwrap().calls.contains(&"execute"));
}
#[test]
fn rejected_authorization_does_not_consume_approval_or_mutate_task() {
    let p = Platform::new(true);
    let id = p.start_approved();
    let task = p.task(&id).unwrap();
    let mut stale = observation();
    stale.observed_at_ms = 0;
    assert!(p.commit(authorization(&task, stale)).is_err());
    assert_eq!(
        p.approval(&id).unwrap().unwrap().state,
        ApprovalState::Approved
    );
    assert_eq!(p.task(&id).unwrap().revision, task.revision);
    let effects = p.commit(authorization(&task, observation())).unwrap();
    assert!(matches!(
        effects.as_slice(),
        [SessionEffect::Execute { .. }]
    ));
    assert!(p.commit(authorization(&task, observation())).is_err());
}
#[test]
fn aggregate_receipt_binds_review_context_and_releases_nothing_on_mismatch() {
    let p = Platform::new(false);
    let id = p.register();
    let task = p.task(&id).unwrap();
    p.commit(SessionCommand::Start {
        task_id: id.clone(),
        revision: task.revision,
        observation: observation(),
    })
    .unwrap();
    let memory = p.memory.lock().unwrap();
    let revision = memory.session.task(&id).unwrap().revision;
    let first = memory
        .session
        .prepare(
            "same".into(),
            SessionCommand::Review {
                task_id: id.clone(),
                revision,
                observation: Some(observation()),
            },
            100_000,
        )
        .unwrap();
    let mut changed = observation();
    changed.evidence_refs = vec!["different:observation".into()];
    let second = memory
        .session
        .prepare(
            "same".into(),
            SessionCommand::Review {
                task_id: id.clone(),
                revision,
                observation: Some(changed),
            },
            100_000,
        )
        .unwrap();
    assert_ne!(first.request(), second.request());
    assert!(
        first
            .confirm(CommitReceipt::confirmed(second.request()))
            .is_err()
    );
    assert_eq!(
        memory.session.approval(&id).unwrap().review_stage,
        ReviewStage::ReadyHarness
    );
}
#[test]
fn aggregate_restart_preserves_authority_and_never_releases_a_permit() {
    let p = Platform::new(true);
    let id = p.start_approved();
    let original = p.task(&id).unwrap();
    p.commit(authorization(&original, observation())).unwrap();
    let memory = p.memory.lock().unwrap();
    let mut restored =
        RecoverySession::restore(memory.session.config().clone(), &memory.entries).unwrap();
    assert!(
        restored
            .prepare("not-recovered".into(), SessionCommand::Deliver, 100_000)
            .is_err()
    );
    let pending = restored
        .prepare("recover".into(), SessionCommand::Recover, 100_000)
        .unwrap();
    let receipt = CommitReceipt::confirmed(pending.request());
    let committed = pending.confirm(receipt).unwrap();
    assert!(committed.effects.is_empty());
    restored = committed.state;
    assert_eq!(restored.task(&id).unwrap().stage, RecoveryStage::Unknown);
    assert_eq!(
        restored.approval(&id).unwrap().state,
        ApprovalState::Unknown
    );
    assert_eq!(restored.task(&id).unwrap().operation, original.operation);
    let mut tampered = memory.entries.clone();
    tampered[0].now_ms += 1;
    assert!(RecoverySession::restore(memory.session.config().clone(), &tampered).is_err());
}
#[test]
fn summary_failure_is_independent_and_retry_never_reexecutes_repair() {
    let mut p = Platform::new(false);
    p.summary_error = true;
    let id = p.register();
    assert_eq!(
        run(RecoveryEngine::advance(&p, &id)).unwrap().stage,
        RecoveryStage::Completed
    );
    assert!(p.pending_experiences().unwrap()[0].last_error.is_some());
    p.summary_error = false;
    run(RecoveryEngine::summarize_pending(&p, true)).unwrap();
    assert!(p.pending_experiences().unwrap().is_empty());
    assert_eq!(
        p.memory
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|v| **v == "execute")
            .count(),
        1
    );
    let revision = p.memory.lock().unwrap().session.revision();
    run(RecoveryEngine::advance(&p, &id)).unwrap();
    assert_eq!(p.memory.lock().unwrap().session.revision(), revision);
}
#[test]
fn unknown_execution_does_not_trigger_verification_or_redispatch() {
    let mut p = Platform::new(false);
    p.unknown = true;
    let id = p.register();
    assert_eq!(
        run(RecoveryEngine::advance(&p, &id)).unwrap().stage,
        RecoveryStage::Unknown
    );
    run(RecoveryEngine::advance(&p, &id)).unwrap();
    let memory = p.memory.lock().unwrap();
    assert!(!memory.calls.contains(&"verify"));
    assert_eq!(memory.calls.iter().filter(|v| **v == "execute").count(), 1);
    assert!(!memory.session.releasable());
}

#[test]
fn expired_approved_task_finishes_without_another_external_call() {
    let mut p = Platform::new(true);
    let id = p.start_approved();
    let before = p.memory.lock().unwrap().calls.clone();
    p.now = 800_000;
    let task = run(RecoveryEngine::advance(&p, &id)).unwrap();
    assert!(task.stage.terminal());
    assert_eq!(
        p.approval(&id).unwrap().unwrap().state,
        ApprovalState::Expired
    );
    assert_eq!(p.memory.lock().unwrap().calls, before);
}
