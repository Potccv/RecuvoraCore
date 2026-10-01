use super::*;

#[tokio::test]
async fn result_check_requires_current_revision_without_replaying_or_renewing_execution() {
    let dir = TestDir::new("recovery-result-check-authority");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    backend
        .state
        .lock()
        .unwrap()
        .outcomes
        .push_back((ScriptOutcome::Unknown, false));
    let service = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let submitted = service.submit(problem("incident-result-check")).unwrap();
    let unknown = drive(&service, &submitted.id).await;
    assert_eq!(unknown.stage, RecoveryStage::Unknown);
    let original_request = service.approval(&submitted.id).unwrap().unwrap().request;
    let approval_id = unknown.approval_id.clone();
    let operation_id = unknown.operation.as_ref().unwrap().operation_id.clone();
    let execution = execution_evidence(&unknown, CheckedExecution::Executed, &clock);
    let mut incomplete_health = business_evidence(&unknown, &clock);
    incomplete_health.healthy = None;
    let checked = service
        .check_result(
            &submitted.id,
            unknown.revision,
            execution.clone(),
            incomplete_health,
            "trusted-operator".into(),
        )
        .unwrap();
    assert_eq!(checked.stage, RecoveryStage::Unknown);
    assert!(checked.revision > unknown.revision);
    assert_eq!(checked.result_check.as_ref().unwrap().execution, execution);
    assert_eq!(
        checked.result_check.as_ref().unwrap().actor,
        "trusted-operator"
    );
    let approval = service.approval(&submitted.id).unwrap().unwrap();
    assert_eq!(approval.state, ApprovalState::Executed);
    assert_eq!(approval.request, original_request);
    assert!(matches!(
        service.check_result(
            &submitted.id,
            unknown.revision,
            execution.clone(),
            business_evidence(&checked, &clock),
            "trusted-operator".into(),
        ),
        Err(RecoveryError::Busy)
    ));
    assert_eq!(
        service.query(&submitted.id).unwrap().unwrap().revision,
        checked.revision
    );
    assert!(service.resume(&submitted.id, checked.revision).is_err());
    assert_eq!(
        drive(&service, &submitted.id).await.stage,
        RecoveryStage::Unknown
    );
    assert_eq!(backend.count("execute"), 1);

    service
        .check_result(
            &submitted.id,
            checked.revision,
            execution.clone(),
            business_evidence(&checked, &clock),
            "trusted-operator".into(),
        )
        .unwrap();
    let completed = drive(&service, &submitted.id).await;
    assert_eq!(completed.stage, RecoveryStage::Completed);
    assert_eq!(completed.approval_id, approval_id);
    assert_eq!(
        completed.operation.as_ref().unwrap().operation_id,
        operation_id
    );
    assert_eq!(
        service.approval(&submitted.id).unwrap().unwrap().request,
        original_request
    );
    assert!(
        service
            .check_result(
                &submitted.id,
                completed.revision,
                execution,
                business_evidence(&completed, &clock),
                "trusted-operator".into(),
            )
            .is_err()
    );
    assert!(service.resume(&submitted.id, completed.revision).is_err());
    assert_eq!(
        drive(&service, &submitted.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("review"), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn current_journal_fields_are_strict_and_unknown_retains_cross_directory_ownership() {
    let dir = TestDir::new("recovery-current-journal");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    backend
        .state
        .lock()
        .unwrap()
        .outcomes
        .push_back((ScriptOutcome::Unknown, false));
    let service = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let task = service.submit(problem("incident-current-journal")).unwrap();
    let unknown = drive(&service, &task.id).await;
    let checked = service
        .check_result(
            &task.id,
            unknown.revision,
            execution_evidence(&unknown, CheckedExecution::Unknown, &clock),
            business_evidence(&unknown, &clock),
            "trusted-operator".into(),
        )
        .unwrap();
    assert_eq!(checked.stage, RecoveryStage::Unknown);
    assert_eq!(
        service.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Unknown
    );
    assert!(dir.path.join("recovery.lock").is_file());
    assert!(dir.path.join("recovery.jsonl").is_file());
    service.shutdown().await.unwrap();
    drop(service);

    let journal = std::fs::read_to_string(dir.path.join("recovery.jsonl")).unwrap();
    let entry: serde_json::Value = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    let saved = &entry["task"];
    assert_eq!(entry["format"], 2);
    assert!(saved.get("reconciliation").is_none());
    assert!(
        saved["result_check"]["execution"]
            .get("reconciled_at_ms")
            .is_none()
    );
    assert_eq!(
        saved["result_check"]["execution"]["checked_at_ms"],
        clock.now_ms()
    );
    let restored_record: RecoveryTask = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(restored_record.revision, checked.revision);
    assert_eq!(restored_record.result_check, checked.result_check);

    for keep_current_field in [false, true] {
        let mut invalid_task = saved.clone();
        let fields = invalid_task.as_object_mut().unwrap();
        let result_check = if keep_current_field {
            fields["result_check"].clone()
        } else {
            fields.remove("result_check").unwrap()
        };
        fields.insert("reconciliation".into(), result_check);
        assert!(serde_json::from_value::<RecoveryTask>(invalid_task).is_err());

        let mut invalid_execution = saved["result_check"]["execution"].clone();
        let fields = invalid_execution.as_object_mut().unwrap();
        let timestamp = if keep_current_field {
            fields["checked_at_ms"].clone()
        } else {
            fields.remove("checked_at_ms").unwrap()
        };
        fields.insert("reconciled_at_ms".into(), timestamp);
        assert!(serde_json::from_value::<ExecutionResultCheck>(invalid_execution.clone()).is_err());
        let mut invalid_task = saved.clone();
        invalid_task["result_check"]["execution"] = invalid_execution;
        assert!(serde_json::from_value::<RecoveryTask>(invalid_task).is_err());
    }

    let authority = FileTargetOwnership::open(dir.path.join("ownership")).unwrap();
    assert!(matches!(
        authority.acquire(
            &CanonicalTarget::new("target-a").unwrap(),
            &dir.path.join("other-state")
        ),
        Err(RecoveryError::Busy)
    ));
    let service = open(&dir.path, config(), backend.clone(), clock.clone()).unwrap();
    let restored = service.query(&task.id).unwrap().unwrap();
    assert_eq!(restored.stage, RecoveryStage::Unknown);
    assert_eq!(restored.result_check, checked.result_check);
    assert_eq!(
        restored
            .result_check
            .as_ref()
            .unwrap()
            .execution
            .checked_at_ms,
        clock.now_ms()
    );
    assert_eq!(
        service.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Unknown
    );
    assert!(matches!(
        service.submit(problem("incident-current-journal-other")),
        Err(RecoveryError::Busy)
    ));
    assert_eq!(
        drive(&service, &task.id).await.stage,
        RecoveryStage::Unknown
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("diagnose"), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn human_approval_reopens_paused_until_current_revision_explicitly_resumes() {
    let dir = TestDir::new("recovery-current-pause");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let mut trusted_config = config();
    trusted_config.approval.reviewer = ReviewerConfig::Human;
    let service = open(
        &dir.path,
        trusted_config.clone(),
        backend.clone(),
        clock.clone(),
    )
    .unwrap();
    let task = service.submit(problem("incident-current-pause")).unwrap();
    assert_eq!(
        service
            .advance(&task.id, Cancellation::new())
            .await
            .unwrap()
            .stage,
        RecoveryStage::AwaitingApproval
    );
    let approval = service.approval(&task.id).unwrap().unwrap();
    let approved = service
        .decide_human(
            &task.id,
            approval.revision,
            ApprovalDecision::Approve,
            "trusted-operator".into(),
            "reviewed exact operation".into(),
        )
        .unwrap();
    assert_eq!(approved.state, ApprovalState::Approved);
    assert_eq!(backend.count("execute"), 0);
    service.shutdown().await.unwrap();
    drop(service);

    let service = open(&dir.path, trusted_config, backend.clone(), clock).unwrap();
    let paused = service
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(paused.stage, RecoveryStage::Paused);
    assert_eq!(backend.count("execute"), 0);
    assert_eq!(
        service.approval(&task.id).unwrap().unwrap().request,
        approved.request
    );
    assert!(matches!(
        service.resume(&task.id, paused.revision - 1),
        Err(RecoveryError::Busy)
    ));
    let unchanged = service.query(&task.id).unwrap().unwrap();
    assert_eq!(unchanged.stage, RecoveryStage::Paused);
    assert_eq!(unchanged.revision, paused.revision);
    service.resume(&task.id, paused.revision).unwrap();
    assert_eq!(
        drive(&service, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(
        service.approval(&task.id).unwrap().unwrap().request,
        approved.request
    );
    assert_eq!(backend.count("execute"), 1);
    assert_eq!(backend.count("diagnose"), 1);
    service.shutdown().await.unwrap();
}
