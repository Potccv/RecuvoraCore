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
        allowed_action_kinds: vec!["execute_script".into()],
        ttl_secs: 600,
    }
}
fn config() -> RecoveryConfig {
    RecoveryConfig {
        schema_version: 1,
        execution_harness: "diagnoser".into(),
        target: TargetBinding {
            target_id: "target".into(),
            executor_id: "executor".into(),
            platform: "linux".into(),
            allowed_languages: vec!["sh".into()],
            diagnostic_queries: vec!["status".into()],
            verification_profile: "business".into(),
            required_facts: facts(),
            action_timeout_secs: 30,
        },
        approval: policy(),
        script_approval: policy(),
        diagnosis_timeout_secs: 30,
        review_timeout_secs: 20,
        max_tool_calls: 4,
        max_diagnoses: 2,
        minimum_script_occurrences: 1,
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
fn plan(version: u64) -> RepairPlan {
    RepairPlan {
        summary: "repair workload".into(),
        reusable: true,
        script: ScriptArtifact {
            id: "script".into(),
            version,
            language: "sh".into(),
            platform: "linux".into(),
            source: "external action".into(),
            preconditions: facts(),
            generated_by_harness: "diagnoser".into(),
            generated_in_session: "diagnosis-session".into(),
        },
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
    fn diagnose(&mut self) -> ProposedOperation {
        let effects = self.step(RecoveryEvent::SelectPlan {
            task_id: self.id.clone(),
            revision: self.task().revision,
            observation: observation(),
            candidate: None,
        });
        assert!(matches!(
            effects.as_slice(),
            [RecoveryEffect::Diagnose { .. }]
        ));
        let call_id = self.task().diagnosis_call.clone().unwrap();
        let effects = self.step(RecoveryEvent::DiagnosisCompleted {
            task_id: self.id.clone(),
            revision: self.task().revision,
            call_id,
            plan: plan(1),
        });
        assert!(matches!(
            effects.as_slice(),
            [RecoveryEffect::RequestApproval { .. }]
        ));
        self.task().operation.clone().unwrap()
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
            ExecutionOutcome::Executed => ScriptOutcome::Executed,
            ExecutionOutcome::Failed => ScriptOutcome::Failed,
            ExecutionOutcome::Unknown => ScriptOutcome::Unknown,
        };
        let op = self.task().operation.as_ref().unwrap();
        self.step(RecoveryEvent::ExecutionRecorded {
            task_id: self.id.clone(),
            revision: self.task().revision,
            receipt: ScriptReceipt {
                execution_trace: Vec::new(),
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
        let op = flow.diagnose();
        let id = flow.attach_and_approve(op);
        let permit = flow.dispatch(&id);
        (flow, id, permit)
    }
}

#[test]
fn complete_repair_does_not_wait_for_knowledge_delivery_and_preserves_outbox() {
    let (mut flow, _, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Executed);
    flow.verify(Some(true));
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
    let delivery = flow.state.pending_deliveries()[0].clone();
    assert_eq!(delivery.case.outcome, RepairOutcome::Verified);
    assert!(delivery.verification.is_some());
    flow.register("incident-2"); // Host's knowledge module may still be unavailable.
    assert_eq!(flow.task().episode_count, 2);
    assert_eq!(flow.state.pending_deliveries()[0].id, delivery.id);
    flow.step(RecoveryEvent::DeliveryConfirmed {
        delivery_id: delivery.id.clone(),
    });
    flow.step(RecoveryEvent::DeliveryConfirmed {
        delivery_id: delivery.id,
    });
    assert!(flow.state.pending_deliveries().is_empty());
}

#[test]
fn failed_new_plan_is_terminal_and_quarantine_precedes_delivery() {
    let (mut flow, _, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Failed);
    assert_eq!(flow.task().stage, RecoveryStage::Failed);
    assert!(flow.state.is_quarantined("script", 1));
    assert_eq!(flow.task().diagnosis_attempts, 1);
    flow.register("incident-2");
    flow.step(RecoveryEvent::SelectPlan {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
        candidate: None,
    });
    let event = RecoveryEvent::DiagnosisCompleted {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        call_id: flow.task().diagnosis_call.clone().unwrap(),
        plan: plan(1),
    };
    assert!(
        flow.state
            .prepare(
                "repeat-script",
                RecoveryCommand::Event(event),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
}

#[test]
fn unknown_execution_blocks_new_tasks_and_never_dispatches_on_restore() {
    let (mut flow, _, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Unknown);
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    assert!(flow.state.is_quarantined("script", 1));
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
fn interrupted_dispatch_is_quarantined_before_reconciliation() {
    let (mut flow, _, _permit) = Flow::started();
    flow.state = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    let effects = flow.step(RecoveryEvent::Recover);
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    assert!(flow.state.is_quarantined("script", 1));
    assert!(matches!(
        effects.as_slice(),
        [RecoveryEffect::DeliverKnowledge(_)]
    ));
    assert_eq!(
        flow.state.pending_deliveries()[0].case.outcome,
        RepairOutcome::Unknown
    );
}

#[test]
fn restored_approval_must_recover_and_explicitly_resume_original_intent() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let op = flow.diagnose();
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
fn interrupted_diagnosis_consumes_budget_and_rejects_late_callback() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    flow.step(RecoveryEvent::SelectPlan {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
        candidate: None,
    });
    let old = flow.task().clone();
    flow.state = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    flow.step(RecoveryEvent::Recover);
    assert_eq!(flow.task().diagnosis_attempts, 1);
    let event = RecoveryEvent::DiagnosisCompleted {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        call_id: old.diagnosis_call.unwrap(),
        plan: plan(1),
    };
    assert!(
        flow.state
            .prepare(
                "late",
                RecoveryCommand::Event(event),
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
    flow.step(RecoveryEvent::RetryDiagnosis {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
    });
    assert_eq!(flow.task().diagnosis_attempts, 2);
}

#[test]
fn replay_rejects_changed_operation_and_bad_sequence() {
    let (flow, _, _permit) = Flow::started();
    let mut entries = flow.state.entries().to_vec();
    entries[1].request.revision += 1;
    assert!(RecoveryState::restore(config(), &entries).is_err());
    let mut entries = flow.state.entries().to_vec();
    if let RecoveryEvent::ExecutionAuthorized { approval, .. } =
        &mut entries.last_mut().unwrap().event
    {
        approval.request.operation.target = "other".into();
    }
    assert!(RecoveryState::restore(config(), &entries).is_err());
}

#[test]
fn current_environment_revision_and_owned_permit_are_required() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let op = flow.diagnose();
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
    assert!(flow.state.is_quarantined("script", 1));
}

#[test]
fn proposal_rejection_does_not_mutate_workflow_or_charge_diagnosis() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let event = RecoveryEvent::SelectPlan {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
        candidate: None,
    };
    let pending = flow
        .state
        .prepare(
            "uncommitted",
            RecoveryCommand::Event(event),
            100_000,
            &flow.knowledge,
        )
        .unwrap();
    assert_eq!(
        pending.state().task(&flow.id).unwrap().diagnosis_attempts,
        1
    );
    drop(pending);
    assert_eq!(flow.task().diagnosis_attempts, 0);
    assert_eq!(flow.task().stage, RecoveryStage::Queued);
}

#[test]
fn diagnosis_callback_uses_revision_from_committed_effect() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let mut effects = flow.step(RecoveryEvent::SelectPlan {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
        candidate: None,
    });
    let RecoveryEffect::Diagnose { task, call_id, .. } = effects.remove(0) else {
        panic!("diagnosis effect")
    };
    assert_eq!(task.revision, flow.task().revision);
    flow.step(RecoveryEvent::DiagnosisCompleted {
        task_id: task.id,
        revision: task.revision,
        call_id,
        plan: plan(1),
    });
    assert_eq!(flow.task().stage, RecoveryStage::AwaitingApproval);
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
    assert!(flow.state.is_quarantined("script", 1));
    let outcomes: Vec<_> = flow
        .state
        .pending_deliveries()
        .iter()
        .map(|d| d.case.outcome)
        .collect();
    assert!(outcomes.contains(&RepairOutcome::Unknown));
    assert!(outcomes.contains(&RepairOutcome::Verified));
}

#[test]
fn unlinked_original_approval_recovers_without_new_diagnosis_or_operation() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let original = flow.diagnose();
    let attempts = flow.task().diagnosis_attempts;
    flow.state = RecoveryState::restore(config(), flow.state.entries()).unwrap();
    flow.step(RecoveryEvent::Recover);
    let effects = flow.step(RecoveryEvent::Resume {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
    });
    assert!(
        matches!(effects.as_slice(),[RecoveryEffect::RequestApproval{operation,..}] if operation==&original)
    );
    assert_eq!(flow.task().diagnosis_attempts, attempts);
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
        [RecoveryEffect::DeliverKnowledge(_)]
    ));
    assert!(flow.state.is_quarantined("script", 1));
}

fn deliver(flow: &mut Flow, delivery: KnowledgeDelivery) {
    let pending = flow
        .knowledge
        .propose(
            format!("candidate-{}", flow.knowledge.revision()),
            KnowledgeCommand::UpsertCandidate(delivery.candidate),
        )
        .unwrap();
    flow.knowledge = commit(pending).0;
    let proof = delivery.verification.map(|v| {
        TrustedBusinessVerification::attest(
            v.operation_id,
            v.target_id,
            v.script_id,
            v.script_version,
            v.verifier_id,
            v.evidence_refs,
            v.verified_at_ms,
        )
        .unwrap()
    });
    let candidate_id = flow
        .knowledge
        .snapshot()
        .records
        .iter()
        .find(|r| r.candidate.script.id == delivery.case.script_id)
        .unwrap()
        .id
        .clone();
    flow.knowledge = commit(
        flow.knowledge
            .propose(
                format!("case-{}", flow.knowledge.revision()),
                KnowledgeCommand::RecordOutcome {
                    record_id: candidate_id,
                    case: delivery.case,
                    verification: proof,
                },
            )
            .unwrap(),
    )
    .0;
}
fn reusable_flow() -> Flow {
    let (mut flow, _, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Executed);
    flow.verify(Some(true));
    let delivery = flow.state.pending_deliveries()[0].clone();
    deliver(&mut flow, delivery);
    flow.register("incident-2");
    let candidate = flow.knowledge.snapshot().records[0].clone();
    flow.step(RecoveryEvent::SelectPlan {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
        candidate: Some(candidate),
    });
    assert!(flow.task().reused_script);
    assert_eq!(flow.task().diagnosis_attempts, 0);
    flow
}

#[test]
fn reused_failure_requires_new_version_diagnosis_and_fresh_approval() {
    let mut flow = reusable_flow();
    let op = flow.task().operation.clone().unwrap();
    let old = op.operation_id.clone();
    let id = flow.attach_and_approve(op);
    let permit = flow.dispatch(&id);
    flow.record(permit, ExecutionOutcome::Failed);
    assert_eq!(flow.task().stage, RecoveryStage::Diagnosing);
    assert!(flow.state.is_quarantined("script", 1));
    let mut effects = flow.step(RecoveryEvent::RetryDiagnosis {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
    });
    let RecoveryEffect::Diagnose { task, call_id, .. } = effects.remove(0) else {
        panic!("diagnose")
    };
    let effects = flow.step(RecoveryEvent::DiagnosisCompleted {
        task_id: task.id,
        revision: task.revision,
        call_id,
        plan: plan(2),
    });
    assert!(
        matches!(effects.as_slice(),[RecoveryEffect::RequestApproval{operation,..}] if operation.operation_id != old)
    );
    assert!(flow.task().approval_id.is_none());
}

#[test]
fn human_denial_of_reuse_is_terminal_but_harness_denial_allows_alternative() {
    for human in [true, false] {
        let mut flow = reusable_flow();
        let op = flow.task().operation.clone().unwrap();
        flow.approvals = commit(
            flow.approvals
                .prepare_request(
                    format!("reuse-request-{}", flow.approvals.revision()),
                    op,
                    policy(),
                    100,
                )
                .unwrap(),
        )
        .0;
        let mut record = flow.approvals.list().last().unwrap().clone();
        let id = record.request.request_id.clone();
        flow.step(RecoveryEvent::ApprovalAttached {
            task_id: flow.id.clone(),
            revision: flow.task().revision,
            record: record.clone(),
        });
        let assessment = ApprovalAssessment {
            decision: ApprovalDecision::Deny,
            reason: "not appropriate".into(),
            reviewer: if human {
                AssessmentSource::Human {
                    actor: "operator".into(),
                }
            } else {
                AssessmentSource::Harness {
                    harness_id: "reviewer".into(),
                    session_id: "review-session".into(),
                }
            },
        };
        let change = if human {
            ApprovalChange::HumanDecision {
                expected_revision: record.revision,
                assessment,
            }
        } else {
            let pending = flow
                .approvals
                .prepare(
                    "review-reuse".into(),
                    ApprovalEvent::Changed {
                        request_id: id.clone(),
                        change: ApprovalChange::BeginReview {
                            expected_revision: record.revision,
                            timeout_secs: 10,
                        },
                    },
                    Some(&policy()),
                    100,
                )
                .unwrap();
            let (approvals, mut effects) = commit(pending);
            flow.approvals = approvals;
            let ApprovalEffect::Review(attempt) = effects.remove(0) else {
                panic!("review effect")
            };
            ApprovalChange::AssessAttempt {
                attempt,
                assessment,
            }
        };
        flow.approvals = commit(
            flow.approvals
                .prepare(
                    "deny-reuse".into(),
                    ApprovalEvent::Changed {
                        request_id: id.clone(),
                        change,
                    },
                    Some(&policy()),
                    100,
                )
                .unwrap(),
        )
        .0;
        record = flow.approvals.get(&id).unwrap().clone();
        flow.step(RecoveryEvent::ApprovalResolved {
            task_id: flow.id.clone(),
            revision: flow.task().revision,
            record,
        });
        assert_eq!(
            flow.task().stage,
            if human {
                RecoveryStage::Denied
            } else {
                RecoveryStage::Diagnosing
            }
        );
    }
}

#[test]
fn reusable_candidate_accepts_case_count_above_default_budget() {
    let (mut flow, _, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Executed);
    flow.verify(Some(true));
    flow.knowledge = KnowledgeState::new(KnowledgeConfig {
        max_records: 10,
        max_cases_per_record: 256,
    })
    .unwrap();
    let delivery = flow.state.pending_deliveries()[0].clone();
    deliver(&mut flow, delivery.clone());
    for index in 0..128 {
        let mut case = delivery.case.clone();
        case.id = format!("verified-{index}");
        case.operation_id = format!("operation-{index}");
        let v = delivery.verification.as_ref().unwrap();
        let proof = TrustedBusinessVerification::attest(
            &case.operation_id,
            &case.target_id,
            &case.script_id,
            case.script_version,
            &v.verifier_id,
            v.evidence_refs.clone(),
            v.verified_at_ms,
        )
        .unwrap();
        flow.knowledge = commit(
            flow.knowledge
                .propose(
                    format!("extra-{index}"),
                    KnowledgeCommand::RecordOutcome {
                        record_id: delivery.candidate.id.clone(),
                        case,
                        verification: Some(proof),
                    },
                )
                .unwrap(),
        )
        .0;
    }
    flow.register("incident-2");
    let candidate = flow.knowledge.get(&delivery.candidate.id).unwrap();
    assert_eq!(candidate.cases.len(), 129);
    flow.step(RecoveryEvent::SelectPlan {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        observation: observation(),
        candidate: Some(candidate),
    });
    assert!(flow.task().reused_script);
}

#[test]
fn still_fresh_prepared_evidence_survives_time_advancing_between_commits() {
    let (mut flow, id, permit) = Flow::started();
    flow.record(permit, ExecutionOutcome::Unknown);
    let execution = ExecutionResultCheck {
        operation_id: flow.task().operation.as_ref().unwrap().operation_id.clone(),
        target_id: "target".into(),
        executor_id: "executor".into(),
        outcome: CheckedExecution::Failed,
        executor_stopped: true,
        evidence_refs: vec!["executor:failed".into()],
        checked_at_ms: 100_100,
    };
    let mut verification = flow.verification(Some(false));
    verification.verified_at_ms = 100_100;
    let event = RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution: execution.clone(),
        verification: verification.clone(),
        actor: "operator".into(),
        approval: flow.approvals.get(&id).unwrap().clone(),
    };
    flow.state = commit(
        flow.state
            .prepare(
                "prepared-result",
                RecoveryCommand::Event(event),
                100_200,
                &flow.knowledge,
            )
            .unwrap(),
    )
    .0;
    flow.approvals = commit(
        flow.approvals
            .prepare(
                "result-reconciled".into(),
                ApprovalEvent::Changed {
                    request_id: id.clone(),
                    change: ApprovalChange::Reconcile {
                        outcome: ExecutionOutcome::Failed,
                        reason: "executor stopped".into(),
                        actor: "operator".into(),
                    },
                },
                Some(&policy()),
                101,
            )
            .unwrap(),
    )
    .0;
    let event = RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution,
        verification,
        actor: "operator".into(),
        approval: flow.approvals.get(&id).unwrap().clone(),
    };
    let (state, effects) = commit(
        flow.state
            .prepare(
                "final-result",
                RecoveryCommand::Event(event),
                101_200,
                &flow.knowledge,
            )
            .unwrap(),
    );
    flow.state = state;
    assert_eq!(flow.task().stage, RecoveryStage::Failed);
    let pending = flow.state.pending_deliveries();
    assert_eq!(pending[0].case.outcome, RepairOutcome::Unknown);
    assert_eq!(pending[1].case.outcome, RepairOutcome::Failed);
    assert!(
        matches!(effects.as_slice(),[RecoveryEffect::DeliverKnowledge(delivery)] if delivery.id==pending[0].id)
    );
    let event = RecoveryEvent::DeliveryConfirmed {
        delivery_id: pending[1].id.clone(),
    };
    assert!(
        flow.state
            .prepare(
                "out-of-order",
                RecoveryCommand::Event(event),
                101_200,
                &flow.knowledge
            )
            .is_err()
    );
}

#[test]
fn completed_approval_without_task_receipt_can_enter_explicit_reconciliation() {
    let mut flow = Flow::new();
    flow.register("incident-1");
    let op = flow.diagnose();
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
    assert!(flow.state.is_quarantined("script", 1));
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
        assert_eq!(task.episode_count, i + 1);
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

fn legacy_revisions() -> (Vec<LegacyRecoveryRevision>, ApprovalLedger, KnowledgeState) {
    let mut task = LegacyRecoveryTask {
        id: "legacy-random-task-4ad1".into(),
        revision: 1,
        problem: problem("legacy-incident"),
        episode_count: 0,
        stage: LegacyRecoveryStage::Queued,
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
        created_at_ms: 100_000,
        updated_at_ms: 100_000,
    };
    let mut history = vec![LegacyRecoveryRevision {
        format: 1,
        sequence: 1,
        task: task.clone(),
    }];
    task.revision = 2;
    task.stage = LegacyRecoveryStage::Diagnosing;
    history.push(LegacyRecoveryRevision {
        format: 1,
        sequence: 2,
        task: task.clone(),
    });
    task.revision = 3;
    task.diagnosis_attempts = 1;
    task.observation = Some(observation());
    history.push(LegacyRecoveryRevision {
        format: 1,
        sequence: 3,
        task: task.clone(),
    });
    task.revision = 4;
    task.stage = LegacyRecoveryStage::AwaitingApproval;
    task.plan = Some(plan(1));
    task.operation = Some(ProposedOperation {
        task_id: task.id.clone(),
        task_revision: 3,
        operation_id: "legacy-original-operation".into(),
        target: "target".into(),
        action: serde_json::json!({"kind":"execute_script","executor_id":"executor","script":plan(1).script,"verification_profile":"business","required_facts":facts(),"timeout_secs":30,"incident_id":"legacy-incident","incident_revision":1}),
    });
    history.push(LegacyRecoveryRevision {
        format: 1,
        sequence: 4,
        task: task.clone(),
    });
    let entries = vec![
        LegacyApprovalEntry {
            sequence: 1,
            now: 100,
            event: serde_json::json!({"event":"requested","request":{"request_id":"approval-0000000000000001","operation":task.operation,"policy":policy(),"created_at":100,"expires_at":700}}),
        },
        LegacyApprovalEntry {
            sequence: 2,
            now: 100,
            event: serde_json::json!({"event":"changed","request_id":"approval-0000000000000001","change":{"change":"assess","assessment":{"decision":"approve","reason":"original review","reviewer":{"source":"harness","harness_id":"reviewer","session_id":"legacy-session"}}}}),
        },
    ];
    let import = ApprovalImport::validate(ApprovalLimits::default(), entries).unwrap();
    let approvals = commit(
        ApprovalLedger::new(ApprovalLimits::default())
            .unwrap()
            .prepare_import("approval-0000000000000001s".into(), import, 100)
            .unwrap(),
    )
    .0;
    task.revision = 5;
    task.approval_id = Some("approval-0000000000000001".into());
    history.push(LegacyRecoveryRevision {
        format: 1,
        sequence: 5,
        task,
    });
    (
        history,
        approvals,
        KnowledgeState::new(KnowledgeConfig::default()).unwrap(),
    )
}
fn push_legacy(history: &mut Vec<LegacyRecoveryRevision>, stage: LegacyRecoveryStage) {
    let mut task = history.last().unwrap().task.clone();
    task.revision += 1;
    task.stage = stage;
    history.push(LegacyRecoveryRevision {
        format: 1,
        sequence: history.len() as u64 + 1,
        task,
    });
}
fn import_workflow(
    history: Vec<LegacyRecoveryRevision>,
    approvals: &ApprovalLedger,
    knowledge: &KnowledgeState,
) -> RecoveryState {
    let import = RecoveryImport::validate(config(), history, approvals, knowledge).unwrap();
    let (state, effects) = commit(
        RecoveryState::new(config())
            .unwrap()
            .prepare_import("legacy-workflow", import, 100_000)
            .unwrap(),
    );
    assert!(effects.is_empty());
    assert!(state.recovery_required());
    state
}

#[test]
fn legacy_full_history_preserves_random_identity_budget_and_resume_operation() {
    let (history, approvals, knowledge) = legacy_revisions();
    let old = history.last().unwrap().task.clone();
    let state = import_workflow(history, &approvals, &knowledge);
    let task = state.task(&old.id).unwrap();
    assert_eq!(task.stage, RecoveryStage::Paused);
    assert_eq!(task.episode_count, 1);
    assert_eq!(task.revision, 5);
    assert_eq!(task.diagnosis_attempts, 1);
    assert_eq!(task.operation, old.operation);
    let state = RecoveryState::restore(config(), state.entries()).unwrap();
    let (state, effects) = commit(
        state
            .prepare(
                "recover",
                RecoveryCommand::Event(RecoveryEvent::Recover),
                100_000,
                &knowledge,
            )
            .unwrap(),
    );
    assert!(effects.is_empty());
    let (state, effects) = commit(
        state
            .prepare(
                "resume",
                RecoveryCommand::Event(RecoveryEvent::Resume {
                    task_id: old.id.clone(),
                    revision: 5,
                }),
                100_000,
                &knowledge,
            )
            .unwrap(),
    );
    assert!(effects.is_empty());
    assert_eq!(state.task(&old.id).unwrap().operation, old.operation);
}

#[test]
fn legacy_history_rejects_missing_or_changed_authority_and_revision_facts() {
    for corruption in 0..7 {
        let (mut history, approvals, knowledge) = legacy_revisions();
        match corruption {
            0 => {
                history.remove(1);
            }
            1 => history[4].task.diagnosis_attempts = 0,
            2 => {
                history[4].task.operation.as_mut().unwrap().action["timeout_secs"] =
                    serde_json::json!(99)
            }
            3 => history[4].task.approval_id = Some("other".into()),
            4 => history[4].task.episode_count = 9,
            5 => history[4].task.plan.as_mut().unwrap().script.source = "changed".into(),
            _ => history[0].format = 2,
        }
        assert!(
            RecoveryImport::validate(config(), history, &approvals, &knowledge).is_err(),
            "corruption {corruption}"
        );
    }
}

#[test]
fn legacy_uncertain_unconsumed_intent_is_sealed_without_new_execution() {
    for stage in [LegacyRecoveryStage::Executing, LegacyRecoveryStage::Unknown] {
        let (mut history, approvals, knowledge) = legacy_revisions();
        push_legacy(&mut history, LegacyRecoveryStage::Executing);
        if stage == LegacyRecoveryStage::Unknown {
            push_legacy(&mut history, stage);
        }
        let import =
            RecoveryImport::validate(config(), history.clone(), &approvals, &knowledge).unwrap();
        let mut proofs = import.execution_uncertainties();
        assert_eq!(proofs.len(), 1);
        assert!(
            RecoveryState::new(config())
                .unwrap()
                .prepare_import("unsafe-import", import, 100_000)
                .is_err()
        );
        let (approvals, effects) = commit(
            approvals
                .prepare_legacy_uncertain("seal".into(), proofs.remove(0), 100)
                .unwrap(),
        );
        assert!(effects.is_empty());
        assert_eq!(
            approvals.get("approval-0000000000000001").unwrap().state,
            ApprovalState::Unknown
        );
        let state = import_workflow(history, &approvals, &knowledge);
        let task = state.tasks().next().unwrap();
        assert_eq!(task.stage, RecoveryStage::Unknown);
        assert!(state.is_quarantined("script", 1));
        assert_eq!(state.pending_deliveries().len(), 1);
        let (state, effects) = commit(
            state
                .prepare(
                    "recover",
                    RecoveryCommand::Event(RecoveryEvent::Recover),
                    100_000,
                    &knowledge,
                )
                .unwrap(),
        );
        assert!(effects.is_empty());
        let task = state.tasks().next().unwrap();
        assert!(
            state
                .prepare(
                    "resume",
                    RecoveryCommand::Event(RecoveryEvent::Resume {
                        task_id: task.id.clone(),
                        revision: task.revision
                    }),
                    100_000,
                    &knowledge
                )
                .is_err()
        );
        let restored = RecoveryState::restore(config(), state.entries()).unwrap();
        assert!(restored.is_quarantined("script", 1));
    }
}

fn legacy_publication() -> (Vec<LegacyRecoveryRevision>, ApprovalLedger, KnowledgeState) {
    let (mut history, approvals, knowledge) = legacy_revisions();
    let approvals = commit(
        approvals
            .prepare_recovery("approval-recover".into(), 100)
            .unwrap(),
    )
    .0;
    let (approvals, mut effects) = commit(
        approvals
            .prepare(
                "consume".into(),
                ApprovalEvent::Changed {
                    request_id: "approval-0000000000000001".into(),
                    change: ApprovalChange::Consume,
                },
                Some(&policy()),
                100,
            )
            .unwrap(),
    );
    let ApprovalEffect::Execute(permit) = effects.remove(0) else {
        panic!("permit")
    };
    let approvals = commit(
        approvals
            .prepare_complete(
                "receipt".into(),
                permit,
                ExecutionOutcome::Executed,
                "original receipt".into(),
                100,
            )
            .unwrap(),
    )
    .0;
    push_legacy(&mut history, LegacyRecoveryStage::Executing);
    push_legacy(&mut history, LegacyRecoveryStage::Verifying);
    history.last_mut().unwrap().task.receipt = Some(ScriptReceipt {
        execution_trace: Vec::new(),
        operation_id: "legacy-original-operation".into(),
        target_id: "target".into(),
        outcome: ScriptOutcome::Executed,
        executor_stopped: true,
        evidence_refs: vec!["receipt:original".into()],
        summary: "original receipt".into(),
    });
    push_legacy(&mut history, LegacyRecoveryStage::Publishing);
    history.last_mut().unwrap().task.verification = Some(BusinessVerification {
        operation_id: "legacy-original-operation".into(),
        target_id: "target".into(),
        profile: "business".into(),
        healthy: Some(true),
        executor_stopped: true,
        evidence_refs: vec!["verification:original".into()],
        verified_at_ms: 100_000,
    });
    (history, approvals, knowledge)
}
fn deliver_legacy(mut knowledge: KnowledgeState, delivery: &KnowledgeDelivery) -> KnowledgeState {
    knowledge = commit(
        knowledge
            .propose(
                format!("candidate-{}", knowledge.revision()),
                KnowledgeCommand::UpsertCandidate(delivery.candidate.clone()),
            )
            .unwrap(),
    )
    .0;
    let proof = delivery.verification.as_ref().map(|v| {
        TrustedBusinessVerification::attest(
            &v.operation_id,
            &v.target_id,
            &v.script_id,
            v.script_version,
            &v.verifier_id,
            v.evidence_refs.clone(),
            v.verified_at_ms,
        )
        .unwrap()
    });
    commit(
        knowledge
            .propose(
                format!("case-{}", knowledge.revision()),
                KnowledgeCommand::RecordOutcome {
                    record_id: delivery.candidate.id.clone(),
                    case: delivery.case.clone(),
                    verification: proof,
                },
            )
            .unwrap(),
    )
    .0
}
#[test]
fn legacy_publication_keeps_exact_case_identity_and_precommitted_timestamp() {
    let (history, approvals, knowledge) = legacy_publication();
    let state = import_workflow(history.clone(), &approvals, &knowledge);
    assert_eq!(
        state.tasks().next().unwrap().stage,
        RecoveryStage::Completed
    );
    let delivery = state.pending_deliveries().remove(0);
    assert_eq!(delivery.case.id, "legacy-original-operation-Verified");
    assert_eq!(delivery.case.recorded_at_ms, 100_000);
    let knowledge = deliver_legacy(knowledge, &delivery);
    let imported = import_workflow(history.clone(), &approvals, &knowledge);
    assert!(imported.pending_deliveries().is_empty());
    let mut bad = history;
    bad.last_mut().unwrap().task.updated_at_ms += 1;
    assert!(RecoveryImport::validate(config(), bad, &approvals, &knowledge).is_err());
}

#[test]
fn legacy_terminal_and_case_associations_cannot_be_substituted() {
    let (mut history, approvals, knowledge) = legacy_publication();
    push_legacy(&mut history, LegacyRecoveryStage::Completed);
    history.last_mut().unwrap().task.knowledge_id = Some("unrelated-candidate".into());
    assert!(RecoveryImport::validate(config(), history, &approvals, &knowledge).is_err());
    let (mut history, approvals, knowledge) = legacy_revisions();
    push_legacy(&mut history, LegacyRecoveryStage::Executing);
    let import =
        RecoveryImport::validate(config(), history.clone(), &approvals, &knowledge).unwrap();
    let proof = import.execution_uncertainties().remove(0);
    let approvals = commit(
        approvals
            .prepare_legacy_uncertain("seal".into(), proof, 100)
            .unwrap(),
    )
    .0;
    let state = import_workflow(history.clone(), &approvals, &knowledge);
    let mut delivery = state.pending_deliveries().remove(0);
    delivery.case.operation_id = "unrelated-operation".into();
    let knowledge = deliver_legacy(knowledge, &delivery);
    assert!(RecoveryImport::validate(config(), history, &approvals, &knowledge).is_err());
}

#[test]
fn legacy_imported_unknown_reconciles_without_losing_prior_quarantine_or_case_identity() {
    let (mut history, approvals, knowledge) = legacy_revisions();
    push_legacy(&mut history, LegacyRecoveryStage::Executing);
    let import =
        RecoveryImport::validate(config(), history.clone(), &approvals, &knowledge).unwrap();
    let proof = import.execution_uncertainties().remove(0);
    let approvals = commit(
        approvals
            .prepare_legacy_uncertain("seal".into(), proof, 100)
            .unwrap(),
    )
    .0;
    let state = import_workflow(history, &approvals, &knowledge);
    let id = state.tasks().next().unwrap().id.clone();
    let original = state.pending_deliveries().remove(0);
    let mut flow = Flow {
        state,
        knowledge,
        approvals,
        id,
    };
    flow.step(RecoveryEvent::Recover);
    flow.approvals = commit(
        flow.approvals
            .prepare_recovery("approval-recover".into(), 100)
            .unwrap(),
    )
    .0;
    let execution = ExecutionResultCheck {
        operation_id: "legacy-original-operation".into(),
        target_id: "target".into(),
        executor_id: "executor".into(),
        outcome: CheckedExecution::Executed,
        executor_stopped: true,
        evidence_refs: vec!["result:independent".into()],
        checked_at_ms: 100_000,
    };
    let verification = flow.verification(Some(true));
    let event = |flow: &Flow| RecoveryEvent::ResultChecked {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        execution: execution.clone(),
        verification: verification.clone(),
        actor: "operator".into(),
        approval: flow
            .approvals
            .get("approval-0000000000000001")
            .unwrap()
            .clone(),
    };
    flow.step(event(&flow));
    assert_eq!(flow.task().stage, RecoveryStage::Unknown);
    let record = flow.approvals.get("approval-0000000000000001").unwrap();
    flow.approvals = commit(
        flow.approvals
            .prepare(
                "reconcile".into(),
                ApprovalEvent::Changed {
                    request_id: record.request.request_id.clone(),
                    change: ApprovalChange::Reconcile {
                        outcome: ExecutionOutcome::Executed,
                        actor: "operator".into(),
                        reason: "result:independent".into(),
                    },
                },
                None,
                100,
            )
            .unwrap(),
    )
    .0;
    flow.step(event(&flow));
    assert_eq!(flow.task().stage, RecoveryStage::Completed);
    let deliveries = flow.state.pending_deliveries();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0].case, original.case);
    assert_eq!(deliveries[1].candidate, original.candidate);
    assert_eq!(deliveries[1].case.id, "legacy-original-operation-Verified");
    for delivery in deliveries {
        flow.knowledge = deliver_legacy(flow.knowledge, &delivery);
        flow.step(RecoveryEvent::DeliveryConfirmed {
            delivery_id: delivery.id,
        });
    }
    assert!(flow.state.is_quarantined("script", 1));
    assert!(flow.knowledge.is_quarantined("script", 1));
    assert_eq!(
        flow.knowledge
            .get(&original.candidate.id)
            .unwrap()
            .cases
            .len(),
        2
    );
    RecoveryState::restore(config(), flow.state.entries()).unwrap();
}

#[test]
fn legacy_task_id_collision_keeps_prior_incident_episode() {
    let (history, _, knowledge) = legacy_revisions();
    let mut first = history[0].clone();
    first.task.id = "task-0000000000000003".into();
    let mut history = vec![first];
    push_legacy(&mut history, LegacyRecoveryStage::Canceled);
    let approvals = ApprovalLedger::new(ApprovalLimits::default()).unwrap();
    let state = import_workflow(history, &approvals, &knowledge);
    let (state, _) = commit(
        state
            .prepare(
                "recover",
                RecoveryCommand::Event(RecoveryEvent::Recover),
                100_000,
                &knowledge,
            )
            .unwrap(),
    );
    let (state, _) = commit(
        state
            .prepare(
                "new",
                RecoveryCommand::Event(RecoveryEvent::Register {
                    problem: problem("second-incident"),
                    incident: IncidentEvidence {
                        incident_id: "second-incident".into(),
                        revision: 1,
                        active: true,
                    },
                }),
                100_000,
                &knowledge,
            )
            .unwrap(),
    );
    assert_eq!(state.tasks().count(), 2);
    assert_eq!(
        state
            .task("task-0000000000000003")
            .unwrap()
            .problem
            .incident_id,
        "legacy-incident"
    );
    assert_eq!(
        state.task("task-0000000000000004").unwrap().episode_count,
        2
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
    assert!(flow.task().plan.is_none());
    assert_eq!(flow.task().diagnosis_attempts, 0);
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
    assert!(flow.state.pending_deliveries().is_empty());
    let job = summarize(&mut flow);
    let experience = job.record("linux").unwrap();
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
    assert!(flow.task().plan.is_none());
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
    let mut flow = Flow::new();
    flow.register("incident");
    assert!(
        flow.state
            .prepare(
                "not-delegated",
                RecoveryCommand::StartRepair {
                    task_id: flow.id.clone(),
                    revision: flow.task().revision,
                    observation: observation(),
                },
                100_000,
                &flow.knowledge
            )
            .is_err()
    );
    assert_eq!(flow.task().stage, RecoveryStage::Queued);
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
    let mut script = plan(1).script;
    script.id = format!(
        "{}-action",
        flow.task().operation.as_ref().unwrap().operation_id
    );
    flow.step(RecoveryEvent::RepairActionPrepared {
        task_id: flow.id.clone(),
        revision: flow.task().revision,
        script: script.clone(),
    });
    assert!(
        flow.state
            .prepare(
                "second-action",
                RecoveryCommand::Event(RecoveryEvent::RepairActionPrepared {
                    task_id: flow.id.clone(),
                    revision: flow.task().revision,
                    script: script.clone(),
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
    report.scriptability = Scriptability::Possible {
        reason: "Can be automated but not tested".into(),
        candidate: Some(plan(8).script),
    };
    flow.step(RecoveryEvent::ExperienceSummarized {
        job_id: job.id,
        call_id: call_id.clone(),
        report,
    });
    let job = flow.state.pending_experiences()[0].clone();
    let item = job.record("linux").unwrap();
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
    assert!(flow.knowledge.search_reusable(&query).unwrap().is_empty());
}
