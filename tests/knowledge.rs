use recuvora_core::operation::{CommitReceipt, CommitRequest};
use recuvora_core::recovery::knowledge::*;
use std::collections::BTreeMap;

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

fn initial() -> KnowledgeState {
    KnowledgeState::new(KnowledgeConfig::default()).unwrap()
}

fn commit(state: KnowledgeState, command: KnowledgeCommand) -> KnowledgeState {
    let pending = state
        .propose(format!("commit-{}", state.revision()), command)
        .unwrap();
    let receipt = CommitReceipt::confirmed(pending.request());
    pending.confirm(receipt).unwrap().state
}

fn verified(record: &str, id: &str, version: u64) -> KnowledgeCommand {
    let case = case(id, RepairOutcome::Verified, version);
    let verification = Some(proof(&case));
    KnowledgeCommand::RecordOutcome {
        record_id: record.into(),
        case,
        verification,
    }
}

#[test]
fn proposals_preserve_original_state_and_require_matching_confirmation() {
    let state = initial();
    let pending = state
        .propose(
            "candidate",
            KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
        )
        .unwrap();
    assert!(state.get("a").is_none());
    assert_eq!(state.revision(), 0);
    let wrong = CommitRequest {
        id: "different".into(),
        expected_revision: 0,
        revision: 1,
        domain: "knowledge".into(),
        input: serde_json::Value::Null,
    };
    assert!(pending.confirm(CommitReceipt::confirmed(&wrong)).is_err());
    let state = commit(state, KnowledgeCommand::UpsertCandidate(candidate("a", 1)));
    assert_eq!(state.get("a").unwrap().status, KnowledgeStatus::Candidate);
    assert!(state.search(&query()).unwrap().is_empty());
}

#[test]
fn verified_requires_bound_trusted_evidence() {
    let state = commit(
        initial(),
        KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
    );
    let result = case("case-a", RepairOutcome::Verified, 1);
    assert!(
        state
            .propose(
                "no-proof",
                KnowledgeCommand::RecordOutcome {
                    record_id: "a".into(),
                    case: result.clone(),
                    verification: None,
                }
            )
            .is_err()
    );
    let mut wrong = result.clone();
    wrong.target_id = "other-target".into();
    assert!(
        state
            .propose(
                "wrong-proof",
                KnowledgeCommand::RecordOutcome {
                    record_id: "a".into(),
                    case: result,
                    verification: Some(proof(&wrong)),
                }
            )
            .is_err()
    );
    let state = commit(state, verified("a", "case-a", 1));
    assert_eq!(state.search(&query()).unwrap().len(), 1);
    assert!(state.search_reusable(&query()).unwrap().is_empty());
}

#[test]
fn immutable_versions_and_global_case_idempotency_are_retained() {
    let original = candidate("a", 1);
    let state = commit(
        initial(),
        KnowledgeCommand::UpsertCandidate(original.clone()),
    );
    let state = commit(state, verified("a", "case-a", 1));
    let before = state.get("a").unwrap();
    let state = commit(state, verified("a", "case-a", 1));
    assert_eq!(state.get("a").unwrap(), before);
    let state = commit(state, KnowledgeCommand::UpsertCandidate(original));
    assert_eq!(state.get("a").unwrap(), before);
    let mut conflicting = candidate("b", 1);
    conflicting.script.source = "changed content".into();
    assert!(
        state
            .propose(
                "changed-script",
                KnowledgeCommand::UpsertCandidate(conflicting)
            )
            .is_err()
    );
    let state = commit(state, KnowledgeCommand::UpsertCandidate(candidate("b", 1)));
    assert!(
        state
            .propose("case-reuse", verified("b", "case-a", 1))
            .is_err()
    );
}

#[test]
fn failure_unknown_and_disablement_permanently_quarantine_all_same_version_cases() {
    for outcome in [RepairOutcome::Failed, RepairOutcome::Unknown] {
        let state = commit(
            initial(),
            KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
        );
        let state = commit(state, KnowledgeCommand::UpsertCandidate(candidate("b", 1)));
        let state = commit(state, verified("b", "success-b", 1));
        let state = commit(
            state,
            KnowledgeCommand::RecordOutcome {
                record_id: "a".into(),
                case: case("failure", outcome, 1),
                verification: None,
            },
        );
        let state = commit(state, verified("a", "later-success", 1));
        assert_eq!(state.get("a").unwrap().status, KnowledgeStatus::Verified);
        assert!(state.is_quarantined("repair-script", 1));
        assert!(state.search(&query()).unwrap().is_empty());
        let state = commit(state, KnowledgeCommand::UpsertCandidate(candidate("c", 2)));
        let state = commit(state, verified("c", "new-version-success", 2));
        assert_eq!(state.search(&query()).unwrap()[0].id, "c");
    }
    let state = commit(
        initial(),
        KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
    );
    let state = commit(
        state,
        KnowledgeCommand::Disable {
            record_id: "a".into(),
            expected_revision: 1,
            actor: "maintainer".into(),
            reason: "excluded".into(),
        },
    );
    assert!(state.is_quarantined("repair-script", 1));
    assert!(
        state
            .propose("post-disable", verified("a", "later", 1))
            .is_err()
    );
}

#[test]
fn replay_repeats_validation_and_preserves_quarantine_even_after_later_success() {
    let commands = vec![
        KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
        KnowledgeCommand::RecordOutcome {
            record_id: "a".into(),
            case: case("unknown", RepairOutcome::Unknown, 1),
            verification: None,
        },
        verified("a", "later-success", 1),
    ];
    let mut state = initial();
    let mut history = Vec::new();
    for (index, command) in commands.into_iter().enumerate() {
        let pending = state
            .propose(format!("commit-{index}"), command.clone())
            .unwrap();
        let request = pending.request().clone();
        state = pending
            .confirm(CommitReceipt::confirmed(&request))
            .unwrap()
            .state;
        history.push(KnowledgeReplayEntry {
            receipt: CommitReceipt::confirmed(&request),
            request,
            command,
        });
    }
    let replayed = KnowledgeState::replay(KnowledgeConfig::default(), history).unwrap();
    assert_eq!(replayed.get("a"), state.get("a"));
    assert!(replayed.is_quarantined("repair-script", 1));
    assert!(replayed.search(&query()).unwrap().is_empty());
    let request = CommitRequest {
        id: "gap".into(),
        expected_revision: 1,
        revision: 2,
        domain: "knowledge".into(),
        input: serde_json::Value::Null,
    };
    assert!(
        KnowledgeState::replay(
            KnowledgeConfig::default(),
            vec![KnowledgeReplayEntry {
                receipt: CommitReceipt::confirmed(&request),
                request,
                command: KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
            }]
        )
        .is_err()
    );
    let request = CommitRequest {
        id: "bad-evidence".into(),
        expected_revision: 0,
        revision: 1,
        domain: "knowledge".into(),
        input: serde_json::Value::Null,
    };
    assert!(
        KnowledgeState::replay(
            KnowledgeConfig::default(),
            vec![KnowledgeReplayEntry {
                receipt: CommitReceipt::confirmed(&request),
                request,
                command: verified("missing", "case-a", 1),
            }]
        )
        .is_err()
    );
}

#[test]
fn exact_matching_and_reuse_filter_precede_limit_and_ranking_is_stable() {
    let mut reusable = candidate("b", 2);
    reusable.reusable = true;
    let state = commit(
        initial(),
        KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
    );
    let state = commit(state, KnowledgeCommand::UpsertCandidate(reusable));
    let state = commit(state, verified("a", "success-a", 1));
    let state = commit(state, verified("a", "success-a2", 1));
    let state = commit(state, verified("b", "success-b", 2));
    let mut limited = query();
    limited.limit = 1;
    assert_eq!(state.search(&limited).unwrap()[0].id, "a");
    assert_eq!(state.search_reusable(&limited).unwrap()[0].id, "b");
    limited
        .conditions
        .insert("workload_version".into(), "2".into());
    assert!(state.search(&limited).unwrap().is_empty());
    let mut different_case = query();
    different_case.keywords = vec!["Condition".into()];
    assert!(state.search(&different_case).unwrap().is_empty());
}

#[test]
fn external_candidates_are_read_only_bound_and_never_reusable() {
    let state = initial();
    let mut proposed = candidate("external", 1);
    proposed.reusable = true;
    let proposal = KnowledgeProposal {
        source_id: "source-a".into(),
        candidate: proposed,
        evidence_refs: vec!["external-evidence".into()],
    };
    let accepted = state
        .validate_external_candidates("source-a", &query(), vec![proposal.clone()])
        .unwrap();
    assert!(!accepted[0].reusable);
    assert!(state.get("external").is_none());
    assert!(
        state
            .validate_external_candidates("source-b", &query(), vec![proposal.clone()])
            .is_err()
    );
    assert!(
        state
            .validate_external_candidates("source-a", &query(), vec![proposal.clone(), proposal])
            .is_err()
    );
    let state = commit(
        state,
        KnowledgeCommand::UpsertCandidate(candidate("local", 1)),
    );
    let state = commit(
        state,
        KnowledgeCommand::RecordOutcome {
            record_id: "local".into(),
            case: case("failure", RepairOutcome::Failed, 1),
            verification: None,
        },
    );
    let proposal = KnowledgeProposal {
        source_id: "source-a".into(),
        candidate: candidate("external", 1),
        evidence_refs: vec!["external-evidence".into()],
    };
    assert!(
        state
            .validate_external_candidates("source-a", &query(), vec![proposal])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn logical_capacity_rejects_before_commit_and_does_not_drop_history() {
    let state = KnowledgeState::new(KnowledgeConfig {
        max_records: 1,
        max_cases_per_record: 1,
    })
    .unwrap();
    let state = commit(state, KnowledgeCommand::UpsertCandidate(candidate("a", 1)));
    assert!(matches!(
        state.propose(
            "exhausted",
            KnowledgeCommand::UpsertCandidate(candidate("b", 2))
        ),
        Err(KnowledgeError::Capacity(_))
    ));
    let state = commit(state, verified("a", "case-a", 1));
    assert!(matches!(
        state.propose("case-exhausted", verified("a", "case-b", 1)),
        Err(KnowledgeError::Capacity(_))
    ));
    assert_eq!(state.get("a").unwrap().cases.len(), 1);
    assert_eq!(state.projection().cases, 1);
}

#[test]
fn external_response_allocation_and_content_are_bounded() {
    let state = initial();
    let mut large = candidate("large", 1);
    large.script.source = "x".repeat(MAX_SCRIPT_BYTES);
    large.conditions = (0..32)
        .map(|index| (format!("condition-{index}"), "v".repeat(1024)))
        .collect();
    large.script.preconditions = large.conditions.clone();
    let proposals = (0..100)
        .map(|index| {
            let mut candidate = large.clone();
            candidate.id = format!("external-{index}");
            KnowledgeProposal {
                source_id: "source-a".into(),
                candidate,
                evidence_refs: vec!["external".into()],
            }
        })
        .collect();
    let mut query = query();
    query.limit = 100;
    assert!(matches!(
        state.validate_external_candidates("source-a", &query, proposals),
        Err(KnowledgeError::Capacity(_))
    ));
    large.script.source.push('x');
    assert!(matches!(
        state.validate_external_candidates(
            "source-a",
            &query,
            vec![KnowledgeProposal {
                source_id: "source-a".into(),
                candidate: large,
                evidence_refs: vec!["external".into()],
            }]
        ),
        Err(KnowledgeError::Invalid(_))
    ));
}

#[test]
fn replay_rejects_unverified_success_and_preserves_idempotency_keys() {
    let first = initial()
        .propose(
            "candidate",
            KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
        )
        .unwrap()
        .request()
        .clone();
    let second = CommitRequest {
        id: "invalid-success".into(),
        expected_revision: 1,
        revision: 2,
        domain: "knowledge".into(),
        input: serde_json::Value::Null,
    };
    let entries = vec![
        KnowledgeReplayEntry {
            receipt: CommitReceipt::confirmed(&first),
            request: first,
            command: KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
        },
        KnowledgeReplayEntry {
            receipt: CommitReceipt::confirmed(&second),
            request: second,
            command: KnowledgeCommand::RecordOutcome {
                record_id: "a".into(),
                case: case("case-a", RepairOutcome::Verified, 1),
                verification: None,
            },
        },
    ];
    assert!(matches!(
        KnowledgeState::replay(KnowledgeConfig::default(), entries),
        Err(KnowledgeError::Invalid(_))
    ));
    let commands = vec![
        KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
        verified("a", "case-a", 1),
        verified("a", "case-a", 1),
    ];
    let mut original = KnowledgeState::new(KnowledgeConfig {
        max_records: 1,
        max_cases_per_record: 1,
    })
    .unwrap();
    let entries = commands
        .into_iter()
        .enumerate()
        .map(|(index, command)| {
            let pending = original
                .propose(format!("commit-{index}"), command.clone())
                .unwrap();
            let request = pending.request().clone();
            original = pending
                .confirm(CommitReceipt::confirmed(&request))
                .unwrap()
                .state;
            KnowledgeReplayEntry {
                receipt: CommitReceipt::confirmed(&request),
                request,
                command,
            }
        })
        .collect();
    let state = KnowledgeState::replay(
        KnowledgeConfig {
            max_records: 1,
            max_cases_per_record: 1,
        },
        entries,
    )
    .unwrap();
    assert_eq!(state.get("a").unwrap().cases.len(), 1);
    assert_eq!(state.get("a").unwrap().revision, 2);
    assert_eq!(state.revision(), 3);
    let mut changed_case = case("case-a", RepairOutcome::Unknown, 1);
    changed_case.evidence_refs = vec!["changed".into()];
    assert!(matches!(
        state.propose(
            "conflicting-retry",
            KnowledgeCommand::RecordOutcome {
                record_id: "a".into(),
                case: changed_case,
                verification: None,
            }
        ),
        Err(KnowledgeError::Conflict(_))
    ));
}

#[test]
fn host_transport_export_preserves_failure_history_after_success() {
    let state = commit(
        initial(),
        KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
    );
    let state = commit(
        state,
        KnowledgeCommand::RecordOutcome {
            record_id: "a".into(),
            case: case("unknown", RepairOutcome::Unknown, 1),
            verification: None,
        },
    );
    let command = verified("a", "later", 1);
    let encoded = serde_json::to_value(&command).unwrap();
    assert!(encoded["RecordOutcome"]["verification"]["verifier_id"].is_string());
    let state = commit(state, command);
    let snapshot = serde_json::to_value(state.snapshot()).unwrap();
    assert_eq!(snapshot["records"][0]["cases"].as_array().unwrap().len(), 2);
    assert_eq!(
        snapshot["records"][0]["cases"][0]["result"]["outcome"],
        "unknown"
    );
    assert_eq!(snapshot["records"][0]["status"], "verified");
    assert!(state.is_quarantined("repair-script", 1));
}

#[test]
fn external_candidate_normalization_is_repeatable_but_conflicts_remain_atomic() {
    let mut raw = candidate("external", 1);
    raw.reusable = true;
    raw.evidence_refs.push("observation:2".into());
    let proposal = KnowledgeProposal {
        source_id: "source".into(),
        candidate: raw,
        evidence_refs: vec!["proposal:2".into(), "proposal:1".into()],
    };
    let normalized = initial()
        .validate_external_candidates("source", &query(), vec![proposal.clone()])
        .unwrap()
        .remove(0);
    assert!(!normalized.reusable);
    let state = commit(
        initial(),
        KnowledgeCommand::UpsertCandidate(normalized.clone()),
    );
    assert_eq!(
        state
            .validate_external_candidates("source", &query(), vec![proposal.clone()])
            .unwrap(),
        vec![normalized.clone()]
    );
    let mut reordered = proposal.clone();
    reordered.candidate.evidence_refs.reverse();
    reordered.evidence_refs.reverse();
    reordered
        .candidate
        .evidence_refs
        .push("knowledge-source:source".into());
    assert_eq!(
        state
            .validate_external_candidates("source", &query(), vec![reordered])
            .unwrap(),
        vec![normalized]
    );
    for field in 0..3 {
        let mut changed = proposal.clone();
        match field {
            0 => changed.candidate.script.source.push_str("changed"),
            1 => changed.candidate.summary.push_str("changed"),
            _ => changed.source_id = "other".into(),
        }
        let expected = changed.source_id.clone();
        assert!(
            state
                .validate_external_candidates(&expected, &query(), vec![changed])
                .is_err()
        );
    }
    let mut conflict = proposal.clone();
    conflict.candidate.id = "second".into();
    conflict.candidate.script.source.push_str("changed");
    assert!(
        state
            .validate_external_candidates("source", &query(), vec![proposal, conflict])
            .is_err()
    );
    assert_eq!(state.projection().records, 1);
}

#[test]
fn capacity_expansion_replays_and_preserves_all_isolation_and_identities() {
    let original = KnowledgeConfig {
        max_records: 2,
        max_cases_per_record: 1,
    };
    let target = KnowledgeConfig {
        max_records: 3,
        max_cases_per_record: 2,
    };
    let mut state = KnowledgeState::new(original.clone()).unwrap();
    let mut history = Vec::new();
    let commands = vec![
        KnowledgeCommand::UpsertCandidate(candidate("a", 1)),
        KnowledgeCommand::RecordOutcome {
            record_id: "a".into(),
            case: case("unknown", RepairOutcome::Unknown, 1),
            verification: None,
        },
        KnowledgeCommand::UpsertCandidate(candidate("b", 2)),
        KnowledgeCommand::RecordOutcome {
            record_id: "b".into(),
            case: case("failed", RepairOutcome::Failed, 2),
            verification: None,
        },
        KnowledgeCommand::Disable {
            record_id: "b".into(),
            expected_revision: 2,
            actor: "operator".into(),
            reason: "disabled".into(),
        },
    ];
    for (i, command) in commands.into_iter().enumerate() {
        let pending = state
            .propose(format!("commit-{i}"), command.clone())
            .unwrap();
        let request = pending.request().clone();
        state = pending
            .confirm(CommitReceipt::confirmed(&request))
            .unwrap()
            .state;
        history.push((request, command));
    }
    let outcome = verified("a", "later", 1);
    assert!(matches!(
        state.propose("full", outcome.clone()),
        Err(KnowledgeError::Capacity(_))
    ));
    let before = state.snapshot();
    let expansion = KnowledgeCommand::ExpandCapacity {
        expected: original.clone(),
        target: target.clone(),
    };
    let pending = state.propose("expand", expansion.clone()).unwrap();
    assert_eq!(state.snapshot(), before);
    let request = pending.request().clone();
    // Interruption before confirmation leaves the old configuration replayable.
    drop(pending);
    let replay = |items: &[(CommitRequest, KnowledgeCommand)], config: KnowledgeConfig| {
        KnowledgeState::replay(
            config,
            items
                .iter()
                .map(|(request, command)| KnowledgeReplayEntry {
                    request: request.clone(),
                    command: command.clone(),
                    receipt: CommitReceipt::confirmed(request),
                })
                .collect(),
        )
    };
    assert_eq!(
        replay(&history, original.clone()).unwrap().snapshot(),
        before
    );
    assert!(replay(&history, target.clone()).is_err());
    history.push((request.clone(), expansion.clone()));
    state = replay(&history, original.clone()).unwrap();
    assert_eq!(state.snapshot().config, target);
    assert_eq!(state.snapshot().records, before.records);
    assert!(state.is_quarantined("repair-script", 1));
    assert!(state.is_quarantined("repair-script", 2));
    assert!(state.propose("repeat", expansion).is_err());
    assert!(state.propose("expand", outcome.clone()).is_err());
    assert!(
        state
            .propose(
                "shrink",
                KnowledgeCommand::ExpandCapacity {
                    expected: target.clone(),
                    target: original.clone()
                }
            )
            .is_err()
    );
    assert!(
        state
            .propose(
                "invalid",
                KnowledgeCommand::ExpandCapacity {
                    expected: target.clone(),
                    target: KnowledgeConfig {
                        max_records: 100_001,
                        ..target.clone()
                    }
                }
            )
            .is_err()
    );
    let pending = state.propose("later", outcome.clone()).unwrap();
    history.push((pending.request().clone(), outcome));
    state = pending
        .confirm(CommitReceipt::confirmed(&history.last().unwrap().0))
        .unwrap()
        .state;
    assert_eq!(state.get("a").unwrap().cases.len(), 2);
    assert!(state.is_quarantined("repair-script", 1));
    assert!(state.search(&query()).unwrap().is_empty());
    let mut changed_case = case("unknown", RepairOutcome::Unknown, 1);
    changed_case.operation_id = "different".into();
    assert!(
        state
            .propose(
                "id-conflict",
                KnowledgeCommand::RecordOutcome {
                    record_id: "a".into(),
                    case: changed_case,
                    verification: None
                }
            )
            .is_err()
    );
    let mut changed_candidate = candidate("new", 1);
    changed_candidate.script.source.push_str("changed");
    assert!(
        state
            .propose(
                "script-conflict",
                KnowledgeCommand::UpsertCandidate(changed_candidate)
            )
            .is_err()
    );
    assert_eq!(
        replay(&history, original).unwrap().snapshot(),
        state.snapshot()
    );
    let mut bad = history.clone();
    bad.last_mut().unwrap().0.input[0] = serde_json::json!("0".repeat(64));
    assert!(
        replay(
            &bad,
            KnowledgeConfig {
                max_records: 2,
                max_cases_per_record: 1
            }
        )
        .is_err()
    );
}

#[test]
fn scale_knowledge_commits_bind_incremental_commands() {
    let mut state = initial();
    let start = std::time::Instant::now();
    let mut history = Vec::new();
    let mut largest = 0;
    for i in 0..500 {
        let command =
            KnowledgeCommand::UpsertCandidate(candidate(&format!("candidate-{i}"), i + 1));
        let pending = state
            .propose(format!("commit-{i}"), command.clone())
            .unwrap();
        largest = largest.max(serde_json::to_vec(pending.request()).unwrap().len());
        assert!(largest < 2000);
        let request = pending.request().clone();
        state = pending
            .confirm(CommitReceipt::confirmed(&request))
            .unwrap()
            .state;
        history.push(KnowledgeReplayEntry {
            receipt: CommitReceipt::confirmed(&request),
            request,
            command,
        });
    }
    let prepare = start.elapsed();
    let start = std::time::Instant::now();
    let restored = KnowledgeState::replay(KnowledgeConfig::default(), history).unwrap();
    let restore = start.elapsed();
    assert_eq!(restored.snapshot(), state.snapshot());
    println!(
        "scale knowledge n=500 max_request={largest} snapshot_bytes={} prepare={prepare:?} restore={restore:?}",
        serde_json::to_vec(&state.snapshot()).unwrap().len()
    );
}
