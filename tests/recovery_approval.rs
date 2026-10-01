use super::*;
use recuvora_core::recovery::approval::{
    ApprovalError, ApprovalRecord, ApprovalStore, ApprovalStoreConfig,
};

#[derive(Clone, Copy, Debug)]
enum ApprovalBoundary {
    BeforeRequest,
    BeforeAssociation,
    AfterAssociation,
}

fn recovery_config(reviewer: ReviewerConfig) -> RecoveryConfig {
    let mut config = config();
    config.max_diagnoses = 1;
    config.approval.reviewer = reviewer;
    config
}

fn human_then_harness() -> ReviewerConfig {
    ReviewerConfig::HumanThenHarness {
        harness_id: "reviewer".into(),
        human_wait_secs: 15,
        review_timeout_secs: 30,
    }
}

async fn waiting_repair(
    dir: &TestDir,
    config: &RecoveryConfig,
    backend: &Arc<Backend>,
    clock: &Arc<Clock>,
) -> (Arc<RecoveryService>, RecoveryTask, ApprovalRecord) {
    let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
    let submitted = recovery
        .submit(problem("incident-approval-commit"))
        .unwrap();
    let task = recovery
        .advance(&submitted.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(task.stage, RecoveryStage::AwaitingApproval);
    assert_eq!(task.diagnosis_attempts, 1);
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("execute"), 0);
    let approval = recovery.approval(&task.id).unwrap().unwrap();
    (recovery, task, approval)
}

fn stop_at_boundary(dir: &TestDir, boundary: ApprovalBoundary, original: &RecoveryTask) {
    if matches!(boundary, ApprovalBoundary::AfterAssociation) {
        return;
    }
    // Remove one complete synced association, never a partial or corrupt line.
    remove_last_journal_entry(&dir.path.join("recovery.jsonl"));
    let journal = std::fs::read_to_string(dir.path.join("recovery.jsonl")).unwrap();
    let intent: serde_json::Value = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    assert_eq!(intent["task"]["stage"], "awaiting_approval");
    assert!(intent["task"]["approval_id"].is_null());
    assert_eq!(
        intent["task"]["plan"],
        serde_json::to_value(&original.plan).unwrap()
    );
    assert_eq!(
        intent["task"]["operation"],
        serde_json::to_value(&original.operation).unwrap()
    );
    if matches!(boundary, ApprovalBoundary::BeforeRequest) {
        // The isolated directory contains only this test's original request.
        std::fs::write(dir.path.join("approvals/approvals.jsonl"), []).unwrap();
    }
}

fn assert_original_plan_and_operation(original: &RecoveryTask, recovered: &RecoveryTask) {
    assert_eq!(recovered.id, original.id);
    assert_eq!(recovered.problem, original.problem);
    assert_eq!(recovered.diagnosis_attempts, 1);
    assert_eq!(recovered.operation, original.operation);
    assert_eq!(
        serde_json::to_value(&recovered.plan).unwrap(),
        serde_json::to_value(&original.plan).unwrap()
    );
}

fn assert_one_approval(dir: &TestDir, clock: &Clock, expected: &ApprovalRecord) {
    let store = ApprovalStore::open(
        dir.path.join("approvals"),
        ApprovalStoreConfig::default(),
        clock.now_ms() / 1000,
    )
    .unwrap();
    let approvals = store.list();
    assert_eq!(
        approvals.len(),
        1,
        "recovery must not leave an orphan request"
    );
    assert_eq!(approvals[0].request, expected.request);
}

#[tokio::test]
async fn all_approval_commit_boundaries_preserve_the_plan_and_require_resume_after_restart() {
    for boundary in [
        ApprovalBoundary::BeforeRequest,
        ApprovalBoundary::BeforeAssociation,
        ApprovalBoundary::AfterAssociation,
    ] {
        let dir = TestDir::new("recovery-approval-commit");
        let clock = Arc::new(Clock::new());
        let backend = Arc::new(Backend::new(clock.clone()));
        let config = recovery_config(human_then_harness());
        let (recovery, original, original_approval) =
            waiting_repair(&dir, &config, &backend, &clock).await;
        recovery.shutdown().await.unwrap();
        drop(recovery);
        stop_at_boundary(&dir, boundary, &original);

        let mut expected_approval = None;
        for _ in 0..3 {
            clock.advance(1000);
            let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
            let recovered = recovery.query(&original.id).unwrap().unwrap();
            assert_eq!(recovered.stage, RecoveryStage::AwaitingApproval);
            assert_original_plan_and_operation(&original, &recovered);
            let approval = recovery.approval(&original.id).unwrap().unwrap();
            assert_eq!(
                approval.request.operation,
                *recovered.operation.as_ref().unwrap()
            );
            if let Some(expected) = &expected_approval {
                assert_eq!(&approval, expected);
            } else {
                if !matches!(boundary, ApprovalBoundary::BeforeRequest) {
                    assert_eq!(approval.request, original_approval.request);
                    assert_eq!(approval.human_deadline, original_approval.human_deadline);
                }
                expected_approval = Some(approval.clone());
            }
            assert_eq!(
                recovery.submit(original.problem.clone()).unwrap().id,
                original.id
            );
            assert_eq!(recovery.tasks().unwrap().len(), 1);
            assert_eq!(
                recovery
                    .advance(&original.id, Cancellation::new())
                    .await
                    .unwrap()
                    .stage,
                RecoveryStage::AwaitingApproval
            );
            assert_eq!(backend.count("diagnose"), 1);
            assert_eq!(backend.count("review"), 0);
            assert_eq!(backend.count("execute"), 0);
            recovery.shutdown().await.unwrap();
            drop(recovery);
            assert_one_approval(&dir, &clock, &approval);
        }

        let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
        let approval = recovery.approval(&original.id).unwrap().unwrap();
        recovery
            .decide_human(
                &original.id,
                approval.revision,
                ApprovalDecision::Approve,
                "trusted-operator".into(),
                "reviewed the recovered original operation".into(),
            )
            .unwrap();
        assert_eq!(backend.count("execute"), 0);
        recovery.shutdown().await.unwrap();
        drop(recovery);
        if !matches!(boundary, ApprovalBoundary::AfterAssociation) {
            // Recover an already approved request while its task link is absent.
            stop_at_boundary(&dir, ApprovalBoundary::BeforeAssociation, &original);
        }

        let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
        let paused = recovery
            .advance(&original.id, Cancellation::new())
            .await
            .unwrap();
        assert_eq!(paused.stage, RecoveryStage::Paused);
        assert_original_plan_and_operation(&original, &paused);
        assert_eq!(backend.count("execute"), 0);
        assert!(recovery.resume(&original.id, paused.revision - 1).is_err());
        recovery.resume(&original.id, paused.revision).unwrap();
        assert_eq!(
            drive(&recovery, &original.id).await.stage,
            RecoveryStage::Completed
        );
        recovery.shutdown().await.unwrap();
        drop(recovery);

        for _ in 0..2 {
            let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
            assert_eq!(
                recovery.submit(original.problem.clone()).unwrap().id,
                original.id
            );
            let completed = drive(&recovery, &original.id).await;
            assert_eq!(completed.stage, RecoveryStage::Completed);
            assert_original_plan_and_operation(&original, &completed);
            assert_eq!(backend.count("diagnose"), 1);
            assert_eq!(backend.count("execute"), 1);
            recovery.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn recovered_harness_handoff_keeps_the_original_deadline_and_operation() {
    let dir = TestDir::new("recovery-approval-handoff-recovery");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let config = recovery_config(human_then_harness());
    let (recovery, original, approval) = waiting_repair(&dir, &config, &backend, &clock).await;
    recovery.shutdown().await.unwrap();
    drop(recovery);
    stop_at_boundary(&dir, ApprovalBoundary::BeforeAssociation, &original);
    clock.advance(14_000);
    let recovery = open(&dir.path, config, backend.clone(), clock.clone()).unwrap();
    let recovered = recovery.approval(&original.id).unwrap().unwrap();
    assert_eq!(recovered.request, approval.request);
    assert_eq!(recovered.human_deadline, approval.human_deadline);
    assert_eq!(
        recovery
            .advance(&original.id, Cancellation::new())
            .await
            .unwrap()
            .stage,
        RecoveryStage::AwaitingApproval
    );
    assert_eq!(backend.count("review"), 0);
    clock.advance(1000);
    let completed = drive(&recovery, &original.id).await;
    assert_eq!(completed.stage, RecoveryStage::Completed);
    assert_original_plan_and_operation(&original, &completed);
    assert_eq!(
        recovery.approval(&original.id).unwrap().unwrap().request,
        approval.request
    );
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("review"), 1);
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    assert_one_approval(&dir, &clock, &approval);
}

#[tokio::test]
async fn recovered_human_and_harness_approval_recheck_the_current_environment() {
    for human in [true, false] {
        let dir = TestDir::new("recovery-approval-environment-recovery");
        let clock = Arc::new(Clock::new());
        let backend = Arc::new(Backend::new(clock.clone()));
        let config = recovery_config(human_then_harness());
        let (recovery, original, _) = waiting_repair(&dir, &config, &backend, &clock).await;
        recovery.shutdown().await.unwrap();
        drop(recovery);
        stop_at_boundary(&dir, ApprovalBoundary::BeforeAssociation, &original);
        let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
        let approval = recovery.approval(&original.id).unwrap().unwrap();
        if human {
            recovery
                .decide_human(
                    &original.id,
                    approval.revision,
                    ApprovalDecision::Approve,
                    "trusted-operator".into(),
                    "reviewed exact original action".into(),
                )
                .unwrap();
        } else {
            clock.advance(15_000);
        }
        backend
            .state
            .lock()
            .unwrap()
            .facts
            .insert("runtime_version".into(), "2".into());
        assert!(matches!(
            recovery.advance(&original.id, Cancellation::new()).await,
            Err(RecoveryError::Invalid(_))
        ));
        assert_original_plan_and_operation(
            &original,
            &recovery.query(&original.id).unwrap().unwrap(),
        );
        assert_eq!(backend.count("diagnose"), 1);
        assert_eq!(backend.count("review"), 0);
        assert_eq!(backend.count("execute"), 0);
        assert_eq!(
            recovery.approval(&original.id).unwrap().unwrap().state,
            if human {
                ApprovalState::Approved
            } else {
                ApprovalState::WaitingHuman
            }
        );
        recovery.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn unlinked_approval_requires_the_original_operation_and_current_hard_policy() {
    let dir = TestDir::new("recovery-approval-binding-recovery");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let config = recovery_config(human_then_harness());
    let (recovery, original, approval) = waiting_repair(&dir, &config, &backend, &clock).await;
    recovery.shutdown().await.unwrap();
    drop(recovery);
    stop_at_boundary(&dir, ApprovalBoundary::BeforeAssociation, &original);
    let path = dir.path.join("approvals/approvals.jsonl");
    let original_journal = std::fs::read_to_string(&path).unwrap();
    let entry: serde_json::Value = serde_json::from_str(original_journal.trim()).unwrap();
    let replacements = [
        (
            "/event/request/operation/task_revision",
            serde_json::json!(original.operation.as_ref().unwrap().task_revision + 1),
        ),
        (
            "/event/request/operation/target",
            serde_json::json!("other-target"),
        ),
        (
            "/event/request/operation/action/script/source",
            serde_json::json!("exit 1"),
        ),
        ("/event/request/policy/version", serde_json::json!(2)),
        (
            "/event/request/policy/allowed_targets",
            serde_json::json!(["other-target"]),
        ),
        (
            "/event/request/policy/allowed_action_kinds",
            serde_json::json!(["other_action"]),
        ),
        (
            "/event/request/policy/reviewer/harness_id",
            serde_json::json!("other-reviewer"),
        ),
    ];
    for (pointer, replacement) in replacements {
        let mut changed = entry.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&changed).unwrap()),
        )
        .unwrap();
        assert!(
            matches!(
                open(&dir.path, config.clone(), backend.clone(), clock.clone()),
                Err(RecoveryError::Approval(ApprovalError::Conflict))
            ),
            "recovery accepted changed binding at {pointer}"
        );
        assert_eq!(backend.count("diagnose"), 1);
        assert_eq!(backend.count("execute"), 0);
    }
    std::fs::write(&path, original_journal).unwrap();
    let mut changed_config = config.clone();
    changed_config.approval.version += 1;
    assert!(matches!(
        open(&dir.path, changed_config, backend.clone(), clock.clone()),
        Err(RecoveryError::Corrupt(_))
    ));
    let recovery = open(&dir.path, config, backend.clone(), clock).unwrap();
    assert_eq!(
        recovery.approval(&original.id).unwrap().unwrap().request,
        approval.request
    );
    assert_original_plan_and_operation(&original, &recovery.query(&original.id).unwrap().unwrap());
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn approval_capacity_retries_keep_the_durable_intent_without_rediagnosis() {
    let dir = TestDir::new("recovery-approval-capacity-recovery");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let config = recovery_config(ReviewerConfig::Human);
    {
        let mut store = ApprovalStore::open(
            dir.path.join("approvals"),
            ApprovalStoreConfig::default(),
            clock.now_ms() / 1000,
        )
        .unwrap();
        let mut scope = config.approval.clone();
        scope.allowed_targets = vec!["other-target".into()];
        store
            .request(
                recuvora_core::recovery::approval::ProposedOperation {
                    task_id: "other-task".into(),
                    task_revision: 1,
                    operation_id: "other-operation".into(),
                    target: "other-target".into(),
                    action: serde_json::json!({"kind": "execute_script"}),
                },
                scope,
                clock.now_ms() / 1000,
            )
            .unwrap();
    }
    let recovery = RecoveryService::open_with_clock_and_store_configs(
        &dir.path,
        config.clone(),
        backend.clone(),
        clock.clone(),
        ApprovalStoreConfig {
            max_requests: 1,
            ..ApprovalStoreConfig::default()
        },
        KnowledgeStoreConfig::default(),
    )
    .unwrap();
    recovery.bind_incident_guard(backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let submitted = recovery
        .submit(problem("incident-approval-capacity"))
        .unwrap();
    assert!(matches!(
        recovery.advance(&submitted.id, Cancellation::new()).await,
        Err(RecoveryError::Approval(ApprovalError::Capacity))
    ));
    let original = recovery.query(&submitted.id).unwrap().unwrap();
    assert_eq!(original.stage, RecoveryStage::AwaitingApproval);
    assert!(original.approval_id.is_none());
    assert!(original.plan.is_some());
    assert!(original.operation.is_some());
    for canceled in [false, true, false] {
        let cancellation = Cancellation::new();
        if canceled {
            cancellation.cancel();
        }
        assert!(matches!(
            recovery.advance(&submitted.id, cancellation).await,
            Err(RecoveryError::Approval(ApprovalError::Capacity))
        ));
        let intent = recovery.query(&submitted.id).unwrap().unwrap();
        assert_eq!(intent.stage, RecoveryStage::AwaitingApproval);
        assert_original_plan_and_operation(&original, &intent);
        assert!(intent.approval_id.is_none());
        assert!(recovery.approval(&submitted.id).unwrap().is_none());
        assert_eq!(backend.count("diagnose"), 1);
        assert_eq!(backend.count("execute"), 0);
    }
    recovery.shutdown().await.unwrap();
    drop(recovery);

    let recovery = RecoveryService::open_with_clock_and_store_configs(
        &dir.path,
        config,
        backend.clone(),
        clock.clone(),
        ApprovalStoreConfig {
            max_requests: 2,
            ..ApprovalStoreConfig::default()
        },
        KnowledgeStoreConfig::default(),
    )
    .unwrap();
    recovery.bind_incident_guard(backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let recovered = recovery.query(&submitted.id).unwrap().unwrap();
    assert_original_plan_and_operation(&original, &recovered);
    let approval = recovery.approval(&submitted.id).unwrap().unwrap();
    recovery
        .decide_human(
            &submitted.id,
            approval.revision,
            ApprovalDecision::Approve,
            "trusted-operator".into(),
            "reviewed the recovered original action".into(),
        )
        .unwrap();
    assert_eq!(
        drive(&recovery, &submitted.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let store = ApprovalStore::open(
        dir.path.join("approvals"),
        ApprovalStoreConfig::default(),
        clock.now_ms() / 1000,
    )
    .unwrap();
    assert_eq!(store.list().len(), 2);
    assert_eq!(
        store.get(&approval.request.request_id).unwrap().request,
        approval.request
    );
}

#[tokio::test]
async fn recovering_an_expired_unlinked_request_does_not_renew_its_authority() {
    let dir = TestDir::new("recovery-approval-expired-recovery");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let config = recovery_config(ReviewerConfig::Human);
    let (recovery, original, approval) = waiting_repair(&dir, &config, &backend, &clock).await;
    recovery.shutdown().await.unwrap();
    drop(recovery);
    stop_at_boundary(&dir, ApprovalBoundary::BeforeAssociation, &original);
    clock.advance(config.approval.ttl_secs * 1000);
    let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
    let recovered = recovery.approval(&original.id).unwrap().unwrap();
    assert_eq!(recovered.request, approval.request);
    assert_eq!(recovered.request.expires_at, clock.now_ms() / 1000);
    let denied = drive(&recovery, &original.id).await;
    assert_eq!(denied.stage, RecoveryStage::Denied);
    assert_original_plan_and_operation(&original, &denied);
    assert_eq!(
        recovery.approval(&original.id).unwrap().unwrap().state,
        ApprovalState::Expired
    );
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("execute"), 0);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let recovery = open(&dir.path, config, backend.clone(), clock.clone()).unwrap();
    assert_eq!(
        recovery.submit(original.problem.clone()).unwrap().id,
        original.id
    );
    assert_eq!(
        drive(&recovery, &original.id).await.stage,
        RecoveryStage::Denied
    );
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("execute"), 0);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    assert_one_approval(&dir, &clock, &approval);
}

#[tokio::test]
async fn recovered_unlinked_consumed_permit_is_unknown_and_cannot_be_dispatched_again() {
    let dir = TestDir::new("recovery-approval-consumed-recovery");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let config = recovery_config(ReviewerConfig::Human);
    let (recovery, original, approval) = waiting_repair(&dir, &config, &backend, &clock).await;
    recovery
        .decide_human(
            &original.id,
            approval.revision,
            ApprovalDecision::Approve,
            "trusted-operator".into(),
            "reviewed exact original action".into(),
        )
        .unwrap();
    recovery.shutdown().await.unwrap();
    drop(recovery);
    stop_at_boundary(&dir, ApprovalBoundary::BeforeAssociation, &original);
    {
        let mut store = ApprovalStore::open(
            dir.path.join("approvals"),
            ApprovalStoreConfig::default(),
            clock.now_ms() / 1000,
        )
        .unwrap();
        // Persist a consumed capability and lose its owner before any receipt.
        let permit = store
            .consume(
                &approval.request.request_id,
                original.operation.as_ref().unwrap(),
                &config.approval,
                clock.now_ms() / 1000,
            )
            .unwrap();
        drop(permit);
    }
    for _ in 0..3 {
        let recovery = open(&dir.path, config.clone(), backend.clone(), clock.clone()).unwrap();
        let unknown = drive(&recovery, &original.id).await;
        assert_eq!(unknown.stage, RecoveryStage::Unknown);
        assert_original_plan_and_operation(&original, &unknown);
        assert_eq!(
            recovery.approval(&original.id).unwrap().unwrap().state,
            ApprovalState::Unknown
        );
        assert_eq!(
            recovery.approval(&original.id).unwrap().unwrap().request,
            approval.request
        );
        assert_eq!(
            recovery.submit(original.problem.clone()).unwrap().id,
            original.id
        );
        assert!(matches!(
            recovery.submit(problem("another-incident")),
            Err(RecoveryError::Busy)
        ));
        assert!(recovery.resume(&original.id, unknown.revision).is_err());
        assert_eq!(backend.count("diagnose"), 1);
        assert_eq!(backend.count("execute"), 0);
        recovery.shutdown().await.unwrap();
        drop(recovery);
        assert_one_approval(&dir, &clock, &approval);
    }
}
