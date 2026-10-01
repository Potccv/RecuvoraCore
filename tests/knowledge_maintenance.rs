use super::*;

fn proposal(id: &str, version: u64) -> KnowledgeProposal {
    KnowledgeProposal {
        source_id: "read-only-source".into(),
        candidate: candidate(id, version),
        evidence_refs: vec!["knowledge-source:retrieval-evidence".into()],
    }
}

fn verified_at(
    store: &mut KnowledgeStore,
    record_id: &str,
    case_id: &str,
    version: u64,
    time: u64,
) {
    let mut result = case(case_id, RepairOutcome::Verified, version);
    result.recorded_at_ms = time + 10;
    let attestation = TrustedBusinessVerification::attest(
        &result.operation_id,
        &result.target_id,
        &result.script_id,
        version,
        "trusted-target-verifier",
        vec![format!("verification:{case_id}")],
        time,
    )
    .unwrap();
    store
        .record_outcome(record_id, result, Some(attestation))
        .unwrap();
}

#[test]
fn external_candidates_are_bounded_read_only_evidence_with_no_reuse_authority() {
    let dir = TestDir::new("knowledge-external-proposals");
    let mut store = open(&dir.path.join("knowledge.jsonl"));
    let mut external = proposal("external-a", 1);
    external.candidate.reusable = true;
    let accepted = store
        .validate_external_candidates("read-only-source", &query(), vec![external.clone()])
        .unwrap();
    assert_eq!(accepted.len(), 1);
    assert!(!accepted[0].reusable);
    assert!(
        accepted[0]
            .evidence_refs
            .contains(&"knowledge-source:read-only-source".into())
    );
    assert!(
        accepted[0]
            .evidence_refs
            .contains(&external.evidence_refs[0])
    );
    assert_eq!(store.projection().records, 0);
    assert!(store.search_reusable(&query()).unwrap().is_empty());

    let mut forged = serde_json::to_value(&external).unwrap();
    forged["status"] = "verified".into();
    assert!(serde_json::from_value::<KnowledgeProposal>(forged).is_err());
    let mut forged = serde_json::to_value(&external).unwrap();
    forged["candidate"]["status"] = "verified".into();
    assert!(serde_json::from_value::<KnowledgeProposal>(forged).is_err());
    assert!(matches!(
        store.validate_external_candidates("another-source", &query(), vec![external.clone()]),
        Err(KnowledgeError::Conflict(_))
    ));
    assert!(matches!(
        store.validate_external_candidates(
            "read-only-source",
            &query(),
            vec![external.clone(), external.clone()]
        ),
        Err(KnowledgeError::Conflict(_))
    ));
    let mut small_query = query();
    small_query.limit = 1;
    assert!(matches!(
        store.validate_external_candidates(
            "read-only-source",
            &small_query,
            vec![external.clone(), proposal("external-b", 2)]
        ),
        Err(KnowledgeError::Capacity(_))
    ));
    let mut malformed = external.clone();
    malformed.evidence_refs.clear();
    assert!(matches!(
        store.validate_external_candidates("read-only-source", &query(), vec![malformed]),
        Err(KnowledgeError::Invalid(_))
    ));
    let mut changed = proposal("external-b", 1);
    changed.candidate.script.source = "print('substituted proposal')".into();
    assert!(matches!(
        store.validate_external_candidates(
            "read-only-source",
            &query(),
            vec![external.clone(), changed]
        ),
        Err(KnowledgeError::Conflict(_))
    ));
    let mut inapplicable = external.clone();
    inapplicable
        .candidate
        .conditions
        .insert("condition".into(), "other".into());
    assert!(
        store
            .validate_external_candidates("read-only-source", &query(), vec![inapplicable])
            .unwrap()
            .is_empty()
    );
    store.upsert_candidate(candidate("local-a", 1)).unwrap();
    store
        .record_outcome(
            "local-a",
            case("local-unknown", RepairOutcome::Unknown, 1),
            None,
        )
        .unwrap();
    assert!(
        store
            .validate_external_candidates("read-only-source", &query(), vec![external.clone()])
            .unwrap()
            .is_empty()
    );
    let mut changed = external;
    changed.candidate.script.source = "print('changed known version')".into();
    assert!(matches!(
        store.validate_external_candidates("read-only-source", &query(), vec![changed]),
        Err(KnowledgeError::Conflict(_))
    ));
    assert_eq!(store.projection().records, 1);
}

#[test]
fn external_batch_byte_limit_is_checked_after_bounded_field_validation() {
    let dir = TestDir::new("knowledge-external-byte-limit");
    let store = open(&dir.path.join("knowledge.jsonl"));
    let mut q = query();
    q.limit = 100;
    let proposals = (0..100)
        .map(|index| {
            let mut value = proposal(&format!("external-{index:03}"), index + 1);
            value.candidate.script.source = "x".repeat(MAX_SCRIPT_BYTES);
            value
                .candidate
                .conditions
                .insert("condition".into(), "outside-query".into());
            value.candidate.evidence_refs = (0..32)
                .map(|reference| format!("candidate-{reference:02}:{}", "x".repeat(1000)))
                .collect();
            value.evidence_refs = (0..32)
                .map(|reference| format!("source-{reference:02}:{}", "x".repeat(1000)))
                .collect();
            value
        })
        .collect();
    assert!(matches!(
        store.validate_external_candidates("read-only-source", &q, proposals),
        Err(KnowledgeError::Capacity(_))
    ));
    let mut oversized = proposal("oversized", 1);
    oversized.candidate.script.source = "x".repeat(MAX_SCRIPT_BYTES + 1);
    assert!(matches!(
        store.validate_external_candidates("read-only-source", &q, vec![oversized]),
        Err(KnowledgeError::Invalid(_))
    ));
    assert_eq!(store.projection().journal_bytes, 0);
}

#[test]
fn verified_retrieval_ranks_trusted_successes_recency_exact_conditions_then_id() {
    let dir = TestDir::new("knowledge-ranked-retrieval");
    let mut store = open(&dir.path.join("knowledge.jsonl"));
    for (id, version) in [
        ("a-old-one", 1),
        ("b-new-one", 2),
        ("c-two-successes", 3),
        ("d-specific", 4),
        ("e-specific", 5),
    ] {
        let mut value = candidate(id, version);
        value.reusable = true;
        if version >= 4 {
            value.conditions.insert("scope".into(), "sample".into());
        }
        store.upsert_candidate(value).unwrap();
    }
    verified_at(&mut store, "a-old-one", "old-success", 1, 200);
    verified_at(&mut store, "b-new-one", "new-success", 2, 400);
    verified_at(&mut store, "c-two-successes", "repeated-success-a", 3, 200);
    verified_at(&mut store, "c-two-successes", "repeated-success-b", 3, 220);
    verified_at(&mut store, "d-specific", "specific-success-a", 4, 400);
    verified_at(&mut store, "e-specific", "specific-success-b", 5, 400);
    let mut q = query();
    q.conditions.insert("scope".into(), "sample".into());
    assert_eq!(
        store
            .search(&q)
            .unwrap()
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        [
            "c-two-successes",
            "d-specific",
            "e-specific",
            "b-new-one",
            "a-old-one"
        ]
    );
    q.limit = 1;
    assert_eq!(store.search_reusable(&q).unwrap()[0].id, "c-two-successes");
    store
        .record_outcome(
            "c-two-successes",
            case("repeated-failure", RepairOutcome::Failed, 3),
            None,
        )
        .unwrap();
    assert_eq!(store.search_reusable(&q).unwrap()[0].id, "d-specific");
}

#[test]
fn operations_projection_exposes_capacity_and_isolation_without_script_text() {
    let dir = TestDir::new("knowledge-operational-projection");
    let mut store = open(&dir.path.join("knowledge.jsonl"));
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    store.upsert_candidate(candidate("record-b", 1)).unwrap();
    verify(&mut store, "record-a", "projection-success", 1);
    store
        .disable("record-b", 1, "trusted-operator", "excluded version")
        .unwrap();
    let capacity = store.projection();
    assert_eq!(
        (
            capacity.records,
            capacity.scripts,
            capacity.cases,
            capacity.quarantined_versions
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(capacity.statuses.verified, 1);
    assert_eq!(capacity.statuses.disabled, 1);
    assert!(capacity.journal_bytes > 0);
    let q = KnowledgeInspectionQuery {
        after_id: None,
        status: None,
        quarantined: Some(true),
        limit: 1,
    };
    let page = store.inspect(&q).unwrap();
    assert_eq!(page[0].id, "record-a");
    assert_eq!(page[0].verified_case_count, 1);
    assert_eq!(page[0].latest_verified_at_ms, Some(200));
    assert!(page[0].quarantined);
    let serialized = serde_json::to_string(&page).unwrap();
    assert!(!serialized.contains("external action proposal"));
    assert!(!serialized.contains("source"));
    let next = store
        .inspect(&KnowledgeInspectionQuery {
            after_id: Some(page[0].id.clone()),
            ..q
        })
        .unwrap();
    assert_eq!(next[0].id, "record-b");
    store.close().unwrap();
    assert!(!store.projection().writable);
    assert!(matches!(
        store.export_compacted(),
        Err(KnowledgeError::Unavailable(_))
    ));
}

#[test]
fn complete_checkpoint_preserves_revisions_all_case_keys_versions_and_quarantine() {
    let dir = TestDir::new("knowledge-complete-checkpoint");
    let path = dir.path.join("knowledge.jsonl");
    let compact_path = dir.path.join("compacted.jsonl");
    let mut store = open(&path);
    for (id, version) in [("record-a", 1), ("record-b", 1), ("record-c", 2)] {
        store.upsert_candidate(candidate(id, version)).unwrap();
    }
    verify(&mut store, "record-a", "checkpoint-first-success", 1);
    store
        .record_outcome(
            "record-a",
            case("checkpoint-failure", RepairOutcome::Failed, 1),
            None,
        )
        .unwrap();
    verify(&mut store, "record-a", "checkpoint-later-success", 1);
    verify(&mut store, "record-b", "checkpoint-other-success", 1);
    store
        .disable("record-b", 2, "trusted-operator", "retired version")
        .unwrap();
    verify(&mut store, "record-c", "checkpoint-new-version", 2);
    let expected: Vec<_> = ["record-a", "record-b", "record-c"]
        .into_iter()
        .map(|id| store.get(id).unwrap())
        .collect();
    let plan = store.export_compacted().unwrap();
    assert_eq!(
        (
            plan.records,
            plan.scripts,
            plan.cases,
            plan.quarantined_versions
        ),
        (3, 2, 5, 1)
    );
    std::fs::write(&compact_path, plan.as_bytes()).unwrap();
    let mut compacted = open(&compact_path);
    for record in &expected {
        assert_eq!(compacted.get(&record.id).unwrap(), *record);
        assert_eq!(
            compacted
                .upsert_candidate(record.candidate.clone())
                .unwrap(),
            *record
        );
        for entry in &record.cases {
            let attestation = entry.verification.as_ref().map(|proof| {
                TrustedBusinessVerification::attest(
                    &proof.operation_id,
                    &proof.target_id,
                    &proof.script_id,
                    proof.script_version,
                    &proof.verifier_id,
                    proof.evidence_refs.clone(),
                    proof.verified_at_ms,
                )
                .unwrap()
            });
            assert_eq!(
                compacted
                    .record_outcome(&record.id, entry.result.clone(), attestation)
                    .unwrap(),
                *record
            );
        }
    }
    assert_eq!(
        std::fs::metadata(&compact_path).unwrap().len(),
        plan.compacted_bytes
    );
    assert_eq!(
        compacted
            .search(&query())
            .unwrap()
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ["record-c"]
    );
    let mut substitution = candidate("record-d", 1);
    substitution.script.source = "print('substituted version')".into();
    assert!(matches!(
        compacted.upsert_candidate(substitution),
        Err(KnowledgeError::Conflict(_))
    ));
    assert_eq!(
        compacted
            .disable("record-b", 2, "trusted-operator", "retired version")
            .unwrap(),
        expected[1]
    );
    compacted
        .upsert_candidate(candidate("record-d", 3))
        .unwrap();
    verify(&mut compacted, "record-d", "checkpoint-post-append", 3);
    drop(compacted);
    let reopened = open(&compact_path);
    assert_eq!(reopened.get("record-a").unwrap(), expected[0]);
    assert_eq!(
        reopened.get("record-d").unwrap().status,
        KnowledgeStatus::Verified
    );
}

#[test]
fn checkpoint_replay_rejects_missing_commit_promoted_state_and_omitted_facts() {
    let dir = TestDir::new("knowledge-checkpoint-corruption");
    let mut store = open(&dir.path.join("original.jsonl"));
    store.upsert_candidate(candidate("record-a", 1)).unwrap();
    store
        .record_outcome(
            "record-a",
            case("checkpoint-unknown", RepairOutcome::Unknown, 1),
            None,
        )
        .unwrap();
    let plan = store.export_compacted().unwrap();
    let entries: Vec<serde_json::Value> = std::str::from_utf8(plan.as_bytes())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for mutation in [
        "commit",
        "status",
        "revision",
        "quarantine",
        "evidence",
        "duplicate_case",
    ] {
        let path = dir.path.join(format!("{mutation}.jsonl"));
        let mut changed = entries.clone();
        match mutation {
            "commit" => {
                changed.pop();
            }
            "status" => changed[1]["event"]["record"]["status"] = "verified".into(),
            "revision" => changed[1]["event"]["record"]["revision"] = 1.into(),
            "quarantine" => changed[0]["event"]["metadata"]["quarantined_versions"] = 0.into(),
            "evidence" => {
                changed[2]["event"]["cases"][0]["result"]["evidence_refs"] = serde_json::json!([])
            }
            _ => {
                let entry = changed[2]["event"]["cases"][0].clone();
                changed[2]["event"]["cases"]
                    .as_array_mut()
                    .unwrap()
                    .push(entry);
            }
        }
        let bytes = changed
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&path, bytes).unwrap();
        assert!(
            matches!(
                KnowledgeStore::open(path, KnowledgeStoreConfig::default()),
                Err(KnowledgeError::Corrupt(_))
            ),
            "accepted {mutation}"
        );
    }
}

#[test]
fn checkpoint_chunks_large_records_and_recompacts_without_losing_case_keys() {
    let dir = TestDir::new("knowledge-checkpoint-chunks");
    let mut store = open(&dir.path.join("original.jsonl"));
    store
        .upsert_candidate(candidate("large-record", 1))
        .unwrap();
    for index in 0..40 {
        let mut result = case(&format!("large-case-{index:02}"), RepairOutcome::Unknown, 1);
        result.evidence_refs = (0..16)
            .map(|reference| format!("observation-{reference:02}:{}", "x".repeat(500)))
            .collect();
        store.record_outcome("large-record", result, None).unwrap();
    }
    let expected = store.get("large-record").unwrap();
    let checkpoint = store.export_compacted().unwrap();
    let lines: Vec<_> = checkpoint
        .as_bytes()
        .split_inclusive(|byte| *byte == b'\n')
        .collect();
    assert!(lines.iter().all(|line| line.len() <= 256 * 1024));
    assert!(
        lines
            .iter()
            .filter(|line| {
                let entry: serde_json::Value = serde_json::from_slice(line).unwrap();
                entry["event"]["type"] == "checkpoint_cases"
            })
            .count()
            >= 2
    );
    let first_path = dir.path.join("first-checkpoint.jsonl");
    std::fs::write(&first_path, checkpoint.as_bytes()).unwrap();
    let mut first = open(&first_path);
    assert_eq!(first.get("large-record").unwrap(), expected);
    let repeated = first.export_compacted().unwrap();
    first.close().unwrap();
    let second_path = dir.path.join("second-checkpoint.jsonl");
    std::fs::write(&second_path, repeated.as_bytes()).unwrap();
    let mut second = open(&second_path);
    for entry in &expected.cases {
        assert_eq!(
            second
                .record_outcome("large-record", entry.result.clone(), None)
                .unwrap(),
            expected
        );
    }
    assert_eq!(second.projection().cases, 40);
    assert_eq!(second.projection().quarantined_versions, 1);
    assert_eq!(
        std::fs::metadata(&second_path).unwrap().len(),
        repeated.compacted_bytes
    );
}

#[test]
fn offline_compaction_recovers_byte_capacity_without_evading_record_or_case_limits() {
    let dir = TestDir::new("knowledge-checkpoint-capacity");
    let path = dir.path.join("original.jsonl");
    let mut initial = open(&path);
    initial.upsert_candidate(candidate("record-a", 1)).unwrap();
    for index in 0..30 {
        verify(&mut initial, "record-a", &format!("success-{index:02}"), 1);
    }
    let before = initial.projection();
    let plan = initial.export_compacted().unwrap();
    assert!(plan.compacted_bytes < before.journal_bytes);
    initial.close().unwrap();
    let config = KnowledgeStoreConfig {
        max_records: 1,
        max_cases_per_record: 31,
        max_journal_bytes: before.journal_bytes,
    };
    let mut full = KnowledgeStore::open(&path, config.clone()).unwrap();
    assert!(matches!(
        full.record_outcome(
            "record-a",
            case("capacity-next", RepairOutcome::Verified, 1),
            Some(proof(&case("capacity-next", RepairOutcome::Verified, 1)))
        ),
        Err(KnowledgeError::Capacity(_))
    ));
    assert_eq!(full.get("record-a").unwrap().cases.len(), 30);
    let fresh_plan = full.export_compacted().unwrap();
    full.close().unwrap();
    let compact_path = dir.path.join("compacted.jsonl");
    std::fs::write(&compact_path, fresh_plan.as_bytes()).unwrap();
    let mut compacted = KnowledgeStore::open(&compact_path, config).unwrap();
    verify(&mut compacted, "record-a", "capacity-next", 1);
    assert_eq!(compacted.get("record-a").unwrap().cases.len(), 31);
    assert!(matches!(
        compacted.upsert_candidate(candidate("record-b", 2)),
        Err(KnowledgeError::Capacity(_))
    ));
    assert!(matches!(
        compacted.record_outcome(
            "record-a",
            case("too-many-cases", RepairOutcome::Unknown, 1),
            None
        ),
        Err(KnowledgeError::Capacity(_))
    ));
}
