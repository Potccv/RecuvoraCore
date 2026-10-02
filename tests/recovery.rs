use recuvora_core::operation::{CommitReceipt, Prepared};
use recuvora_core::recovery::{approval::*, knowledge::*, workflow::*};
use std::collections::BTreeMap;

fn commit<S, E>(pending: Prepared<S, E>) -> (S, Vec<E>) {
    let receipt = CommitReceipt::confirmed(pending.request());
    let committed = pending.confirm(receipt).unwrap();
    (committed.state, committed.effects)
}
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
fn artifact(version: u64) -> RepairArtifact {
    RepairArtifact {
        id: "artifact".into(),
        version,
        kind: "restart_workload".into(),
        payload: serde_json::json!({"workload":"demo"}),
        preconditions: facts(),
        generated_by_harness: "diagnoser".into(),
        generated_in_session: "summary-session".into(),
    }
}
struct Flow {
    state: RecoveryState,
    knowledge: KnowledgeState,
    approvals: ApprovalLedger,
    id: String,
}
impl Flow {
    fn new() -> Self {
        Self {
            state: RecoveryState::new(config()).unwrap(),
            knowledge: KnowledgeState::new(KnowledgeConfig::default()).unwrap(),
            approvals: ApprovalLedger::new(ApprovalLimits::default()).unwrap(),
            id: String::new(),
        }
    }
    fn task(&self) -> &RecoveryTask {
        self.state.task(&self.id).unwrap()
    }
    fn step(&mut self, event: RecoveryEvent) -> Vec<RecoveryEffect> {
        let pending = self
            .state
            .prepare(
                format!("workflow-{}", self.state.revision()),
                RecoveryCommand::Event(event),
                100_000,
                &self.knowledge,
            )
            .unwrap();
        let (state, effects) = commit(pending);
        self.state = state;
        effects
    }
    fn register(&mut self, id: &str) {
        self.step(RecoveryEvent::Register {
            problem: problem(id),
            incident: IncidentEvidence {
                incident_id: id.into(),
                revision: 1,
                active: true,
            },
        });
        self.id = self
            .state
            .tasks()
            .find(|t| t.problem.incident_id == id)
            .unwrap()
            .id
            .clone();
    }
    fn start(&mut self) -> ProposedOperation {
        let pending = self
            .state
            .prepare(
                format!("start-{}", self.state.revision()),
                RecoveryCommand::StartRepair {
                    task_id: self.id.clone(),
                    revision: self.task().revision,
                    observation: observation(),
                },
                100_000,
                &self.knowledge,
            )
            .unwrap();
        assert_eq!(self.task().stage, RecoveryStage::Queued);
        let (state, effects) = commit(pending);
        self.state = state;
        assert!(matches!(
            effects.as_slice(),
            [RecoveryEffect::RequestApproval { .. }]
        ));
        self.task().operation.clone().unwrap()
    }
    fn prepare_action(&mut self) -> RepairArtifact {
        let op = self.task().operation.as_ref().unwrap();
        let mut action = artifact(1);
        action.id = format!("{}-action", op.operation_id);
        action.generated_in_session = op.operation_id.clone();
        self.step(RecoveryEvent::RepairActionPrepared {
            task_id: self.id.clone(),
            revision: self.task().revision,
            action: action.clone(),
        });
        action
    }
    fn attach_and_approve(&mut self, op: ProposedOperation) -> String {
        let active_policy = self.state.config().approval.clone();
        let pending = self
            .approvals
            .prepare_request(
                format!("request-{}", self.approvals.revision()),
                op,
                active_policy.clone(),
                100,
            )
            .unwrap();
        self.approvals = commit(pending).0;
        let record = self.approvals.list().last().unwrap().clone();
        let id = record.request.request_id.clone();
        self.step(RecoveryEvent::ApprovalAttached {
            task_id: self.id.clone(),
            revision: self.task().revision,
            record,
        });
        let pending = self
            .approvals
            .prepare(
                format!("review-{}", self.approvals.revision()),
                ApprovalEvent::Changed {
                    request_id: id.clone(),
                    change: ApprovalChange::BeginReview {
                        expected_revision: self.approvals.get(&id).unwrap().revision,
                        timeout_secs: 10,
                    },
                },
                Some(&active_policy),
                100,
            )
            .unwrap();
        let (approvals, mut effects) = commit(pending);
        self.approvals = approvals;
        let ApprovalEffect::Review(attempt) = effects.remove(0) else {
            panic!("review effect")
        };
        let pending = self
            .approvals
            .prepare(
                format!("assess-{}", self.approvals.revision()),
                ApprovalEvent::Changed {
                    request_id: id.clone(),
                    change: ApprovalChange::AssessAttempt {
                        attempt,
                        assessment: ApprovalAssessment {
                            decision: ApprovalDecision::Approve,
                            reason: "valid".into(),
                            reviewer: AssessmentSource::Harness {
                                harness_id: "reviewer".into(),
                                session_id: "review-session".into(),
                            },
                        },
                    },
                },
                Some(&active_policy),
                100,
            )
            .unwrap();
        self.approvals = commit(pending).0;
        id
    }
    fn consume(&mut self, id: &str) -> ExecutionPermit {
        let active_policy = self.approvals.get(id).unwrap().request.policy.clone();
        let pending = self
            .approvals
            .prepare(
                format!("consume-{}", self.approvals.revision()),
                ApprovalEvent::Changed {
                    request_id: id.into(),
                    change: ApprovalChange::Consume,
                },
                Some(&active_policy),
                100,
            )
            .unwrap();
        let (approvals, mut effects) = commit(pending);
        self.approvals = approvals;
        match effects.remove(0) {
            ApprovalEffect::Execute(permit) => permit,
            _ => panic!("expected permit"),
        }
    }
    fn authorization(&self, permit: ExecutionPermit, id: &str) -> RecoveryCommand {
        RecoveryCommand::AuthorizeExecution {
            task_id: self.id.clone(),
            revision: self.task().revision,
            permit,
            approval: self.approvals.get(id).unwrap().clone(),
            observation: observation(),
            incident: IncidentEvidence {
                incident_id: self.task().problem.incident_id.clone(),
                revision: 1,
                active: true,
            },
            authority: TargetAuthority {
                target_id: "target".into(),
                epoch: "ownership-1".into(),
            },
        }
    }
    fn dispatch(&mut self, id: &str) -> ExecutionPermit {
        let permit = self.consume(id);
        let pending = self
            .state
            .prepare(
                format!("dispatch-{}", self.state.revision()),
                self.authorization(permit, id),
                100_000,
                &self.knowledge,
            )
            .unwrap();
        assert_eq!(self.task().stage, RecoveryStage::AwaitingApproval);
        let (state, mut effects) = commit(pending);
        self.state = state;
        match effects.remove(0) {
            RecoveryEffect::Execute { permit, .. } => permit,
            _ => panic!("dispatch effect"),
        }
    }
    fn record(&mut self, permit: ExecutionPermit, outcome: ExecutionOutcome) {
        let id = permit.request_id().to_owned();
        self.approvals = commit(
            self.approvals
                .prepare_complete(
                    format!("complete-{}", self.approvals.revision()),
                    permit,
                    outcome,
                    "executor evidence".into(),
                    100,
                )
                .unwrap(),
        )
        .0;
        let script_outcome = match outcome {
            ExecutionOutcome::Executed => RepairExecutionOutcome::Executed,
            ExecutionOutcome::Failed => RepairExecutionOutcome::Failed,
            ExecutionOutcome::Unknown => RepairExecutionOutcome::Unknown,
        };
        let op = self.task().operation.as_ref().unwrap();
        self.step(RecoveryEvent::ExecutionRecorded {
            task_id: self.id.clone(),
            revision: self.task().revision,
            receipt: RepairReceipt {
                execution_trace: self
                    .state
                    .repair_action(&op.operation_id)
                    .cloned()
                    .into_iter()
                    .collect(),
                operation_id: op.operation_id.clone(),
                target_id: "target".into(),
                outcome: script_outcome,
                executor_stopped: outcome != ExecutionOutcome::Unknown,
                evidence_refs: vec!["execution:receipt".into()],
                summary: "executor result".into(),
            },
            approval: self.approvals.get(&id).unwrap().clone(),
        });
    }
    fn verify(&mut self, healthy: Option<bool>) {
        self.step(RecoveryEvent::VerificationRecorded {
            task_id: self.id.clone(),
            revision: self.task().revision,
            verification: self.verification(healthy),
        });
    }
    fn verification(&self, healthy: Option<bool>) -> BusinessVerification {
        BusinessVerification {
            operation_id: self.task().operation.as_ref().unwrap().operation_id.clone(),
            target_id: "target".into(),
            profile: "business".into(),
            healthy,
            executor_stopped: true,
            evidence_refs: vec!["business:receipt".into()],
            verified_at_ms: 100_000,
        }
    }
    fn started() -> (Self, String, ExecutionPermit) {
        let mut flow = Self::new();
        flow.register("incident-1");
        let op = flow.start();
        let id = flow.attach_and_approve(op);
        let permit = flow.dispatch(&id);
        flow.prepare_action();
        (flow, id, permit)
    }
}

#[test]
fn unknown_execution_blocks_new_tasks_and_never_dispatches_on_restore() {
    let (mut flow, _, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Unknown);
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    assert!(flow.state.is_quarantined(
        &format!(
            "{}-action",
            flow.task().operation.as_ref().unwrap().operation_id
        ),
        1
    ));
    let restored = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    assert!(restored.recovery_required());
    assert_eq!(
        restored.task(&flow.id).unwrap().stage,
        RecoveryStage::Unknown
    );
    flow.state = restored;
    assert!(flow.step(RecoveryEvent::Recover).is_empty());
    let event = RecoveryEvent::Register {
        problem: problem("incident-2"),
        incident: IncidentEvidence {
            incident_id: "incident-2".into(),
            revision: 1,
            active: true,
        },
    };
    assert!(matches!(
        flow.state.prepare(
            "other-task",
            RecoveryCommand::Event(event),
            100_000,
            &flow.knowledge
        ),
        Err(RecoveryError::Busy)
    ));
}

#[test]
fn restored_approval_must_recover_and_explicitly_resume_original_intent() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let op = flow.start();
    let id = flow.attach_and_approve(op.clone());
    flow.state = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    let pending = flow.state.prepare(
        "bypass",
        RecoveryCommand::Event(RecoveryEvent::Resume {
            task_id: flow.id.clone(),
            revision: flow.task().revision,
        }),
        100_000,
        &flow.knowledge,
    );
    assert!(pending.is_err());
    flow.step(RecoveryEvent::Recover);
    assert_eq!(flow.task().stage, RecoveryStage::Paused);
    let revision = flow.task().revision;
    flow.step(RecoveryEvent::Resume {
        task_id: flow.id.clone(),
        revision,
    });
    assert_eq!(flow.task().operation.as_ref(), Some(&op));
    let permit = flow.dispatch(&id);
    assert_eq!(permit.operation(), &op);
}

#[test]
fn replay_rejects_changed_operation_and_bad_sequence() {
    let (flow, _, _permit) = Flow::started();
    let mut entries = flow.state.entries().to_vec();
    entries[1].request.revision += 1;
    assert!(RecoveryState::restore(config(), &entries).is_err());
    let mut entries = flow.state.entries().to_vec();
    if let RecoveryEvent::ExecutionAuthorized { approval, .. } = &mut entries
        .iter_mut()
        .find(|e| matches!(e.event, RecoveryEvent::ExecutionAuthorized { .. }))
        .unwrap()
        .event
    {
        approval.request.operation.target = "other".into();
    }
    assert!(RecoveryState::restore(config(), &entries).is_err());
}

#[test]
fn current_environment_revision_and_owned_permit_are_required() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let op = flow.start();
    let id = flow.attach_and_approve(op);
    let permit = flow.consume(&id);
    let mut command = flow.authorization(permit, &id);
    if let RecoveryCommand::AuthorizeExecution { observation, .. } = &mut command {
        observation
            .facts
            .insert("environment".into(), "changed".into());
    }
    assert!(
        flow.state
            .prepare("bad-environment", command, 100_000, &flow.knowledge)
            .is_err()
    );
    assert_eq!(flow.task().stage, RecoveryStage::AwaitingApproval);
    let event = RecoveryEvent::ExecutionAuthorized {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        approval: flow.approvals.get(&id).unwrap().clone(),
        observation: observation(),
        incident: IncidentEvidence {
            incident_id: "incident-1".into(),
            revision: 1,
            active: true,
        },
        authority: TargetAuthority {
            target_id: "target".into(),
            epoch: "epoch".into(),
        },
    };
    assert!(
        flow.state
            .prepare(
                "forged",
                RecoveryCommand::Event(event),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
}

#[test]
fn independent_result_check_cannot_infer_execution_from_health() {
    let (mut flow, id, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Unknown);
    let operation = flow.task().operation.as_ref().unwrap().clone();
    let event = RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution: ExecutionResultCheck {
            operation_id: operation.operation_id,
            target_id: "target".into(),
            executor_id: "executor".into(),
            outcome: CheckedExecution::Unknown,
            executor_stopped: true,
            evidence_refs: vec!["executor:unknown".into()],
            checked_at_ms: 100_000,
        },
        verification: flow.verification(Some(true)),
        actor: "operator".into(),
        approval: flow.approvals.get(&id).unwrap().clone(),
    };
    flow.step(event);
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    assert!(flow.state.is_quarantined(
        &format!(
            "{}-action",
            flow.task().operation.as_ref().unwrap().operation_id
        ),
        1
    ));
}

#[test]
fn result_check_prepares_evidence_before_approval_finalization_without_losing_isolation() {
    let (mut flow, id, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Unknown);
    let execution = ExecutionResultCheck {
        operation_id: flow.task().operation.as_ref().unwrap().operation_id.clone(),
        target_id: "target".into(),
        executor_id: "executor".into(),
        outcome: CheckedExecution::Executed,
        executor_stopped: true,
        evidence_refs: vec!["executor:confirmed".into()],
        checked_at_ms: 100_000,
    };
    flow.step(RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution: execution.clone(),
        verification: flow.verification(Some(true)),
        actor: "operator".into(),
        approval: flow.approvals.get(&id).unwrap().clone(),
    });
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    assert_eq!(
        flow.task().result_check.as_ref().unwrap().execution.outcome,
        CheckedExecution::Executed
    );
    flow.state = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    flow.step(RecoveryEvent::Recover);
    let pending = flow
        .approvals
        .prepare(
            "reconciled".into(),
            ApprovalEvent::Changed {
                request_id: id.clone(),
                change: ApprovalChange::Reconcile {
                    outcome: ExecutionOutcome::Executed,
                    reason: "independent evidence".into(),
                    actor: "operator".into(),
                },
            },
            Some(&policy()),
            100,
        )
        .unwrap();
    flow.approvals = commit(pending).0;
    flow.step(RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution,
        verification: flow.verification(Some(true)),
        actor: "operator".into(),
        approval: flow.approvals.get(&id).unwrap().clone(),
    });
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
    assert!(flow.state.is_quarantined(
        &format!(
            "{}-action",
            flow.task().operation.as_ref().unwrap().operation_id
        ),
        1
    ));
    let outcomes: Vec<_> = flow
        .state
        .pending_experiences()
        .iter()
        .map(|d| d.outcome)
        .collect();
    assert!(outcomes.contains(&RepairOutcome::Unknown));
    assert!(outcomes.contains(&RepairOutcome::Verified));
}

#[test]
fn unlinked_original_approval_recovers_without_new_operation() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let original = flow.start();
    flow.state = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    flow.step(RecoveryEvent::Recover);
    let effects = flow.step(RecoveryEvent::Resume {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
    });
    assert!(
        matches!(effects.as_slice(),[RecoveryEffect::RequestApproval{operation,..}] if operation==&original)
    );
    assert_eq!(flow.task().operation.as_ref(), Some(&original));
}

#[test]
fn known_execution_with_unknown_business_health_rechecks_verification_only() {
    let (mut flow, id, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Executed);
    flow.verify(None);
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    let execution = ExecutionResultCheck {
        operation_id: flow.task().operation.as_ref().unwrap().operation_id.clone(),
        target_id: "target".into(),
        executor_id: "executor".into(),
        outcome: CheckedExecution::Executed,
        executor_stopped: true,
        evidence_refs: vec!["executor:confirmed".into()],
        checked_at_ms: 100_000,
    };
    let effects = flow.step(RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution,
        verification: flow.verification(Some(true)),
        actor: "operator".into(),
        approval: flow.approvals.get(&id).unwrap().clone(),
    });
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
    assert!(matches!(
        effects.as_slice(),
        [RecoveryEffect::ExperiencePending { .. }]
    ));
    assert!(flow.state.is_quarantined(
        &format!(
            "{}-action",
            flow.task().operation.as_ref().unwrap().operation_id
        ),
        1
    ));
}

#[test]
fn completed_approval_without_task_receipt_can_enter_explicit_reconciliation() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let op = flow.start();
    let id = flow.attach_and_approve(op);
    let permit = flow.consume(&id);
    flow.approvals = commit(
        flow.approvals
            .prepare_complete(
                "external-complete".into(),
                permit,
                ExecutionOutcome::Executed,
                "executor result".into(),
                100,
            )
            .unwrap(),
    )
    .0;
    flow.step(RecoveryEvent::ApprovalResolved {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        record: flow.approvals.get(&id).unwrap().clone(),
    });
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    let execution = ExecutionResultCheck {
        operation_id: flow.task().operation.as_ref().unwrap().operation_id.clone(),
        target_id: "target".into(),
        executor_id: "executor".into(),
        outcome: CheckedExecution::Executed,
        executor_stopped: true,
        evidence_refs: vec!["executor:confirmed".into()],
        checked_at_ms: 100_000,
    };
    flow.step(RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution,
        verification: flow.verification(Some(true)),
        actor: "operator".into(),
        approval: flow.approvals.get(&id).unwrap().clone(),
    });
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
}

#[test]
fn scale_recovery_tasks_retain_incremental_history_and_explicit_recovery() {
    let config = RecoveryConfig {
        max_tasks: 1000,
        ..config()
    };
    let mut state = RecoveryState::new(config.clone()).unwrap();
    let knowledge = KnowledgeState::new(KnowledgeConfig::default()).unwrap();
    let start = std::time::Instant::now();
    let mut largest = 0;
    for i in 0..500 {
        let incident_id = format!("incident-{i}");
        let pending = state
            .prepare(
                format!("register-{i}"),
                RecoveryCommand::Event(RecoveryEvent::Register {
                    problem: problem(&incident_id),
                    incident: IncidentEvidence {
                        incident_id: incident_id.clone(),
                        revision: 1,
                        active: true,
                    },
                }),
                100_000,
                &knowledge,
            )
            .unwrap();
        largest = largest.max(serde_json::to_vec(pending.request()).unwrap().len());
        state = commit(pending).0;
        let task = state
            .tasks()
            .find(|t| t.problem.incident_id == incident_id)
            .unwrap();
        let pending = state
            .prepare(
                format!("cancel-{i}"),
                RecoveryCommand::Event(RecoveryEvent::Cancel {
                    task_id: task.id.clone(),
                    revision: task.revision,
                    approval: None,
                }),
                100_000,
                &knowledge,
            )
            .unwrap();
        largest = largest.max(serde_json::to_vec(pending.request()).unwrap().len());
        assert!(largest < 4000);
        state = commit(pending).0;
    }
    let prepare = start.elapsed();
    let entries = state.entries();
    let bytes = serde_json::to_vec(&entries).unwrap().len();
    let start = std::time::Instant::now();
    let restored = RecoveryState::restore(config.clone(), &entries).unwrap();
    let restore = start.elapsed();
    assert_eq!(restored.tasks().count(), 500);
    assert!(restored.recovery_required());
    let pending = restored
        .prepare(
            "recover",
            RecoveryCommand::Event(RecoveryEvent::Recover),
            100_000,
            &knowledge,
        )
        .unwrap();
    let (_, effects) = commit(pending);
    assert!(effects.is_empty());
    let mut changed = entries;
    changed[1].request.input[3] = serde_json::json!("0".repeat(64));
    assert!(RecoveryState::restore(config, changed).is_err());
    println!(
        "scale recovery n=500 max_request={largest} history_bytes={bytes} prepare={prepare:?} restore={restore:?}"
    );
}

fn unified_started() -> (Flow, String, ExecutionPermit) {
    let mut settings = config();
    settings.approval.allowed_action_kinds = vec!["repair_with_harness".into()];
    let mut flow = Flow::new();
    flow.state = RecoveryState::new(settings).unwrap();
    flow.register("unified-incident");
    let pending = flow
        .state
        .prepare(
            "start-harness",
            RecoveryCommand::StartRepair {
                task_id: flow.id.clone(),
                revision: flow.task().revision,
                observation: observation(),
            },
            100_000,
            &flow.knowledge,
        )
        .unwrap();
    assert_eq!(flow.task().stage, RecoveryStage::Queued);
    let (state, effects) = commit(pending);
    flow.state = state;
    assert!(matches!(
        effects.as_slice(),
        [RecoveryEffect::RequestApproval { .. }]
    ));
    let op = flow.task().operation.clone().unwrap();
    let request: HarnessRepairRequest =
        serde_json::from_value(op.action["request"].clone()).unwrap();
    assert!(request.experiences.is_empty());
    assert!(request.summarize_experience && request.assess_scriptability);
    let id = flow.attach_and_approve(op);
    let permit = flow.dispatch(&id);
    (flow, id, permit)
}
fn report_without_script() -> ExperienceReport {
    ExperienceReport {
        summary: "repair findings".into(),
        lessons: "An interactive diagnosis was required".into(),
        related_experience_ids: vec![],
        scriptability: Scriptability::NotSuitable {
            reason: "Requires contextual judgment".into(),
        },
    }
}
fn summarize(flow: &mut Flow) -> ExperienceJob {
    let job = flow.state.pending_experiences()[0].clone();
    let effects = flow.step(RecoveryEvent::BeginExperience {
        job_id: job.id.clone(),
    });
    let RecoveryEffect::SummarizeExperience { call_id, .. } = &effects[0] else {
        panic!("summary effect");
    };
    flow.step(RecoveryEvent::ExperienceSummarized {
        job_id: job.id.clone(),
        call_id: call_id.clone(),
        report: report_without_script(),
    });
    flow.state.pending_experiences()[0].clone()
}
#[test]
fn unified_repair_supports_script_free_experience_and_matches_it_next_time() {
    let (mut flow, _, permit) = unified_started();
    flow.record(permit, ExecutionOutcome::Executed);
    flow.verify(Some(true));
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
    let job = summarize(&mut flow);
    let experience = job.record().unwrap();
    let pending = flow
        .knowledge
        .propose(
            "experience",
            KnowledgeCommand::RecordExperience(
                TrustedRepairExperience::attest(experience.clone()).unwrap(),
            ),
        )
        .unwrap();
    assert!(flow.knowledge.snapshot().experiences.is_empty());
    flow.knowledge = commit(pending).0;
    flow.step(RecoveryEvent::ExperienceDelivered { job_id: job.id });
    flow.register("another-incident");
    flow.state = commit(
        flow.state
            .prepare(
                "known-harness",
                RecoveryCommand::StartRepair {
                    task_id: flow.id.clone(),
                    revision: flow.task().revision,
                    observation: observation(),
                },
                100_000,
                &flow.knowledge,
            )
            .unwrap(),
    )
    .0;
    let request: HarnessRepairRequest =
        serde_json::from_value(flow.task().operation.as_ref().unwrap().action["request"].clone())
            .unwrap();
    assert_eq!(request.experiences, vec![experience]);
}
#[test]
fn summary_failure_and_restart_do_not_change_result_or_reissue_execution() {
    let (mut flow, _, permit) = unified_started();
    flow.record(permit, ExecutionOutcome::Executed);
    flow.verify(Some(true));
    let job = flow.state.pending_experiences()[0].clone();
    let effects = flow.step(RecoveryEvent::BeginExperience {
        job_id: job.id.clone(),
    });
    let RecoveryEffect::SummarizeExperience { call_id, .. } = &effects[0] else {
        panic!()
    };
    flow.state = RecoveryState::restore(flow.state.config().clone(), flow.state.entries()).unwrap();
    let effects = flow.step(RecoveryEvent::Recover);
    assert!(effects.is_empty());
    assert!(
        flow.state
            .prepare(
                "late-summary",
                RecoveryCommand::Event(RecoveryEvent::ExperienceSummarized {
                    job_id: job.id.clone(),
                    call_id: call_id.clone(),
                    report: report_without_script(),
                }),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
    let retried = summarize(&mut flow);
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.attempt, 2);
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
}
#[test]
fn harness_repair_requires_explicit_policy_and_cannot_be_forged_as_event() {
    let mut settings = config();
    settings.approval.allowed_action_kinds = vec!["other_action".into()];
    assert!(RecoveryState::new(settings).is_err());
    let (flow, _, _) = unified_started();
    let request: HarnessRepairRequest =
        serde_json::from_value(flow.task().operation.as_ref().unwrap().action["request"].clone())
            .unwrap();
    assert!(
        flow.state
            .prepare(
                "forged",
                RecoveryCommand::Event(RecoveryEvent::RepairRequested {
                    task_id: flow.id.clone(),
                    revision: flow.task().revision,
                    request,
                }),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
}
#[test]
fn interrupted_harness_action_retains_exact_action_and_quarantine() {
    let (mut flow, _, _) = unified_started();
    let mut script = artifact(1);
    script.id = format!(
        "{}-action",
        flow.task().operation.as_ref().unwrap().operation_id
    );
    script.generated_in_session = flow.task().operation.as_ref().unwrap().operation_id.clone();
    flow.step(RecoveryEvent::RepairActionPrepared {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        action: script.clone(),
    });
    assert!(
        flow.state
            .prepare(
                "second-action",
                RecoveryCommand::Event(RecoveryEvent::RepairActionPrepared {
                    task_id: flow.id.clone(),
                    revision: flow.task().revision,
                    action: script.clone(),
                }),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
    flow.state = RecoveryState::restore(flow.state.config().clone(), flow.state.entries()).unwrap();
    assert_eq!(
        flow.state
            .repair_action(&flow.task().operation.as_ref().unwrap().operation_id),
        Some(&script)
    );
    let effects = flow.step(RecoveryEvent::Recover);
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, RecoveryEffect::Execute { .. }))
    );
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    assert_eq!(
        flow.task().receipt.as_ref().unwrap().execution_trace,
        vec![script.clone()]
    );
    assert!(flow.state.is_quarantined(&script.id, script.version));
    assert!(
        flow.state
            .prepare(
                "new-incident",
                RecoveryCommand::Event(RecoveryEvent::Register {
                    problem: problem("next"),
                    incident: IncidentEvidence {
                        incident_id: "next".into(),
                        revision: 1,
                        active: true
                    },
                }),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
}
#[test]
fn generated_candidate_is_not_verified_by_the_successful_harness_repair() {
    let (mut flow, _, permit) = unified_started();
    flow.record(permit, ExecutionOutcome::Executed);
    flow.verify(Some(true));
    let job = flow.state.pending_experiences()[0].clone();
    let effects = flow.step(RecoveryEvent::BeginExperience {
        job_id: job.id.clone(),
    });
    let RecoveryEffect::SummarizeExperience { call_id, .. } = &effects[0] else {
        panic!()
    };
    let mut report = report_without_script();
    let mut candidate = artifact(8);
    candidate.generated_in_session = call_id.clone();
    report.scriptability = Scriptability::Possible {
        reason: "Can be automated but not tested".into(),
        candidate: Some(candidate),
    };
    flow.step(RecoveryEvent::ExperienceSummarized {
        job_id: job.id,
        call_id: call_id.clone(),
        report,
    });
    let job = flow.state.pending_experiences()[0].clone();
    let item = job.record().unwrap();
    flow.knowledge = commit(
        flow.knowledge
            .propose(
                "candidate-experience",
                KnowledgeCommand::RecordExperience(
                    TrustedRepairExperience::attest(item.clone()).unwrap(),
                ),
            )
            .unwrap(),
    )
    .0;
    let query = KnowledgeQuery {
        conditions: item.conditions,
        keywords: item.keywords,
        limit: 4,
    };
    assert_eq!(flow.knowledge.search_experiences(&query).unwrap().len(), 1);
    assert!(flow.knowledge.get(&item.id).unwrap().actions.is_empty());
}

#[test]
fn repair_preparation_and_wrong_commit_receipts_never_change_original_state() {
    let mut flow = Flow::new();
    flow.register("incident");
    let command = || RecoveryCommand::StartRepair {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
    };
    let pending = flow
        .state
        .prepare("start", command(), 100_000, &flow.knowledge)
        .unwrap();
    let other = flow
        .state
        .prepare("other", command(), 100_000, &flow.knowledge)
        .unwrap();
    assert_eq!(
        pending.state().task(&flow.id).unwrap().stage,
        RecoveryStage::AwaitingApproval
    );
    assert!(
        pending
            .confirm(CommitReceipt::confirmed(other.request()))
            .is_err()
    );
    assert_eq!(flow.task().stage, RecoveryStage::Queued);
    assert!(flow.task().operation.is_none());
}

#[test]
fn action_scope_origin_and_preconditions_are_checked_before_budget_is_consumed() {
    let (mut flow, _, _) = unified_started();
    let operation_id = flow.task().operation.as_ref().unwrap().operation_id.clone();
    let mut valid = artifact(1);
    valid.id = format!("{operation_id}-action");
    valid.generated_in_session = operation_id.clone();
    let mut bad_kind = valid.clone();
    bad_kind.kind = "delete_workload".into();
    let mut bad_origin = valid.clone();
    bad_origin.generated_in_session = "another-operation".into();
    let mut bad_harness = valid.clone();
    bad_harness.generated_by_harness = "other-harness".into();
    let mut bad_facts = valid.clone();
    bad_facts
        .preconditions
        .insert("environment".into(), "other".into());
    for (i, action) in [bad_kind, bad_origin, bad_harness, bad_facts]
        .into_iter()
        .enumerate()
    {
        assert!(
            flow.state
                .prepare(
                    format!("invalid-{i}"),
                    RecoveryCommand::Event(RecoveryEvent::RepairActionPrepared {
                        task_id: flow.id.clone(),
                        revision: flow.task().revision,
                        action
                    }),
                    100_000,
                    &flow.knowledge
                )
                .is_err()
        );
        assert!(flow.state.repair_action(&operation_id).is_none());
    }
    let action = flow.prepare_action();
    assert_eq!(action, valid);
}

#[test]
fn receipts_bind_the_exact_committed_action_payload() {
    let (mut flow, id, permit) = Flow::started();
    flow.approvals = commit(
        flow.approvals
            .prepare_complete(
                "finished".into(),
                permit,
                ExecutionOutcome::Executed,
                "evidence".into(),
                100,
            )
            .unwrap(),
    )
    .0;
    let op = flow.task().operation.as_ref().unwrap();
    let mut altered = flow.state.repair_action(&op.operation_id).unwrap().clone();
    altered.payload = serde_json::json!({"workload":"other"});
    let event = RecoveryEvent::ExecutionRecorded {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        receipt: RepairReceipt {
            execution_trace: vec![altered],
            operation_id: op.operation_id.clone(),
            target_id: op.target.clone(),
            outcome: RepairExecutionOutcome::Executed,
            executor_stopped: true,
            evidence_refs: vec!["executor:evidence".into()],
            summary: "finished".into(),
        },
        approval: flow.approvals.get(&id).unwrap().clone(),
    };
    assert!(
        flow.state
            .prepare(
                "altered",
                RecoveryCommand::Event(event),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
    assert_eq!(flow.task().stage, RecoveryStage::Executing);
}

#[test]
fn failed_repair_is_terminal_and_summary_failure_preserves_local_isolation() {
    let (mut flow, _, permit) = Flow::started();
    let action = flow
        .state
        .repair_action(&flow.task().operation.as_ref().unwrap().operation_id)
        .unwrap()
        .clone();
    flow.record(permit, ExecutionOutcome::Failed);
    assert_eq!(flow.task().stage, RecoveryStage::Failed);
    assert!(flow.state.is_quarantined(&action.id, action.version));
    let job = flow.state.pending_experiences()[0].clone();
    let effects = flow.step(RecoveryEvent::BeginExperience {
        job_id: job.id.clone(),
    });
    let RecoveryEffect::SummarizeExperience { call_id, .. } = &effects[0] else {
        panic!()
    };
    flow.step(RecoveryEvent::ExperienceFailed {
        job_id: job.id.clone(),
        call_id: call_id.clone(),
        reason: "summary unavailable".into(),
    });
    let completed = summarize(&mut flow);
    assert_eq!(completed.id, job.id);
    assert_eq!(completed.attempt, 2);
    assert_eq!(completed.record().unwrap().actions, vec![action.clone()]);
    assert_eq!(flow.task().stage, RecoveryStage::Failed);
    assert!(flow.state.is_quarantined(&action.id, action.version));
}

#[test]
fn restoring_approval_does_not_extend_original_expiration() {
    let mut flow = Flow::new();
    flow.register("incident");
    let op = flow.start();
    let id = flow.attach_and_approve(op.clone());
    let expires = flow.approvals.get(&id).unwrap().request.expires_at;
    let permit = flow.consume(&id);
    flow.state = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    flow.step(RecoveryEvent::Recover);
    flow.step(RecoveryEvent::Resume {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
    });
    assert_eq!(flow.task().operation.as_ref(), Some(&op));
    assert_eq!(flow.approvals.get(&id).unwrap().request.expires_at, expires);
    let mut command = flow.authorization(permit, &id);
    if let RecoveryCommand::AuthorizeExecution { observation, .. } = &mut command {
        observation.observed_at_ms = expires * 1000;
    }
    assert!(
        flow.state
            .prepare("expired", command, expires * 1000, &flow.knowledge)
            .is_err()
    );
    assert_eq!(flow.task().stage, RecoveryStage::AwaitingApproval);
}

#[test]
fn large_matching_experiences_are_selected_whole_within_request_budget() {
    for sizes in [
        [MAX_ARTIFACT_BYTES - 2; 4],
        [
            MAX_ARTIFACT_BYTES - 2,
            MAX_ARTIFACT_BYTES - 2,
            MAX_ARTIFACT_BYTES - 2,
            64,
        ],
        [12 * 1024; 4],
    ] {
        let mut flow = Flow::new();
        let mut all = Vec::new();
        for (index, size) in sizes.into_iter().enumerate() {
            let mut action = artifact(1);
            action.id = format!("reference-action-{index}");
            action.payload = serde_json::Value::String("x".repeat(size));
            let mut conditions = facts();
            conditions.insert("fault_fingerprint".into(), "fault".into());
            let experience = RepairExperience {
                id: format!("reference-{index}"),
                operation_id: format!("prior-operation-{index}"),
                target_id: "target".into(),
                conditions,
                keywords: vec!["workload".into()],
                outcome: RepairOutcome::Verified,
                evidence_refs: vec!["business:prior".into()],
                recorded_at_ms: 4 - index as u64,
                actions: vec![action],
                report: report_without_script(),
            };
            flow.knowledge = commit(
                flow.knowledge
                    .propose(
                        format!("reference-{index}"),
                        KnowledgeCommand::RecordExperience(
                            TrustedRepairExperience::attest(experience.clone()).unwrap(),
                        ),
                    )
                    .unwrap(),
            )
            .0;
            all.push(experience);
        }
        flow.register("large-references");
        let operation = flow.start();
        operation.validate().unwrap();
        let request: HarnessRepairRequest =
            serde_json::from_value(operation.action["request"].clone()).unwrap();
        assert!(serde_json::to_vec(&request).unwrap().len() <= MAX_REPAIR_REQUEST_BYTES);
        let expected = if sizes[0] == 12 * 1024 {
            vec![all[0].clone(), all[1].clone()]
        } else if sizes[3] == 64 {
            vec![all[3].clone()]
        } else {
            Vec::new()
        };
        assert_eq!(request.experiences, expected);
        assert_eq!(flow.knowledge.snapshot().experiences.len(), 4);
        let restored = RecoveryState::restore(config(), flow.state.entries()).unwrap();
        assert_eq!(
            restored.task(&flow.id).unwrap().operation.as_ref(),
            Some(&operation)
        );
    }
}

#[test]
fn oversized_required_repair_context_is_rejected_without_mutation() {
    let mut flow = Flow::new();
    let mut context = problem("large-fault");
    context.evidence_refs = (0..32)
        .map(|index| format!("{index:02}{}", "x".repeat(1022)))
        .collect();
    flow.step(RecoveryEvent::Register {
        problem: context,
        incident: IncidentEvidence {
            incident_id: "large-fault".into(),
            revision: 1,
            active: true,
        },
    });
    flow.id = flow.state.tasks().next().unwrap().id.clone();
    let result = flow.state.prepare(
        "oversized",
        RecoveryCommand::StartRepair {
            task_id: flow.id.clone(),
            revision: flow.task().revision,
            observation: observation(),
        },
        100_000,
        &flow.knowledge,
    );
    assert!(
        matches!(result,Err(RecoveryError::Invalid(reason)) if reason=="repair request exceeds context budget")
    );
    assert_eq!(flow.task().stage, RecoveryStage::Queued);
    assert!(flow.task().operation.is_none());
}
