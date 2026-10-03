use recuvora_core::recovery::{
    BusinessError, ExperienceInput, build_experience, knowledge::*, planning::*,
};
use std::collections::BTreeMap;

fn input() -> RepairRequestInput {
    RepairRequestInput {
        problem: ProblemContext {
            incident_id: "incident".into(),
            incident_revision: 1,
            target_id: "target".into(),
            fingerprint: "unhealthy".into(),
            summary: "Workload is unavailable".into(),
            occurrences: 2,
            keywords: vec!["workload".into()],
            conditions: BTreeMap::from([("environment".into(), "test".into())]),
            evidence_refs: vec!["observation:1".into()],
        },
        observation: TargetObservation {
            target_id: "target".into(),
            facts: BTreeMap::from([
                ("environment".into(), "test".into()),
                ("sample".into(), "dynamic".into()),
            ]),
            evidence_refs: vec!["observation:1".into()],
            observed_at_ms: 10,
        },
        harness_id: "harness".into(),
        delegation: "restore workload".into(),
        target: TargetBinding {
            target_id: "target".into(),
            executor_id: "executor".into(),
            allowed_action_kinds: vec!["restart".into()],
            verification_profile: "health".into(),
            required_facts: BTreeMap::from([("environment".into(), "test".into())]),
            action_timeout_secs: 30,
        },
        max_tool_calls: 4,
    }
}

fn experience(id: &str, time: u64, outcome: RepairOutcome) -> RepairExperience {
    let request = input();
    RepairExperience {
        id: id.into(),
        operation_id: format!("op-{id}"),
        target_id: "target".into(),
        conditions: stable_conditions(&request.problem, &request.target).unwrap(),
        keywords: vec!["workload".into(), "unhealthy".into()],
        outcome,
        evidence_refs: vec!["result:1".into()],
        recorded_at_ms: time,
        actions: vec![],
        report: ExperienceReport {
            summary: "Investigated workload".into(),
            lessons: "Inspect before changing".into(),
            related_experience_ids: vec![],
            scriptability: Scriptability::NotSuitable {
                reason: "Requires contextual judgment".into(),
            },
        },
    }
}

#[test]
fn unknown_fault_uses_same_request_and_requires_summary_and_script_assessment() {
    let request = prepare_repair(input(), []).unwrap();
    assert_eq!(request.matched_experience_count, 0);
    assert!(request.experiences.is_empty());
    assert!(request.summarize_experience && request.assess_scriptability);
    assert_eq!(request.problem.fingerprint, "unhealthy");
    assert_eq!(request.observation.facts["sample"], "dynamic");
}

#[test]
fn known_fault_includes_failed_and_unknown_evidence_with_deterministic_order() {
    let records = [
        experience("z", 30, RepairOutcome::Failed),
        experience("older", 10, RepairOutcome::Verified),
        experience("a", 30, RepairOutcome::Unknown),
    ];
    let request = prepare_repair(input(), records.iter()).unwrap();
    assert_eq!(request.matched_experience_count, 3);
    assert_eq!(
        request
            .experiences
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z", "older"]
    );
    assert_eq!(request.experiences[0].outcome, RepairOutcome::Unknown);
    assert_eq!(request.experiences[1].outcome, RepairOutcome::Failed);
    assert!(request.summarize_experience && request.assess_scriptability);
}

#[test]
fn matching_uses_exact_stable_conditions_and_case_sensitive_keyword_and() {
    let mut records = vec![experience("matches", 1, RepairOutcome::Verified)];
    let mut wrong = records[0].clone();
    wrong.id = "wrong-condition".into();
    wrong
        .conditions
        .insert("environment".into(), "production".into());
    records.push(wrong);
    let mut wrong = records[0].clone();
    wrong.id = "wrong-keyword".into();
    wrong.keywords = vec!["Workload".into()];
    records.push(wrong);
    let request = prepare_repair(input(), records.iter()).unwrap();
    assert_eq!(request.matched_experience_count, 1);
    assert_eq!(request.experiences[0].id, "matches");
    let mut changed = input();
    changed
        .observation
        .facts
        .insert("sample".into(), "changed".into());
    assert_eq!(
        prepare_repair(changed, records.iter()).unwrap().experiences,
        request.experiences
    );
}

#[test]
fn count_is_complete_even_when_only_four_whole_records_are_selected() {
    let records: Vec<_> = (0..9)
        .map(|i| experience(&format!("e{i}"), i, RepairOutcome::Verified))
        .collect();
    let request = prepare_repair(input(), records.iter()).unwrap();
    assert_eq!(request.matched_experience_count, 9);
    assert_eq!(request.experiences.len(), 4);
    assert_eq!(request.experiences[0], records[8]);
    assert_eq!(request.experiences[3], records[5]);
}

#[test]
fn budget_skips_large_references_and_continues_to_later_matches() {
    let mut records: Vec<_> = (0..5)
        .map(|i| experience(&format!("e{i}"), 10 - i, RepairOutcome::Verified))
        .collect();
    for record in &mut records[..4] {
        record.report.lessons = "汉".repeat(MAX_REPAIR_REQUEST_BYTES);
    }
    let request = prepare_repair(input(), records.iter()).unwrap();
    assert_eq!(request.matched_experience_count, 5);
    assert_eq!(request.experiences, vec![records[4].clone()]);
    assert!(serde_json::to_vec(&request).unwrap().len() <= MAX_REPAIR_REQUEST_BYTES);
}

#[test]
fn serialized_budget_includes_utf8_escaping_and_mandatory_context() {
    let base = prepare_repair(input(), []).unwrap();
    let spare = MAX_REPAIR_REQUEST_BYTES - serde_json::to_vec(&base).unwrap().len();
    let mut exact = input();
    exact.delegation.push_str(&"x".repeat(spare));
    let request = prepare_repair(exact, []).unwrap();
    assert_eq!(
        serde_json::to_vec(&request).unwrap().len(),
        MAX_REPAIR_REQUEST_BYTES
    );
    let mut oversized = input();
    oversized.delegation.push_str(&"x".repeat(spare));
    oversized.delegation.push('"');
    assert!(prepare_repair(oversized, []).is_err());
    let mut unicode = input();
    unicode.delegation.push_str(&"汉".repeat(spare / 3 + 1));
    assert!(prepare_repair(unicode, []).is_err());
}

#[test]
fn stable_conditions_merge_consistent_facts_and_reject_ambiguity() {
    let mut request = input();
    request
        .target
        .required_facts
        .insert("version".into(), "2".into());
    let conditions = stable_conditions(&request.problem, &request.target).unwrap();
    assert_eq!(conditions.len(), 3);
    assert_eq!(conditions["fault_fingerprint"], "unhealthy");
    assert!(!conditions.contains_key("sample"));
    request
        .target
        .required_facts
        .insert("environment".into(), "production".into());
    assert!(stable_conditions(&request.problem, &request.target).is_err());
    request = input();
    request
        .problem
        .conditions
        .insert("fault_fingerprint".into(), "spoof".into());
    assert!(prepare_repair(request, []).is_err());
}

#[test]
fn stable_conditions_enforce_total_capacity_after_union_and_fingerprint() {
    let mut request = input();
    request.problem.conditions = (0..31).map(|i| (format!("key{i}"), "v".into())).collect();
    assert!(matches!(
        stable_conditions(&request.problem, &request.target),
        Err(BusinessError::Capacity)
    ));
}

#[test]
fn experience_preserves_confirmed_outcome_separately_from_model_report() {
    let request = input();
    let mut report = experience("reference", 1, RepairOutcome::Verified).report;
    report.scriptability = Scriptability::Possible {
        reason: "Can automate".into(),
        candidate: Some(RepairArtifact {
            id: "candidate".into(),
            version: 1,
            kind: "restart".into(),
            payload: serde_json::json!({"action": "restart"}),
            preconditions: request.problem.conditions.clone(),
            generated_by_harness: "summarizer".into(),
            generated_in_session: "summary-session".into(),
        }),
    };
    let evidence = vec!["trusted-result".into()];
    for outcome in [
        RepairOutcome::Verified,
        RepairOutcome::Failed,
        RepairOutcome::Unknown,
    ] {
        let record = build_experience(ExperienceInput {
            id: "delivery",
            operation_id: "operation",
            problem: &request.problem,
            target: &request.target,
            outcome,
            actions: &[],
            evidence_refs: &evidence,
            recorded_at_ms: 42,
            report: &report,
        })
        .unwrap();
        assert_eq!(record.outcome, outcome);
        assert_eq!(record.report, report);
        assert!(record.actions.is_empty());
        assert_eq!(record.id, "delivery");
        assert_eq!(record.recorded_at_ms, 42);
        assert_eq!(record.evidence_refs, evidence);
        assert!(!record.conditions.contains_key("sample"));
    }
}

#[test]
fn planning_is_repeatable_and_does_not_attest_input_freshness_or_authorize_execution() {
    let records = [experience("a", 1, RepairOutcome::Verified)];
    let mut request = input();
    request.observation.observed_at_ms = 0;
    request.observation.target_id = "untrusted-target".into();
    let one = prepare_repair(request, records.iter()).unwrap();
    let mut same = input();
    same.observation = one.observation.clone();
    let two = prepare_repair(same, records.iter()).unwrap();
    assert_eq!(
        serde_json::to_value(one).unwrap(),
        serde_json::to_value(two).unwrap()
    );
    assert_eq!(records[0].id, "a");
}
