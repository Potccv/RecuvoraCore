use recuvora_core::recovery::knowledge::*;
use std::{collections::BTreeMap, fs::OpenOptions, io::Write, path::Path};

#[path = "workflow_support.rs"]
mod support;
use support::TestDir;

#[path = "knowledge_maintenance.rs"]
mod maintenance;

fn candidate(id: &str, version: u64) -> KnowledgeCandidate {
    KnowledgeCandidate {
        id: id.into(),
        incident_id: "incident-a".into(),
        summary: "target condition differs".into(),
        keywords: vec!["condition".into(), "workload".into()],
        conditions: BTreeMap::from([("condition".into(), "unready".into())]),
        script: ScriptArtifact {
            id: "repair-script".into(),
            version,
            language: "python".into(),
            platform: "portable".into(),
            source: "print('external action proposal')\n".into(),
            preconditions: BTreeMap::from([("workload_version".into(), "1".into())]),
            generated_by_harness: "harness-execution".into(),
            generated_in_session: "session-a".into(),
        },
        reusable: false,
        evidence_refs: vec!["observation:1".into()],
        created_at_ms: 100,
    }
}

fn case(id: &str, outcome: RepairOutcome, version: u64) -> RepairCase {
    RepairCase {
        id: id.into(),
        operation_id: format!("operation-{id}"),
        target_id: "target-a".into(),
        script_id: "repair-script".into(),
        script_version: version,
        outcome,
        evidence_refs: vec![format!("operation:{id}")],
        recorded_at_ms: 300,
    }
}

fn proof(case: &RepairCase) -> TrustedBusinessVerification {
    TrustedBusinessVerification::attest(
        &case.operation_id,
        &case.target_id,
        &case.script_id,
        case.script_version,
        "trusted-target-verifier",
        vec![format!("verification:{}", case.id)],
        200,
    )
    .unwrap()
}

fn query() -> KnowledgeQuery {
    KnowledgeQuery {
        conditions: BTreeMap::from([
            ("condition".into(), "unready".into()),
            ("workload_version".into(), "1".into()),
        ]),
        keywords: vec!["condition".into()],
        limit: 10,
    }
}

fn open(path: &Path) -> KnowledgeStore {
    KnowledgeStore::open(path, KnowledgeStoreConfig::default()).unwrap()
}

fn verify(
    store: &mut KnowledgeStore,
    record_id: &str,
    case_id: &str,
    version: u64,
) -> KnowledgeRecord {
    let result = case(case_id, RepairOutcome::Verified, version);
    let verification = proof(&result);
    store
        .record_outcome(record_id, result, Some(verification))
        .unwrap()
}

#[test]
fn candidate_is_not_reusable_and_verified_case_survives_restart() {
    let dir = TestDir::new("knowledge-verified");
    let path = dir.path.join("nested/knowledge.jsonl");
    let mut store = open(&path);
    assert_eq!(
        store
            .upsert_candidate(candidate("record-a", 1))
            .unwrap()
            .status,
        KnowledgeStatus::Candidate
    );
    assert!(store.search(&query()).unwrap().is_empty());
    assert!(matches!(
        store.record_outcome("record-a", case("case-a", RepairOutcome::Verified, 1), None),
        Err(KnowledgeError::Invalid(_))
    ));
    assert_eq!(store.get("record-a").unwrap().revision, 1);
    let expected = verify(&mut store, "record-a", "case-a", 1);
    assert_eq!(expected.status, KnowledgeStatus::Verified);
    assert_eq!(
        expected.cases[0].verification.as_ref().unwrap().verifier_id,
        "trusted-target-verifier"
    );
    drop(store);
    let reopened = open(&path);
    assert_eq!(reopened.get("record-a").unwrap(), expected);
    assert_eq!(reopened.search(&query()).unwrap(), vec![expected]);
}

#[test]
fn immutable_version_and_candidate_identity_prevent_content_substitution() {
    let dir = TestDir::new("knowledge-immutable");
    let mut store = open(&dir.path.join("knowledge.jsonl"));
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    let mut changed = candidate("record-a", 1);
    changed.summary = "different explanation".into();
    assert!(matches!(
        store.upsert_candidate(changed),
        Err(KnowledgeError::Conflict(_))
    ));
    for field in ["source", "platform", "origin", "precondition"] {
        let mut changed = candidate("record-b", 1);
        match field {
            "source" => changed.script.source = "print('changed proposal')".into(),
            "platform" => changed.script.platform = "other".into(),
            "origin" => changed.script.generated_by_harness = "another-harness".into(),
            _ => {
                changed
                    .script
                    .preconditions
                    .insert("workload_version".into(), "2".into());
            }
        }
        assert!(matches!(
            store.validate_script(&changed.script),
            Err(KnowledgeError::Conflict(_))
        ));
        assert!(matches!(
            store.upsert_candidate(changed),
            Err(KnowledgeError::Conflict(_))
        ));
    }
    let mut new_version = candidate("record-b", 2);
    new_version.script.source = "print('new proposal version')".into();
    store.upsert_candidate(new_version).unwrap();
    assert!(store.search(&query()).unwrap().is_empty());
}

#[test]
fn candidate_and_case_retries_are_idempotent_even_after_later_failure_and_restart() {
    let dir = TestDir::new("knowledge-retry");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = open(&path);
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    verify(&mut store, "record-a", "case-a", 1);
    store
        .record_outcome("record-a", case("case-b", RepairOutcome::Failed, 1), None)
        .unwrap();
    let expected = store.get("record-a").unwrap();
    let length = std::fs::metadata(&path).unwrap().len();
    drop(store);
    let mut store = open(&path);
    assert_eq!(
        store.upsert_candidate(candidate("record-a", 1)).unwrap(),
        expected
    );
    assert_eq!(verify(&mut store, "record-a", "case-a", 1), expected);
    assert_eq!(
        store
            .record_outcome("record-a", case("case-b", RepairOutcome::Failed, 1), None)
            .unwrap(),
        expected
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
    let mut changed = case("case-b", RepairOutcome::Failed, 1);
    changed.target_id = "other-target".into();
    assert!(matches!(
        store.record_outcome("record-a", changed, None),
        Err(KnowledgeError::Conflict(_))
    ));
}

#[test]
fn failed_and_unknown_versions_are_quarantined_across_records() {
    for outcome in [RepairOutcome::Failed, RepairOutcome::Unknown] {
        let dir = TestDir::new("knowledge-quarantine");
        let path = dir.path.join("knowledge.jsonl");
        let mut store = open(&path);
        store.upsert_candidate(candidate("record-a", 1)).unwrap();
        store.upsert_candidate(candidate("record-b", 1)).unwrap();
        verify(&mut store, "record-a", "case-a", 1);
        verify(&mut store, "record-b", "case-b", 1);
        store
            .record_outcome("record-a", case("case-c", outcome, 1), None)
            .unwrap();
        assert!(store.search(&query()).unwrap().is_empty());
        drop(store);
        let mut store = open(&path);
        assert!(store.search(&query()).unwrap().is_empty());
        store.upsert_candidate(candidate("record-c", 2)).unwrap();
        verify(&mut store, "record-c", "case-d", 2);
        assert_eq!(store.search(&query()).unwrap()[0].id, "record-c");
    }
}

#[test]
fn verification_cannot_be_reused_for_another_operation_target_or_script() {
    let dir = TestDir::new("knowledge-proof");
    let mut store = open(&dir.path.join("knowledge.jsonl"));
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    let result = case("case-a", RepairOutcome::Verified, 1);
    for mismatch in ["operation", "target", "script", "version", "time"] {
        let verification = TrustedBusinessVerification::attest(
            if mismatch == "operation" {
                "other-op"
            } else {
                &result.operation_id
            },
            if mismatch == "target" {
                "other-target"
            } else {
                &result.target_id
            },
            if mismatch == "script" {
                "other-script"
            } else {
                &result.script_id
            },
            if mismatch == "version" { 2 } else { 1 },
            "trusted-verifier",
            vec!["verification:1".into()],
            if mismatch == "time" { 400 } else { 200 },
        )
        .unwrap();
        assert!(matches!(
            store.record_outcome("record-a", result.clone(), Some(verification)),
            Err(KnowledgeError::Conflict(_))
        ));
    }
    assert!(
        TrustedBusinessVerification::attest(
            "op",
            "target",
            "script",
            1,
            "verifier",
            Vec::new(),
            200
        )
        .is_err()
    );
    assert_eq!(
        store.get("record-a").unwrap().status,
        KnowledgeStatus::Candidate
    );
}

#[test]
fn retrieval_requires_all_exact_conditions_and_keywords() {
    let dir = TestDir::new("knowledge-match");
    let mut store = open(&dir.path.join("knowledge.jsonl"));
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    verify(&mut store, "record-a", "case-a", 1);
    let mut q = query();
    q.conditions.remove("workload_version");
    assert!(store.search(&q).unwrap().is_empty());
    let mut q = query();
    q.conditions.insert("condition".into(), "ready".into());
    assert!(store.search(&q).unwrap().is_empty());
    let mut q = query();
    q.keywords.push("unlisted".into());
    assert!(store.search(&q).unwrap().is_empty());
    let mut q = query();
    q.keywords = vec!["Condition".into()];
    assert!(store.search(&q).unwrap().is_empty());
    let mut q = query();
    q.conditions.clear();
    assert!(store.search(&q).is_err());
    assert_eq!(store.search(&query()).unwrap().len(), 1);
}

#[test]
fn disabling_requires_revision_and_does_not_reset_on_retry() {
    let dir = TestDir::new("knowledge-disabled");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = open(&path);
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    verify(&mut store, "record-a", "case-a", 1);
    assert!(matches!(
        store.disable("record-a", 1, "operator", "retired"),
        Err(KnowledgeError::Conflict(_))
    ));
    let disabled = store.disable("record-a", 2, "operator", "retired").unwrap();
    let length = std::fs::metadata(&path).unwrap().len();
    assert_eq!(disabled.status, KnowledgeStatus::Disabled);
    assert_eq!(
        store.disable("record-a", 2, "operator", "retired").unwrap(),
        disabled
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
    assert!(store.search(&query()).unwrap().is_empty());
    assert!(matches!(
        store.record_outcome("record-a", case("case-b", RepairOutcome::Failed, 1), None),
        Err(KnowledgeError::Conflict(_))
    ));
}

#[test]
fn exclusive_lock_close_and_snapshot_reads_have_separate_lifetimes() {
    let dir = TestDir::new("knowledge-lock");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = open(&path);
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    assert!(matches!(
        KnowledgeStore::open(&path, KnowledgeStoreConfig::default()),
        Err(KnowledgeError::Unavailable(_))
    ));
    store.close().unwrap();
    assert!(store.get("record-a").is_some());
    assert!(matches!(
        store.upsert_candidate(candidate("record-b", 2)),
        Err(KnowledgeError::Unavailable(_))
    ));
    let reopened = open(&path);
    assert!(reopened.get("record-a").is_some());
}

#[test]
fn partial_malformed_and_duplicate_journal_entries_fail_closed() {
    for corruption in ["partial", "malformed", "duplicate", "sequence"] {
        let dir = TestDir::new("knowledge-corrupt");
        let path = dir.path.join("knowledge.jsonl");
        let mut store = open(&path);
        store.upsert_candidate(candidate("record-a", 1)).unwrap();
        drop(store);
        let original = std::fs::read(&path).unwrap();
        let extra = match corruption {
            "partial" => b"{\"format\":".to_vec(),
            "malformed" => b"not-json\n".to_vec(),
            "duplicate" => {
                let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                value["sequence"] = 2.into();
                let mut bytes = serde_json::to_vec(&value).unwrap();
                bytes.push(b'\n');
                bytes
            }
            _ => original.clone(),
        };
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&extra)
            .unwrap();
        assert!(matches!(
            KnowledgeStore::open(&path, KnowledgeStoreConfig::default()),
            Err(KnowledgeError::Corrupt(_))
        ));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            (original.len() + extra.len()) as u64
        );
    }
}

#[test]
fn capacity_and_script_byte_limits_do_not_publish_partial_records() {
    let dir = TestDir::new("knowledge-capacity");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = KnowledgeStore::open(
        &path,
        KnowledgeStoreConfig {
            max_records: 1,
            max_cases_per_record: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let mut huge = candidate("record-a", 1);
    huge.script.source = "x".repeat(MAX_SCRIPT_BYTES + 1);
    assert!(matches!(
        store.upsert_candidate(huge),
        Err(KnowledgeError::Invalid(_))
    ));
    assert!(store.get("record-a").is_none());
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    assert!(matches!(
        store.upsert_candidate(candidate("record-b", 2)),
        Err(KnowledgeError::Capacity(_))
    ));
    verify(&mut store, "record-a", "case-a", 1);
    let length = std::fs::metadata(&path).unwrap().len();
    assert!(matches!(
        store.record_outcome("record-a", case("case-b", RepairOutcome::Failed, 1), None),
        Err(KnowledgeError::Capacity(_))
    ));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
    assert_eq!(store.get("record-a").unwrap().revision, 2);
    let tiny_path = dir.path.join("tiny.jsonl");
    let mut tiny = KnowledgeStore::open(
        &tiny_path,
        KnowledgeStoreConfig {
            max_journal_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        tiny.upsert_candidate(candidate("record-a", 1)),
        Err(KnowledgeError::Capacity(_))
    ));
    assert!(tiny.get("record-a").is_none());
}

#[test]
fn external_mutation_is_blocked_or_poisoned_without_false_success() {
    let dir = TestDir::new("knowledge-external");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = open(&path);
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    let append = OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"external\n");
    #[cfg(windows)]
    if let Err(error) = append {
        // Windows enforces this byte-range lock against the second writer.
        assert_eq!(error.raw_os_error(), Some(33));
        assert!(store.get("record-b").is_none());
        store.upsert_candidate(candidate("record-b", 2)).unwrap();
        return;
    }
    #[cfg(not(windows))]
    append.unwrap();
    assert!(matches!(
        store.upsert_candidate(candidate("record-b", 2)),
        Err(KnowledgeError::Unavailable(_))
    ));
    assert!(store.get("record-b").is_none());
    assert!(matches!(
        store.upsert_candidate(candidate("record-c", 3)),
        Err(KnowledgeError::Unavailable(_))
    ));
    store.close().unwrap();
    assert!(matches!(
        store.export_compacted(),
        Err(KnowledgeError::Unavailable(_))
    ));
}

#[test]
fn model_candidate_json_cannot_supply_verified_status() {
    let mut value = serde_json::to_value(candidate("record-a", 1)).unwrap();
    value["status"] = "verified".into();
    assert!(serde_json::from_value::<KnowledgeCandidate>(value).is_err());
}

#[test]
fn source_directory_and_hard_link_paths_are_rejected() {
    assert!(matches!(
        KnowledgeStore::open(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("forbidden-knowledge.jsonl"),
            KnowledgeStoreConfig::default()
        ),
        Err(KnowledgeError::Invalid(_))
    ));
    let dir = TestDir::new("knowledge-links");
    let original = dir.path.join("original.jsonl");
    let link = dir.path.join("linked.jsonl");
    std::fs::write(&original, "").unwrap();
    std::fs::hard_link(&original, &link).unwrap();
    assert!(matches!(
        KnowledgeStore::open(link, KnowledgeStoreConfig::default()),
        Err(KnowledgeError::Invalid(_))
    ));
}

#[test]
fn ordinary_cases_remain_searchable_while_script_reuse_filters_before_limit() {
    let dir = TestDir::new("knowledge-reuse-eligibility");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = open(&path);
    store.upsert_candidate(candidate("a-ordinary", 1)).unwrap();
    verify(&mut store, "a-ordinary", "ordinary-success", 1);
    let mut reusable = candidate("b-reusable", 1);
    reusable.reusable = true;
    store.upsert_candidate(reusable).unwrap();
    verify(&mut store, "b-reusable", "reusable-success", 1);
    let mut q = query();
    q.limit = 1;
    assert_eq!(store.search(&q).unwrap()[0].id, "a-ordinary");
    assert_eq!(store.search_reusable(&q).unwrap()[0].id, "b-reusable");
    drop(store);
    let mut reopened = open(&path);
    assert!(!reopened.get("a-ordinary").unwrap().candidate.reusable);
    assert_eq!(reopened.search_reusable(&q).unwrap()[0].id, "b-reusable");
    // Failure in an ordinary case still quarantines this version for every reuse case.
    reopened
        .record_outcome(
            "a-ordinary",
            case("ordinary-failure", RepairOutcome::Failed, 1),
            None,
        )
        .unwrap();
    assert!(reopened.search_reusable(&q).unwrap().is_empty());
    assert!(reopened.search(&q).unwrap().is_empty());
}

#[test]
fn legacy_candidate_defaults_to_ordinary_case_without_reuse_eligibility() {
    let mut value = serde_json::to_value(candidate("legacy", 1)).unwrap();
    value.as_object_mut().unwrap().remove("reusable");
    let decoded: KnowledgeCandidate = serde_json::from_value(value).unwrap();
    assert!(!decoded.reusable);
}
