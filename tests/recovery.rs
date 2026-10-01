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
        let pending = self
            .approvals
            .prepare_request(
                format!("request-{}", self.approvals.revision()),
                op,
                policy(),
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
                format!("assess-{}", self.approvals.revision()),
                ApprovalEvent::Changed {
                    request_id: id.clone(),
                    change: ApprovalChange::Assess {
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
                Some(&policy()),
                100,
            )
            .unwrap();
        self.approvals = commit(pending).0;
        id
    }
    fn consume(&mut self, id: &str) -> ExecutionPermit {
        let pending = self
            .approvals
            .prepare(
                format!("consume-{}", self.approvals.revision()),
                ApprovalEvent::Changed {
                    request_id: id.into(),
                    change: ApprovalChange::Consume,
                },
                Some(&policy()),
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
            ApprovalChange::Assess { assessment }
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
