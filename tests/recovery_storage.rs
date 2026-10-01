use super::{storage::TaskStore, *};
use crate::recovery::workflow_test_support::TestDir;
use serde_json::json;
use std::{fs::OpenOptions, io::Write};

fn config() -> RecoveryConfig {
    let policy = json!({"id":"policy","version":1,"reviewer":{"mode":"harness","harness_id":"reviewer"},"delegation":"scoped target repair","allowed_targets":["target-a"],"allowed_action_kinds":["execute_script"],"ttl_secs":60});
    serde_json::from_value(json!({
        "schema_version":1,"execution_harness":"execution",
        "target":{"target_id":"target-a","executor_id":"target-node","platform":"portable","allowed_languages":["python"],"diagnostic_queries":["snapshot"],"verification_profile":"readiness","required_facts":{"version":"1"},"action_timeout_secs":10},
        "approval":policy,"script_approval":policy,
        "diagnosis_timeout_secs":10,"review_timeout_secs":10,"max_tool_calls":4,"max_diagnoses":2,
        "minimum_script_occurrences":1,"max_tasks":10,"max_journal_bytes":1048576
    })).unwrap()
}

fn task() -> RecoveryTask {
    RecoveryTask {
        id: "task-a".into(),
        revision: 0,
        episode_count: 1,
        problem: ProblemContext {
            incident_id: "incident-a".into(),
            incident_revision: 1,
            target_id: "target-a".into(),
            fingerprint: "not-ready".into(),
            summary: "condition active".into(),
            occurrences: 1,
            keywords: vec!["readiness".into()],
            conditions: BTreeMap::from([("version".into(), "1".into())]),
            evidence_refs: vec!["incident:1".into()],
        },
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
        created_at_ms: 100,
        updated_at_ms: 100,
    }
}

fn approval_intent(mut current: RecoveryTask, config: &RecoveryConfig) -> RecoveryTask {
    let script = ScriptArtifact {
        id: "script-a".into(),
        version: 1,
        language: "python".into(),
        platform: "portable".into(),
        source: "print('stored proposal only')".into(),
        preconditions: config.target.required_facts.clone(),
        generated_by_harness: "execution".into(),
        generated_in_session: "session-a".into(),
    };
    current.plan = Some(RepairPlan {
        summary: "bounded proposal".into(),
        script: script.clone(),
        reusable: true,
    });
    current.observation = Some(TargetObservation {
        target_id: current.problem.target_id.clone(),
        facts: config.target.required_facts.clone(),
        evidence_refs: vec!["inspect:1".into()],
        observed_at_ms: current.updated_at_ms,
    });
    current.operation = Some(approval::ProposedOperation {
        task_id: current.id.clone(),
        task_revision: current.revision,
        operation_id: "operation-a".into(),
        target: current.problem.target_id.clone(),
        action: json!({
            "kind":"execute_script","executor_id":config.target.executor_id,"script":script,
            "verification_profile":config.target.verification_profile,"required_facts":config.target.required_facts,
            "timeout_secs":config.target.action_timeout_secs,"incident_id":current.problem.incident_id,
            "incident_revision":current.problem.incident_revision
        }),
    });
    current.stage = RecoveryStage::AwaitingApproval;
    current
}

fn illegal_approval_associations(prepared: &RecoveryTask) -> Vec<(&'static str, RecoveryTask)> {
    let mut association = prepared.clone();
    association.approval_id = Some("approval-a".into());
    let mut cases = Vec::new();
    let mut changed = association.clone();
    changed.operation.as_mut().unwrap().operation_id = "replacement-operation".into();
    cases.push(("replacement operation", changed));
    let mut changed = association.clone();
    changed.plan.as_mut().unwrap().script.source = "print('replacement proposal')".into();
    changed.operation.as_mut().unwrap().action["script"] =
        serde_json::to_value(&changed.plan.as_ref().unwrap().script).unwrap();
    cases.push(("replacement script and matching operation", changed));
    let mut changed = association.clone();
    changed.plan.as_mut().unwrap().summary = "replacement summary".into();
    cases.push(("replacement plan", changed));
    let mut changed = association.clone();
    changed.observation.as_mut().unwrap().evidence_refs = vec!["inspect:replacement".into()];
    cases.push(("replacement observation", changed));
    let mut changed = association.clone();
    changed.reused_script = true;
    changed.knowledge_id = Some("case-a".into());
    cases.push(("changed reuse policy and knowledge", changed));
    let mut changed = association.clone();
    changed.knowledge_id = Some("case-a".into());
    cases.push(("changed knowledge association", changed));
    let mut changed = association.clone();
    changed.receipt = Some(ScriptReceipt {
        operation_id: "operation-a".into(),
        target_id: prepared.problem.target_id.clone(),
        outcome: ScriptOutcome::Executed,
        executor_stopped: true,
        evidence_refs: vec!["receipt:unexpected".into()],
        summary: "unexpected execution evidence".into(),
    });
    cases.push(("introduced execution receipt", changed));
    let mut changed = association.clone();
    changed.verification = Some(BusinessVerification {
        operation_id: "operation-a".into(),
        target_id: prepared.problem.target_id.clone(),
        profile: "readiness".into(),
        healthy: Some(true),
        executor_stopped: true,
        evidence_refs: vec!["verification:unexpected".into()],
        verified_at_ms: prepared.updated_at_ms,
    });
    cases.push(("introduced business verification", changed));
    let mut changed = association;
    changed.diagnosis_attempts += 1;
    cases.push(("changed diagnosis budget", changed));
    cases.push(("unassociated self loop", prepared.clone()));
    let mut changed = prepared.clone();
    changed.stage = RecoveryStage::Executing;
    cases.push(("execution before association", changed));
    cases
}

fn illegal_linked_approval_changes(attached: &RecoveryTask) -> Vec<(&'static str, RecoveryTask)> {
    let mut changed = attached.clone();
    changed.stage = RecoveryStage::Executing;
    changed.approval_id = Some("replacement-approval".into());
    let mut cleared = attached.clone();
    cleared.approval_id = None;
    vec![
        ("replacement approval on execution transition", changed),
        ("associated self loop", attached.clone()),
        ("cleared approval association", cleared),
    ]
}

fn prepare_approval(store: &mut TaskStore, config: &RecoveryConfig) -> RecoveryTask {
    let mut current = store.save(task(), config, 100).unwrap();
    current.stage = RecoveryStage::Diagnosing;
    current = store.save(current, config, 110).unwrap();
    current.diagnosis_attempts = 1;
    current = store.save(current, config, 120).unwrap();
    store
        .save(approval_intent(current, config), config, 130)
        .unwrap()
}

#[test]
fn recovery_storage_replays_revisions_and_rejects_changed_trusted_configuration() {
    let dir = TestDir::new("recovery-storage-replay");
    let state = dir.path.join("state");
    let config = config();
    let mut store = TaskStore::open(&state, &config).unwrap();
    let mut next = store.save(task(), &config, 100).unwrap();
    next.stage = RecoveryStage::Diagnosing;
    let expected = store.save(next, &config, 200).unwrap();
    drop(store);
    let reopened = TaskStore::open(&state, &config).unwrap();
    assert_eq!(reopened.tasks["task-a"].revision, expected.revision);
    assert_eq!(reopened.tasks["task-a"].stage, RecoveryStage::Diagnosing);
    drop(reopened);
    let mut changed = config;
    changed.max_tool_calls += 1;
    assert!(matches!(
        TaskStore::open(&state, &changed),
        Err(RecoveryError::Corrupt(_))
    ));
}

#[test]
fn recovery_storage_holds_single_owner_lock_and_rejects_lock_contents() {
    let dir = TestDir::new("recovery-storage-lock");
    let state = dir.path.join("state");
    let config = config();
    let store = TaskStore::open(&state, &config).unwrap();
    assert!(TaskStore::open(&state, &config).is_err());
    drop(store);
    std::fs::write(state.join("recovery.lock"), "unexpected state").unwrap();
    assert!(matches!(
        TaskStore::open(&state, &config),
        Err(RecoveryError::Corrupt(_))
    ));
}

#[test]
fn unsupported_storage_layout_cannot_initialize_new_task_or_target_authority() {
    for name in ["pipeline.jsonl", "pipeline.lock"] {
        let dir = TestDir::new("recovery-storage-layout");
        let state = dir.path.join("state");
        std::fs::create_dir(&state).unwrap();
        let unsupported = state.join(name);
        let evidence = b"unrecognized stored execution evidence";
        std::fs::write(&unsupported, evidence).unwrap();
        assert!(matches!(
            TaskStore::open(&state, &config()),
            Err(RecoveryError::Invalid(_))
        ));
        let authority = FileTargetOwnership::open(dir.path.join("ownership")).unwrap();
        assert!(matches!(
            authority.acquire(&CanonicalTarget::new("target-a").unwrap(), &state),
            Err(RecoveryError::Invalid(_))
        ));
        assert_eq!(std::fs::read(&unsupported).unwrap(), evidence);
        assert!(!state.join("recovery.lock").exists());
        assert!(!state.join("recovery.jsonl").exists());
        assert!(!dir.path.join("ownership/target-target-a.jsonl").exists());
    }
}

#[test]
fn recovery_storage_incomplete_and_malformed_entries_are_never_truncated() {
    for suffix in [b"{\"format\":".as_slice(), b"invalid-json\n".as_slice()] {
        let dir = TestDir::new("recovery-storage-corrupt");
        let state = dir.path.join("state");
        let config = config();
        let mut store = TaskStore::open(&state, &config).unwrap();
        store.save(task(), &config, 100).unwrap();
        drop(store);
        let path = state.join("recovery.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(suffix)
            .unwrap();
        let length = std::fs::metadata(&path).unwrap().len();
        assert!(matches!(
            TaskStore::open(&state, &config),
            Err(RecoveryError::Corrupt(_))
        ));
        assert_eq!(std::fs::metadata(path).unwrap().len(), length);
    }
}

#[test]
fn recovery_storage_live_file_replacement_is_blocked_or_poisons_the_owner() {
    let dir = TestDir::new("recovery-storage-replacement");
    let state = dir.path.join("state");
    let config = config();
    let mut store = TaskStore::open(&state, &config).unwrap();
    let mut old = store.save(task(), &config, 100).unwrap();
    old.stage = RecoveryStage::Diagnosing;
    let journal = state.join("recovery.jsonl");
    let moved = state.join("replaced.jsonl");
    let result = std::fs::rename(&journal, &moved);
    #[cfg(windows)]
    {
        assert!(result.is_err(), "live journal must deny FILE_SHARE_DELETE");
        assert!(
            std::fs::rename(&state, dir.path.join("moved-state")).is_err(),
            "ancestor must remain pinned"
        );
        assert_eq!(store.save(old, &config, 200).unwrap().revision, 2);
    }
    #[cfg(not(windows))]
    {
        result.unwrap();
        std::fs::write(&journal, "").unwrap();
        assert!(store.save(old.clone(), &config, 200).is_err());
        assert_eq!(store.tasks["task-a"].revision, 1);
        assert!(store.save(old, &config, 200).is_err());
    }
}

#[test]
fn recovery_storage_truncation_is_blocked_or_detected_before_memory_commit() {
    let dir = TestDir::new("recovery-storage-truncation");
    let state = dir.path.join("state");
    let config = config();
    let mut store = TaskStore::open(&state, &config).unwrap();
    let mut old = store.save(task(), &config, 100).unwrap();
    old.stage = RecoveryStage::Diagnosing;
    let journal = state.join("recovery.jsonl");
    let original_length = std::fs::metadata(&journal).unwrap().len();
    let result = OpenOptions::new()
        .write(true)
        .open(&journal)
        .and_then(|file| file.set_len(0));
    if result.is_ok() {
        assert!(store.save(old.clone(), &config, 200).is_err());
        assert_eq!(store.tasks["task-a"].revision, 1);
        assert!(store.save(old, &config, 200).is_err());
    } else {
        assert_eq!(std::fs::metadata(&journal).unwrap().len(), original_length);
        assert_eq!(store.save(old, &config, 200).unwrap().revision, 2);
    }
}

#[test]
fn recovery_storage_hard_links_and_non_directory_state_paths_are_rejected() {
    let dir = TestDir::new("recovery-storage-paths");
    let state = dir.path.join("state");
    std::fs::create_dir(&state).unwrap();
    let original = dir.path.join("original.jsonl");
    std::fs::write(&original, "").unwrap();
    std::fs::hard_link(&original, state.join("recovery.jsonl")).unwrap();
    assert!(matches!(
        TaskStore::open(&state, &config()),
        Err(RecoveryError::Invalid(_))
    ));
    assert!(matches!(
        TaskStore::open(&original, &config()),
        Err(RecoveryError::Invalid(_))
    ));
}

#[test]
fn recovery_storage_rejects_live_stage_shape_and_corrupt_replayed_shape() {
    let dir = TestDir::new("recovery-storage-shape");
    let state = dir.path.join("state");
    let config = config();
    let mut store = TaskStore::open(&state, &config).unwrap();
    let original = store.save(task(), &config, 100).unwrap();
    let length = std::fs::metadata(state.join("recovery.jsonl"))
        .unwrap()
        .len();
    for stage in [
        RecoveryStage::Executing,
        RecoveryStage::Publishing,
        RecoveryStage::Completed,
    ] {
        let mut invalid = original.clone();
        invalid.stage = stage;
        assert!(store.save(invalid, &config, 200).is_err());
    }
    assert_eq!(store.tasks["task-a"].revision, 1);
    assert_eq!(
        std::fs::metadata(state.join("recovery.jsonl"))
            .unwrap()
            .len(),
        length
    );
    drop(store);
    let path = state.join("recovery.jsonl");
    let mut entry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    entry["task"]["stage"] = "completed".into();
    let mut bytes = serde_json::to_vec(&entry).unwrap();
    bytes.push(b'\n');
    std::fs::write(&path, bytes).unwrap();
    assert!(matches!(
        TaskStore::open(&state, &config),
        Err(RecoveryError::Corrupt(_))
    ));
}

#[test]
fn recovery_storage_completed_requires_positive_verification_and_cannot_execute_again() {
    let dir = TestDir::new("recovery-storage-completed");
    let state = dir.path.join("state");
    let config = config();
    let mut store = TaskStore::open(&state, &config).unwrap();
    let mut current = store.save(task(), &config, 100).unwrap();
    let script = ScriptArtifact {
        id: "script-a".into(),
        version: 1,
        language: "python".into(),
        platform: "portable".into(),
        source: "print('stored proposal only')".into(),
        preconditions: config.target.required_facts.clone(),
        generated_by_harness: "execution".into(),
        generated_in_session: "session-a".into(),
    };
    current.plan = Some(RepairPlan {
        summary: "bounded proposal".into(),
        script: script.clone(),
        reusable: true,
    });
    current.observation = Some(TargetObservation {
        target_id: "target-a".into(),
        facts: config.target.required_facts.clone(),
        evidence_refs: vec!["inspect:1".into()],
        observed_at_ms: 100,
    });
    current.approval_id = Some("approval-a".into());
    current.operation = Some(approval::ProposedOperation {
        task_id: current.id.clone(),
        task_revision: 1,
        operation_id: "operation-a".into(),
        target: "target-a".into(),
        action: json!({
            "kind":"execute_script","executor_id":config.target.executor_id,"script":script,
            "verification_profile":config.target.verification_profile,"required_facts":config.target.required_facts,
            "timeout_secs":config.target.action_timeout_secs,"incident_id":current.problem.incident_id,
            "incident_revision":current.problem.incident_revision
        }),
    });
    current.stage = RecoveryStage::AwaitingApproval;
    current = store.save(current, &config, 110).unwrap();
    current.stage = RecoveryStage::Executing;
    current = store.save(current, &config, 120).unwrap();
    current.receipt = Some(ScriptReceipt {
        operation_id: "operation-a".into(),
        target_id: "target-a".into(),
        outcome: ScriptOutcome::Executed,
        executor_stopped: true,
        evidence_refs: vec!["receipt:1".into()],
        summary: "executor stopped".into(),
    });
    current.stage = RecoveryStage::Verifying;
    current = store.save(current, &config, 130).unwrap();
    current.verification = Some(BusinessVerification {
        operation_id: "operation-a".into(),
        target_id: "target-a".into(),
        profile: "readiness".into(),
        healthy: Some(true),
        executor_stopped: true,
        evidence_refs: vec!["verification:1".into()],
        verified_at_ms: 135,
    });
    current.stage = RecoveryStage::Publishing;
    current = store.save(current, &config, 140).unwrap();
    let mut invalid = current.clone();
    invalid.stage = RecoveryStage::Completed;
    invalid.verification.as_mut().unwrap().healthy = Some(false);
    assert!(store.save(invalid, &config, 150).is_err());
    current.stage = RecoveryStage::Completed;
    current = store.save(current, &config, 150).unwrap();
    let completed_revision = current.revision;
    current.stage = RecoveryStage::Executing;
    assert!(store.save(current.clone(), &config, 160).is_err());
    assert_eq!(store.tasks["task-a"].revision, completed_revision);
    // A conflicting authoritative approval can still conservatively downgrade a terminal task.
    current.stage = RecoveryStage::Unknown;
    assert!(store.save(current, &config, 160).is_ok());
}

#[test]
fn recovery_storage_prepared_approval_replays_and_only_attaches_unchanged_intent() {
    let dir = TestDir::new("recovery-storage-approval-intent");
    let state = dir.path.join("state");
    let config = config();
    let mut store = TaskStore::open(&state, &config).unwrap();
    let prepared = prepare_approval(&mut store, &config);
    assert!(prepared.approval_id.is_none());
    drop(store);
    let mut store = TaskStore::open(&state, &config).unwrap();
    assert_eq!(
        serde_json::to_value(&store.tasks[&prepared.id]).unwrap(),
        serde_json::to_value(&prepared).unwrap()
    );
    let journal = state.join("recovery.jsonl");
    let before = std::fs::metadata(&journal).unwrap().len();
    for (case, invalid) in illegal_approval_associations(&prepared) {
        assert!(
            matches!(
                store.save(invalid, &config, 140),
                Err(RecoveryError::Invalid(_))
            ),
            "{case} must be rejected before committing"
        );
        assert_eq!(
            store.tasks[&prepared.id].revision, prepared.revision,
            "{case}"
        );
        assert_eq!(std::fs::metadata(&journal).unwrap().len(), before, "{case}");
    }
    let mut associated = prepared.clone();
    associated.approval_id = Some("approval-a".into());
    let attached = store.save(associated, &config, 140).unwrap();
    assert_eq!(attached.diagnosis_attempts, prepared.diagnosis_attempts);
    assert_eq!(attached.operation, prepared.operation);
    assert_eq!(
        serde_json::to_value(&attached.plan).unwrap(),
        serde_json::to_value(&prepared.plan).unwrap()
    );
    let before = std::fs::metadata(&journal).unwrap().len();
    for (case, invalid) in illegal_linked_approval_changes(&attached) {
        assert!(
            matches!(
                store.save(invalid, &config, 150),
                Err(RecoveryError::Invalid(_))
            ),
            "{case} must remain forbidden after association"
        );
        assert_eq!(
            store.tasks[&attached.id].revision, attached.revision,
            "{case}"
        );
        assert_eq!(std::fs::metadata(&journal).unwrap().len(), before, "{case}");
    }
    drop(store);
    let reopened = TaskStore::open(&state, &config).unwrap();
    assert_eq!(
        serde_json::to_value(&reopened.tasks[&attached.id]).unwrap(),
        serde_json::to_value(&attached).unwrap()
    );
}

#[test]
fn recovery_storage_replay_rejects_illegal_approval_association_events() {
    let dir = TestDir::new("recovery-storage-approval-replay");
    let state = dir.path.join("state");
    let config = config();
    let mut store = TaskStore::open(&state, &config).unwrap();
    let prepared = prepare_approval(&mut store, &config);
    drop(store);
    let journal = state.join("recovery.jsonl");
    let reject_replay = |current: &RecoveryTask, cases: Vec<(&str, RecoveryTask)>| {
        let committed = std::fs::read(&journal).unwrap();
        let last = committed
            .split(|byte| *byte == b'\n')
            .rev()
            .find(|line| !line.is_empty())
            .unwrap();
        let template: serde_json::Value = serde_json::from_slice(last).unwrap();
        for (case, mut invalid) in cases {
            invalid.revision = current.revision + 1;
            invalid.updated_at_ms = current.updated_at_ms + 10;
            let mut entry = template.clone();
            entry["sequence"] = (template["sequence"].as_u64().unwrap() + 1).into();
            entry["task"] = serde_json::to_value(invalid).unwrap();
            let mut bytes = committed.clone();
            bytes.extend(serde_json::to_vec(&entry).unwrap());
            bytes.push(b'\n');
            std::fs::write(&journal, &bytes).unwrap();
            assert!(
                matches!(
                    TaskStore::open(&state, &config),
                    Err(RecoveryError::Corrupt(_))
                ),
                "replay must reject {case}"
            );
            assert_eq!(std::fs::read(&journal).unwrap(), bytes, "{case}");
        }
        std::fs::write(&journal, committed).unwrap();
    };
    reject_replay(&prepared, illegal_approval_associations(&prepared));
    let mut store = TaskStore::open(&state, &config).unwrap();
    let mut associated = prepared;
    associated.approval_id = Some("approval-a".into());
    let attached = store.save(associated, &config, 140).unwrap();
    drop(store);
    reject_replay(&attached, illegal_linked_approval_changes(&attached));
}
