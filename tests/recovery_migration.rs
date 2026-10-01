use super::storage::TaskStore;
use super::*;
use crate::recovery::workflow_test_support::TestDir;
use serde_json::{Value, json};

fn config_value() -> Value {
    let policy = json!({
        "id":"policy", "version":1,
        "reviewer":{"mode":"harness", "harness_id":"reviewer"},
        "delegation":"scoped target repair", "allowed_targets":["target-a"],
        "allowed_action_kinds":["execute_script"], "ttl_secs":60
    });
    json!({
        "schema_version":1,"execution_harness":"execution",
        "target":{"target_id":"target-a","executor_id":"executor-a","platform":"portable",
            "allowed_languages":["python"],"diagnostic_queries":["snapshot"],
            "verification_profile":"readiness","required_facts":{"version":"1"},"action_timeout_secs":10},
        "approval":policy,"script_approval":policy,
        "diagnosis_timeout_secs":10,"review_timeout_secs":10,"max_tool_calls":4,"max_diagnoses":2,
        "minimum_script_occurrences":2,"max_tasks":10,"max_journal_bytes":1048576
    })
}

fn config() -> RecoveryConfig {
    serde_json::from_value(config_value()).unwrap()
}

fn task_value(id: &str, incident: &str) -> Value {
    json!({
        "id":id,"revision":1,
        "problem":{"incident_id":incident,"incident_revision":1,"target_id":"target-a",
            "fingerprint":"not-ready","summary":"condition active","occurrences":999,
            "keywords":["readiness"],"conditions":{"version":"1"},"evidence_refs":["incident:1"]},
        "stage":"queued","diagnosis_attempts":0,"plan":null,"knowledge_id":null,"reused_script":false,
        "approval_id":null,"operation":null,"observation":null,"receipt":null,"verification":null,
        "result_check":null,"note":null,"created_at_ms":100,"updated_at_ms":100
    })
}

fn entry(sequence: u64, task: Value) -> Value {
    json!({"format":1,"sequence":sequence,"config":config_value(),"task":task})
}

fn journal(entries: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut bytes, entry).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn snapshots(bytes: &[u8]) -> Vec<Value> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn advance(task: &Value, stage: &str) -> Value {
    let mut next = task.clone();
    next["revision"] = json!(task["revision"].as_u64().unwrap() + 1);
    next["updated_at_ms"] = json!(task["updated_at_ms"].as_u64().unwrap() + 1);
    next["stage"] = json!(stage);
    next
}

#[test]
fn legacy_migration_counts_distinct_episodes_and_preserves_samples_revisions_and_terminal_states() {
    let first = task_value("task-a", "incident-a");
    let second = task_value("task-b", "incident-b");
    let mut other_conditions = task_value("task-c", "incident-c");
    other_conditions["problem"]["conditions"] = json!({"version":"1", "scope":"other"});
    let source = journal(&[
        entry(1, first.clone()),
        entry(2, advance(&first, "canceled")),
        entry(3, second.clone()),
        entry(4, advance(&second, "canceled")),
        entry(5, other_conditions),
    ]);
    let migration = plan_recovery_journal_migration(&source, &config(), &config()).unwrap();
    assert_eq!(migration.source().entries, 5);
    assert_eq!(migration.source().tasks, 3);
    assert_eq!(migration.source().version_1_entries, 5);
    assert_eq!(migration.migrated().version_2_entries, 5);
    let output = snapshots(migration.journal());
    let original = snapshots(&source);
    let expected_counts = [1, 1, 2, 2, 1];
    for ((original, mut converted), episode) in
        original.into_iter().zip(output).zip(expected_counts)
    {
        assert_eq!(converted["format"], 2);
        assert_eq!(converted["sequence"], original["sequence"]);
        assert_eq!(converted["task"]["episode_count"], episode);
        converted["task"]
            .as_object_mut()
            .unwrap()
            .remove("episode_count");
        assert_eq!(converted["task"], original["task"]);
    }
    assert_eq!(
        validate_recovery_journal(migration.journal(), &config()).unwrap(),
        *migration.migrated()
    );
}

#[test]
fn explicit_limits_migration_revalidates_every_historical_snapshot() {
    let first = task_value("task-a", "incident-a");
    let mut diagnosis = advance(&first, "diagnosing");
    let mut attempted = advance(&diagnosis, "diagnosing");
    attempted["diagnosis_attempts"] = json!(1);
    diagnosis["problem"]["occurrences"] = json!(999);
    let source = journal(&[entry(1, first), entry(2, diagnosis), entry(3, attempted)]);
    let original = config();
    let mut replacement = original.clone();
    replacement.max_tasks = 1;
    replacement.max_diagnoses = 1;
    replacement.max_tool_calls = 1;
    replacement.max_journal_bytes = 16384;
    let migration = plan_recovery_journal_migration(&source, &original, &replacement).unwrap();
    assert!(migration.journal().len() <= replacement.max_journal_bytes as usize);
    assert_eq!(migration.migrated().maximum_diagnoses, 1);
    assert!(validate_recovery_journal(migration.journal(), &original).is_err());
    validate_recovery_journal(migration.journal(), &replacement).unwrap();
    let again =
        plan_recovery_journal_migration(migration.journal(), &replacement, &original).unwrap();
    assert_eq!(again.source().version_1_entries, 0);
    assert_eq!(again.source().version_2_entries, 3);
    assert_eq!(
        snapshots(again.journal())[2]["task"]["diagnosis_attempts"],
        1
    );
}

#[test]
fn migration_rejects_changes_to_authority_action_contract_and_reuse_threshold() {
    let source = journal(&[entry(1, task_value("task-a", "incident-a"))]);
    let original = config();
    let mut changes = Vec::new();
    let mut changed = original.clone();
    changed.target.executor_id = "replacement-executor".into();
    changes.push(changed);
    let mut changed = original.clone();
    changed.execution_harness = "replacement-harness".into();
    changes.push(changed);
    let mut changed = original.clone();
    changed.approval.version += 1;
    changes.push(changed);
    let mut changed = original.clone();
    changed.target.verification_profile = "replacement-verification".into();
    changes.push(changed);
    let mut changed = original.clone();
    changed.target.action_timeout_secs += 1;
    changes.push(changed);
    let mut changed = original.clone();
    changed.minimum_script_occurrences = 1;
    changes.push(changed);
    let mut changed = original.clone();
    changed.review_timeout_secs += 1;
    changes.push(changed);
    for replacement in changes {
        assert!(plan_recovery_journal_migration(&source, &original, &replacement).is_err());
    }
}

#[test]
fn migration_rejects_shrinking_limits_below_tasks_attempts_and_full_output_bytes() {
    let mut first = task_value("task-a", "incident-a");
    first["note"] = json!("stored context ".repeat(400));
    let second = task_value("task-b", "incident-b");
    let original = config();
    let mut replacement = original.clone();
    replacement.max_tasks = 1;
    assert!(
        plan_recovery_journal_migration(
            &journal(&[entry(1, first.clone()), entry(2, second)]),
            &original,
            &replacement
        )
        .is_err()
    );
    let diagnosis = advance(&first, "diagnosing");
    let mut attempt_1 = advance(&diagnosis, "diagnosing");
    attempt_1["diagnosis_attempts"] = json!(1);
    let mut attempt_2 = advance(&attempt_1, "diagnosing");
    attempt_2["diagnosis_attempts"] = json!(2);
    replacement = original.clone();
    replacement.max_diagnoses = 1;
    let source = journal(&[
        entry(1, first),
        entry(2, diagnosis),
        entry(3, attempt_1),
        entry(4, attempt_2),
    ]);
    assert!(plan_recovery_journal_migration(&source, &original, &replacement).is_err());
    replacement = original.clone();
    replacement.max_journal_bytes = 4096;
    assert!(plan_recovery_journal_migration(&source, &original, &replacement).is_err());
}

#[test]
fn complete_source_validation_rejects_corruption_and_unauthorized_config_changes() {
    let original = config();
    let first = entry(1, task_value("task-a", "incident-a"));
    let mut cases = Vec::new();
    let mut malformed = journal(std::slice::from_ref(&first));
    malformed.pop();
    cases.push(malformed);
    let mut unknown_format = first.clone();
    unknown_format["format"] = json!(3);
    cases.push(journal(&[unknown_format]));
    let mut bad_sequence = first.clone();
    bad_sequence["sequence"] = json!(2);
    cases.push(journal(&[bad_sequence]));
    let mut revision_gap = first.clone();
    revision_gap["task"]["revision"] = json!(2);
    cases.push(journal(&[revision_gap]));
    let mut invalid_shape = first.clone();
    invalid_shape["task"]["stage"] = json!("executing");
    cases.push(journal(&[invalid_shape]));
    let mut changed_config = first.clone();
    changed_config["config"]["max_tasks"] = json!(9);
    cases.push(journal(&[changed_config]));
    let mut unknown_field = first.clone();
    unknown_field["task"]["made_up_fact"] = json!(true);
    cases.push(journal(&[unknown_field]));
    let second = entry(2, advance(&first["task"], "completed"));
    cases.push(journal(&[first.clone(), second]));
    let mut duplicated_incident = task_value("task-b", "incident-a");
    duplicated_incident["problem"]["occurrences"] = json!(1000);
    cases.push(journal(&[first, entry(2, duplicated_incident)]));
    for source in cases {
        assert!(validate_recovery_journal(&source, &original).is_err());
        assert!(plan_recovery_journal_migration(&source, &original, &original).is_err());
    }
}

#[test]
fn migration_never_accepts_supplied_episode_counts_that_exceed_trusted_history() {
    for format in [1, 2] {
        let mut first = entry(1, task_value("task-a", "incident-a"));
        first["format"] = json!(format);
        first["task"]["episode_count"] = json!(999);
        let source = journal(&[first]);
        assert!(validate_recovery_journal(&source, &config()).is_err());
        assert!(plan_recovery_journal_migration(&source, &config(), &config()).is_err());
    }
    let mut missing_count = entry(1, task_value("task-a", "incident-a"));
    missing_count["format"] = json!(2);
    assert!(validate_recovery_journal(&journal(&[missing_count]), &config()).is_err());
}

fn planned_task(current: &Value) -> Value {
    let mut planned = advance(current, "awaiting_approval");
    let script = json!({
        "id":"script-a","version":1,"language":"python","platform":"portable",
        "source":"print('bounded proposal')","preconditions":{"version":"1"},
        "generated_by_harness":"execution","generated_in_session":"session-a"
    });
    planned["plan"] = json!({"summary":"bounded proposal", "script":script, "reusable":false});
    planned["observation"] = json!({
        "target_id":"target-a","facts":{"version":"1"},"evidence_refs":["inspect:1"],
        "observed_at_ms":planned["updated_at_ms"]
    });
    planned["approval_id"] = json!("approval-original");
    planned["operation"] = json!({
        "task_id":planned["id"],"task_revision":current["revision"],"operation_id":"operation-original",
        "target":"target-a","action":{"kind":"execute_script","executor_id":"executor-a",
            "script":script,"verification_profile":"readiness","required_facts":{"version":"1"},
            "timeout_secs":10,"incident_id":planned["problem"]["incident_id"],"incident_revision":1}
    });
    planned
}

#[test]
fn migration_preserves_exact_approval_operation_execution_and_unknown_result_check_facts() {
    let queued = task_value("task-a", "incident-a");
    let diagnosing = advance(&queued, "diagnosing");
    let approved = planned_task(&diagnosing);
    let executing = advance(&approved, "executing");
    let mut unknown = advance(&executing, "unknown");
    let mut reconciled = advance(&unknown, "unknown");
    reconciled["verification"] = json!({
        "operation_id":"operation-original","target_id":"target-a","profile":"readiness",
        "healthy":null,"executor_stopped":false,"evidence_refs":["verify:uncertain"],
        "verified_at_ms":reconciled["updated_at_ms"]
    });
    reconciled["result_check"] = json!({
        "execution":{"operation_id":"operation-original","target_id":"target-a","executor_id":"executor-a",
            "outcome":"unknown","executor_stopped":false,"evidence_refs":["executor:uncertain"],
            "checked_at_ms":reconciled["updated_at_ms"]},"actor":"trusted-reviewer"
    });
    let source = journal(&[
        entry(1, queued),
        entry(2, diagnosing),
        entry(3, approved),
        entry(4, executing),
        entry(5, unknown.clone()),
        entry(6, reconciled.clone()),
    ]);
    let migrated = plan_recovery_journal_migration(&source, &config(), &config()).unwrap();
    let last = snapshots(migrated.journal()).pop().unwrap();
    let mut restored = last["task"].clone();
    restored.as_object_mut().unwrap().remove("episode_count");
    assert_eq!(restored, reconciled);
    assert_eq!(restored["stage"], "unknown");
    assert_eq!(restored["plan"]["reusable"], false);
    unknown["plan"]["reusable"] = json!(true);
    let invalid = journal(&[
        entry(1, task_value("task-a", "incident-a")),
        entry(
            2,
            advance(&task_value("task-a", "incident-a"), "diagnosing"),
        ),
        entry(
            3,
            planned_task(&advance(&task_value("task-a", "incident-a"), "diagnosing")),
        ),
        entry(
            4,
            advance(
                &planned_task(&advance(&task_value("task-a", "incident-a"), "diagnosing")),
                "executing",
            ),
        ),
        entry(5, unknown),
    ]);
    assert!(plan_recovery_journal_migration(&invalid, &config(), &config()).is_err());
}

#[test]
fn migration_preserves_completed_history_and_does_not_fill_legacy_orphan_plans() {
    let queued = task_value("task-a", "incident-a");
    let diagnosing = advance(&queued, "diagnosing");
    let planned = planned_task(&diagnosing);
    let executing = advance(&planned, "executing");
    let mut verifying = advance(&executing, "verifying");
    verifying["receipt"] = json!({
        "operation_id":"operation-original","target_id":"target-a","outcome":"executed",
        "executor_stopped":true,"evidence_refs":["executor:stopped"],"summary":"external action complete"
    });
    let mut publishing = advance(&verifying, "publishing");
    publishing["verification"] = json!({
        "operation_id":"operation-original","target_id":"target-a","profile":"readiness",
        "healthy":true,"executor_stopped":true,"evidence_refs":["verify:healthy"],
        "verified_at_ms":publishing["updated_at_ms"]
    });
    let completed = advance(&publishing, "completed");
    let source = journal(&[
        entry(1, queued.clone()),
        entry(2, diagnosing.clone()),
        entry(3, planned),
        entry(4, executing),
        entry(5, verifying),
        entry(6, publishing),
        entry(7, completed.clone()),
    ]);
    let migrated = plan_recovery_journal_migration(&source, &config(), &config()).unwrap();
    let last = snapshots(migrated.journal()).pop().unwrap();
    let mut restored = last["task"].clone();
    restored.as_object_mut().unwrap().remove("episode_count");
    assert_eq!(restored, completed);
    let legacy_unlinked = journal(&[entry(1, queued), entry(2, diagnosing)]);
    let untouched =
        plan_recovery_journal_migration(&legacy_unlinked, &config(), &config()).unwrap();
    let last = snapshots(untouched.journal()).pop().unwrap();
    assert_eq!(last["task"]["stage"], "diagnosing");
    assert!(last["task"]["plan"].is_null());
    assert!(last["task"]["approval_id"].is_null());
}

#[test]
fn validation_bounds_whole_input_entries_and_format_progression() {
    let mut tiny = config();
    tiny.max_journal_bytes = 4096;
    assert!(matches!(
        validate_recovery_journal(&vec![b' '; 4097], &tiny),
        Err(RecoveryError::Capacity)
    ));
    let mut huge_entry = entry(1, task_value("task-a", "incident-a"));
    huge_entry["task"]["note"] = json!("x".repeat(256 * 1024));
    assert!(validate_recovery_journal(&journal(&[huge_entry]), &config()).is_err());
    let mut first = entry(1, task_value("task-a", "incident-a"));
    first["format"] = json!(2);
    first["task"]["episode_count"] = json!(1);
    let second = entry(2, advance(&first["task"], "canceled"));
    assert!(validate_recovery_journal(&journal(&[first, second]), &config()).is_err());
}

#[test]
fn live_legacy_replay_uses_trusted_counts_and_appends_only_version_2() {
    let dir = TestDir::new("legacy-episode-replay");
    let first = task_value("task-a", "incident-a");
    let second = task_value("task-b", "incident-b");
    let source = journal(&[
        entry(1, first.clone()),
        entry(2, advance(&first, "canceled")),
        entry(3, second),
    ]);
    let path = dir.path.join("recovery.jsonl");
    std::fs::write(&path, source).unwrap();
    let mut store = TaskStore::open(&dir.path, &config()).unwrap();
    assert_eq!(store.tasks["task-a"].episode_count, 1);
    assert_eq!(store.tasks["task-b"].episode_count, 2);
    let mut current = store.tasks["task-b"].clone();
    current.stage = RecoveryStage::Canceled;
    store.save(current, &config(), 102).unwrap();
    drop(store);
    let output = std::fs::read(path).unwrap();
    let last = snapshots(&output).pop().unwrap();
    assert_eq!(last["format"], 2);
    assert_eq!(last["task"]["episode_count"], 2);
    let reopened = TaskStore::open(&dir.path, &config()).unwrap();
    assert_eq!(reopened.tasks["task-b"].episode_count, 2);
    assert_eq!(reopened.tasks["task-b"].stage, RecoveryStage::Canceled);
    validate_recovery_journal(&output, &config()).unwrap();
}

#[test]
fn empty_history_can_be_planned_without_creating_facts() {
    let migration = plan_recovery_journal_migration(&[], &config(), &config()).unwrap();
    assert_eq!(migration.source().entries, 0);
    assert_eq!(migration.migrated().tasks, 0);
    assert!(migration.into_journal().is_empty());
}
