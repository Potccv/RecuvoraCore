use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use recuvora_core::recovery::approval::{
    ApprovalAssessment, ApprovalDecision, ApprovalError, ApprovalPolicy, ApprovalState,
    ApprovalStore, ApprovalStoreConfig, AssessmentSource, ExecutionOutcome, ModelAssessment,
    ProposedOperation, ReviewStage, ReviewerConfig, ReviewerIdentity,
};
use serde_json::json;

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir {
    path: PathBuf,
    root: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(
            std::env::var_os("RECUVORA_TEST_TEMP")
                .expect("set RECUVORA_TEST_TEMP to an external test directory"),
        );
        assert!(root.is_absolute());
        std::fs::create_dir_all(&root).expect("create test root");
        let root = root.canonicalize().expect("resolve test root");
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("project root");
        assert!(!root.starts_with(project));
        let path = root.join(format!(
            "approval-{name}-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create unique test directory");
        Self { path, root }
    }

    fn open(&self, now: u64) -> ApprovalStore {
        ApprovalStore::open(&self.path, ApprovalStoreConfig::default(), now).expect("open store")
    }

    fn journal(&self) -> PathBuf {
        self.path.join("approvals.jsonl")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.parent() == Some(self.root.as_path())
            && self.path.starts_with(&self.root)
            && let Err(error) = std::fs::remove_dir_all(&self.path)
            && !std::thread::panicking()
        {
            panic!("test directory cleanup failed: {error}");
        }
    }
}

fn policy() -> ApprovalPolicy {
    ApprovalPolicy {
        id: "repair-project".into(),
        version: 1,
        reviewer: ReviewerConfig::Harness {
            harness_id: "local".into(),
        },
        delegation: "Repair this project's configuration after checking user intent.".into(),
        allowed_targets: vec!["project-a".into()],
        allowed_action_kinds: vec!["replace_text".into()],
        ttl_secs: 60,
    }
}

fn operation(id: &str) -> ProposedOperation {
    ProposedOperation {
        task_id: "task-1".into(),
        task_revision: 3,
        operation_id: id.into(),
        target: "project-a".into(),
        action: json!({"kind":"replace_text","path":"config.txt","expected":"old","replacement":"new"}),
    }
}

fn assessment(id: &str, decision: ApprovalDecision) -> ModelAssessment {
    ModelAssessment {
        request_id: id.into(),
        decision,
        reason: "Matches the requested repair.".into(),
    }
}

fn reviewer() -> ReviewerIdentity {
    ReviewerIdentity {
        harness_id: "local".into(),
        session_id: "independent-review-session".into(),
    }
}

fn approved(store: &mut ApprovalStore, op: &ProposedOperation) -> String {
    let id = store
        .request(op.clone(), policy(), 100)
        .unwrap()
        .request
        .request_id;
    store
        .assess(
            &id,
            assessment(&id, ApprovalDecision::Approve),
            reviewer(),
            &policy(),
            101,
        )
        .unwrap();
    id
}

fn fallback_policy() -> ApprovalPolicy {
    ApprovalPolicy {
        reviewer: ReviewerConfig::HumanThenHarness {
            harness_id: "local".into(),
            human_wait_secs: 20,
            review_timeout_secs: 15,
        },
        ..policy()
    }
}

fn human_assessment(decision: ApprovalDecision) -> ApprovalAssessment {
    ApprovalAssessment {
        decision,
        reason: "independent human review".into(),
        reviewer: AssessmentSource::Human {
            actor: "operator".into(),
        },
    }
}

#[test]
fn human_deadline_hands_off_one_bounded_review_without_extending_authority() {
    let dir = TestDir::new("fallback-deadlines");
    let mut store = dir.open(100);
    let policy = fallback_policy();
    let requested = store
        .request(operation("fallback"), policy.clone(), 100)
        .unwrap();
    let id = &requested.request.request_id;
    assert_eq!(requested.state, ApprovalState::WaitingHuman);
    assert_eq!(requested.review_stage, ReviewStage::WaitingHuman);
    assert_eq!(requested.human_deadline, Some(120));
    assert!(matches!(
        store.begin_harness_review(id, 0, &policy, 15, 119),
        Err(ApprovalError::ReviewNotDue)
    ));
    assert!(
        store
            .assess(
                id,
                assessment(id, ApprovalDecision::Approve),
                reviewer(),
                &policy,
                119
            )
            .is_err()
    );
    assert!(store.begin_harness_review(id, 0, &policy, 16, 120).is_err());
    assert_eq!(store.get(id).unwrap(), &requested);
    let attempt = store.begin_harness_review(id, 0, &policy, 15, 122).unwrap();
    assert_eq!(attempt.deadline, 137);
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.revision, 1);
    assert_eq!(store.get(id).unwrap().request.expires_at, 160);
    assert!(store.begin_harness_review(id, 0, &policy, 15, 123).is_err());
    let approved = store
        .assess_attempt(
            &attempt,
            assessment(id, ApprovalDecision::Approve),
            reviewer(),
            &policy,
            123,
        )
        .unwrap();
    assert_eq!(approved.state, ApprovalState::Approved);
    assert_eq!(approved.review_stage, ReviewStage::Finished);
    assert!(
        store
            .assess_attempt(
                &attempt,
                assessment(id, ApprovalDecision::Approve),
                reviewer(),
                &policy,
                124
            )
            .is_err()
    );
    assert!(matches!(
        store.consume(id, &approved.request.operation, &policy, 160),
        Err(ApprovalError::Expired)
    ));
}

#[test]
fn timed_out_or_uncertain_fallback_requires_human_without_repeating_the_loop() {
    for decision in [None, Some(ApprovalDecision::Escalate)] {
        let dir = TestDir::new("fallback-no-loop");
        let mut store = dir.open(100);
        let policy = fallback_policy();
        let requested = store
            .request(operation("fallback"), policy.clone(), 100)
            .unwrap();
        let id = &requested.request.request_id;
        let attempt = store.begin_harness_review(id, 0, &policy, 15, 120).unwrap();
        let at = if let Some(decision) = decision {
            store
                .assess_attempt(&attempt, assessment(id, decision), reviewer(), &policy, 125)
                .unwrap();
            125
        } else {
            assert!(matches!(
                store.assess_attempt(
                    &attempt,
                    assessment(id, ApprovalDecision::Approve),
                    reviewer(),
                    &policy,
                    135
                ),
                Err(ApprovalError::ReviewTimedOut)
            ));
            135
        };
        let waiting = store.get(id).unwrap().clone();
        assert_eq!(waiting.review_stage, ReviewStage::NeedsHuman);
        assert_eq!(waiting.state, ApprovalState::WaitingHuman);
        assert!(
            store
                .begin_harness_review(id, waiting.revision, &policy, 15, at)
                .is_err()
        );
        assert!(
            store
                .assess_attempt(
                    &attempt,
                    assessment(id, ApprovalDecision::Approve),
                    reviewer(),
                    &policy,
                    at
                )
                .is_err()
        );
        assert!(
            store
                .consume(id, &requested.request.operation, &policy, at)
                .is_err()
        );
        store
            .decide_human_at_revision(
                id,
                waiting.revision,
                human_assessment(ApprovalDecision::Approve),
                &policy,
                at,
            )
            .unwrap();
    }
}

#[test]
fn human_decision_and_timeout_use_revision_to_reject_losing_callbacks() {
    let dir = TestDir::new("fallback-race");
    let mut store = dir.open(100);
    let policy = fallback_policy();
    let first = store
        .request(operation("human-wins"), policy.clone(), 100)
        .unwrap();
    let id = &first.request.request_id;
    store
        .decide_human_at_revision(
            id,
            0,
            human_assessment(ApprovalDecision::Deny),
            &policy,
            120,
        )
        .unwrap();
    assert!(store.begin_harness_review(id, 0, &policy, 15, 120).is_err());
    assert_eq!(store.get(id).unwrap().state, ApprovalState::Denied);

    let second = store
        .request(operation("timer-wins"), policy.clone(), 100)
        .unwrap();
    let id = &second.request.request_id;
    let attempt = store.begin_harness_review(id, 0, &policy, 15, 120).unwrap();
    assert!(matches!(
        store.decide_human_at_revision(
            id,
            0,
            human_assessment(ApprovalDecision::Approve),
            &policy,
            120
        ),
        Err(ApprovalError::Conflict)
    ));
    store
        .decide_human_at_revision(
            id,
            attempt.revision,
            human_assessment(ApprovalDecision::Deny),
            &policy,
            121,
        )
        .unwrap();
    assert!(
        store
            .assess_attempt(
                &attempt,
                assessment(id, ApprovalDecision::Approve),
                reviewer(),
                &policy,
                122
            )
            .is_err()
    );
    assert!(
        store
            .fail_review_attempt(&attempt, "late error".into(), &policy, 122)
            .is_err()
    );
    assert_eq!(store.get(id).unwrap().state, ApprovalState::Denied);
}

#[test]
fn restart_preserves_human_deadlines_and_does_not_replay_inflight_reviews() {
    let dir = TestDir::new("fallback-restart");
    let policy = fallback_policy();
    let mut store = dir.open(100);
    let waiting = store
        .request(operation("waiting"), policy.clone(), 100)
        .unwrap();
    drop(store);
    let mut store = dir.open(119);
    assert_eq!(store.get(&waiting.request.request_id).unwrap(), &waiting);
    let attempt = store
        .begin_harness_review(&waiting.request.request_id, 0, &policy, 15, 120)
        .unwrap();
    drop(store);
    let mut store = dir.open(121);
    let recovered = store.get(&attempt.request_id).unwrap().clone();
    assert_eq!(recovered.review_stage, ReviewStage::NeedsHuman);
    assert_eq!(recovered.request.expires_at, 160);
    assert!(
        store
            .assess_attempt(
                &attempt,
                assessment(&attempt.request_id, ApprovalDecision::Approve),
                reviewer(),
                &policy,
                122
            )
            .is_err()
    );
    assert!(
        store
            .begin_harness_review(&attempt.request_id, recovered.revision, &policy, 15, 122)
            .is_err()
    );
    drop(store);
    assert_eq!(dir.open(123).get(&attempt.request_id).unwrap(), &recovered);
}

#[test]
fn review_timer_expiry_and_policy_binding_fail_closed() {
    let dir = TestDir::new("review-timer");
    let mut store = dir.open(100);
    let policy = fallback_policy();
    let requested = store
        .request(operation("timer"), policy.clone(), 100)
        .unwrap();
    let id = &requested.request.request_id;
    let attempt = store.begin_harness_review(id, 0, &policy, 15, 150).unwrap();
    assert_eq!(attempt.deadline, 160);
    assert!(matches!(
        store.expire_review(id, attempt.revision, &policy, 159),
        Err(ApprovalError::ReviewNotDue)
    ));
    assert!(matches!(
        store.expire_review(id, attempt.revision, &policy, 160),
        Err(ApprovalError::Expired)
    ));
    assert_eq!(store.get(id).unwrap().state, ApprovalState::Expired);

    let requested = store
        .request(operation("early-timer"), policy.clone(), 100)
        .unwrap();
    let id = &requested.request.request_id;
    let attempt = store.begin_harness_review(id, 0, &policy, 15, 120).unwrap();
    let mut changed_policy = policy.clone();
    changed_policy.version += 1;
    assert!(matches!(
        store.assess_attempt(
            &attempt,
            assessment(id, ApprovalDecision::Approve),
            reviewer(),
            &changed_policy,
            121
        ),
        Err(ApprovalError::Conflict)
    ));
    let mut forged = attempt.clone();
    forged.harness_id = "other".into();
    assert!(
        store
            .assess_attempt(
                &forged,
                assessment(id, ApprovalDecision::Approve),
                reviewer(),
                &policy,
                121
            )
            .is_err()
    );
    assert_eq!(
        store
            .expire_review(id, attempt.revision, &policy, 135)
            .unwrap()
            .review_stage,
        ReviewStage::NeedsHuman
    );
}

#[test]
fn handoff_requires_durable_storage_and_valid_timeout_configuration() {
    let dir = TestDir::new("review-capacity");
    let mut store = dir.open(100);
    let policy = fallback_policy();
    let requested = store
        .request(operation("capacity"), policy.clone(), 100)
        .unwrap();
    drop(store);
    let size = std::fs::metadata(dir.journal()).unwrap().len();
    let mut store = ApprovalStore::open(
        &dir.path,
        ApprovalStoreConfig {
            max_requests: 10,
            max_journal_bytes: size + 1,
        },
        120,
    )
    .unwrap();
    assert!(matches!(
        store.begin_harness_review(&requested.request.request_id, 0, &policy, 15, 120),
        Err(ApprovalError::Capacity)
    ));
    assert_eq!(
        store.get(&requested.request.request_id).unwrap(),
        &requested
    );
    for (human_wait_secs, review_timeout_secs) in [(0, 1), (20, 0), (20, 40), (u64::MAX, 1)] {
        let invalid = ApprovalPolicy {
            reviewer: ReviewerConfig::HumanThenHarness {
                harness_id: "local".into(),
                human_wait_secs,
                review_timeout_secs,
            },
            ..policy.clone()
        };
        assert!(invalid.validate().is_err());
    }
}

#[test]
fn tracked_reviews_keep_scope_identity_and_human_authorship_checks() {
    let dir = TestDir::new("tracked-scope");
    let mut store = dir.open(100);
    let policy = fallback_policy();
    let mut operation = operation("scope");
    operation.target = "outside-target".into();
    let requested = store.request(operation, policy.clone(), 100).unwrap();
    let id = &requested.request.request_id;
    let attempt = store.begin_harness_review(id, 0, &policy, 15, 120).unwrap();
    let mut other = reviewer();
    other.harness_id = "unselected-reviewer".into();
    assert!(matches!(
        store.assess_attempt(
            &attempt,
            assessment(id, ApprovalDecision::Deny),
            other,
            &policy,
            121
        ),
        Err(ApprovalError::Conflict)
    ));
    assert!(matches!(
        store.assess_attempt(
            &attempt,
            assessment(id, ApprovalDecision::Approve),
            reviewer(),
            &policy,
            121
        ),
        Err(ApprovalError::OutOfScope)
    ));
    assert!(matches!(
        store.decide_human_at_revision(
            id,
            attempt.revision,
            human_assessment(ApprovalDecision::Approve),
            &policy,
            121
        ),
        Err(ApprovalError::OutOfScope)
    ));
    let forged_human = ApprovalAssessment {
        reviewer: AssessmentSource::Harness {
            harness_id: "local".into(),
            session_id: "session".into(),
        },
        ..human_assessment(ApprovalDecision::Deny)
    };
    assert!(
        store
            .decide_human_at_revision(id, attempt.revision, forged_human, &policy, 121)
            .is_err()
    );
    store
        .assess_attempt(
            &attempt,
            assessment(id, ApprovalDecision::Deny),
            reviewer(),
            &policy,
            121,
        )
        .unwrap();
}

#[test]
fn canceled_review_and_late_timer_cannot_replace_a_terminal_human_decision() {
    let dir = TestDir::new("late-review-timer");
    let mut store = dir.open(100);
    let policy = fallback_policy();
    for canceled in [false, true] {
        let requested = store
            .request(
                operation(if canceled { "cancel" } else { "approve" }),
                policy.clone(),
                100,
            )
            .unwrap();
        let id = &requested.request.request_id;
        let attempt = store.begin_harness_review(id, 0, &policy, 15, 120).unwrap();
        if canceled {
            store.cancel(id, "operator canceled".into(), 121).unwrap();
        } else {
            store
                .decide_human_at_revision(
                    id,
                    attempt.revision,
                    human_assessment(ApprovalDecision::Approve),
                    &policy,
                    121,
                )
                .unwrap();
        }
        let decided = store.get(id).unwrap().clone();
        assert!(
            store
                .expire_review(id, attempt.revision, &policy, 135)
                .is_err()
        );
        assert!(
            store
                .fail_review_attempt(&attempt, "late transport error".into(), &policy, 135)
                .is_err()
        );
        assert_eq!(store.get(id).unwrap(), &decided);
    }
}

#[test]
fn approved_operation_executes_once_and_preserves_exact_binding() {
    let dir = TestDir::new("once");
    let mut store = dir.open(100);
    let op = operation("op-1");
    let id = approved(&mut store, &op);
    let mut changed = op.clone();
    changed.action["replacement"] = json!("different");
    assert!(matches!(
        store.consume(&id, &changed, &policy(), 102),
        Err(ApprovalError::Conflict)
    ));
    changed = op.clone();
    changed.task_revision += 1;
    assert!(matches!(
        store.consume(&id, &changed, &policy(), 102),
        Err(ApprovalError::Conflict)
    ));
    let mut new_policy = policy();
    new_policy.version += 1;
    assert!(matches!(
        store.consume(&id, &op, &new_policy, 102),
        Err(ApprovalError::Conflict)
    ));
    let permit = store.consume(&id, &op, &policy(), 102).unwrap();
    assert_eq!(permit.operation(), &op);
    assert!(matches!(
        store.consume(&id, &op, &policy(), 102),
        Err(ApprovalError::InvalidState(ApprovalState::Executing))
    ));
    let completed = store
        .complete(
            permit,
            ExecutionOutcome::Executed,
            "read back expected content".into(),
            103,
        )
        .unwrap();
    assert_eq!(completed.state, ApprovalState::Executed);
    assert_eq!(store.request(op.clone(), policy(), 104).unwrap(), completed);
    drop(store);
    let mut reopened = dir.open(104);
    assert_eq!(reopened.get(&id).unwrap(), &completed);
    assert!(reopened.consume(&id, &op, &policy(), 105).is_err());
}

#[test]
fn automatic_and_human_review_respect_hard_policy_scope() {
    let dir = TestDir::new("scope");
    let mut store = dir.open(100);
    let mut op = operation("out-of-scope");
    op.target = "other-project".into();
    let id = store.request(op, policy(), 100).unwrap().request.request_id;
    assert!(matches!(
        store.assess(
            &id,
            assessment(&id, ApprovalDecision::Approve),
            reviewer(),
            &policy(),
            101
        ),
        Err(ApprovalError::OutOfScope)
    ));
    assert!(matches!(
        store.decide_human(
            &id,
            ApprovalDecision::Approve,
            "manual review".into(),
            "operator".into(),
            &policy(),
            101
        ),
        Err(ApprovalError::OutOfScope)
    ));
    assert_eq!(store.get(&id).unwrap().state, ApprovalState::Pending);
    store
        .decide_human(
            &id,
            ApprovalDecision::Deny,
            "outside configured target".into(),
            "operator".into(),
            &policy(),
            101,
        )
        .unwrap();
    assert_eq!(store.get(&id).unwrap().state, ApprovalState::Denied);
}

#[test]
fn escalation_and_reviewer_failure_require_explicit_human_decision() {
    let dir = TestDir::new("escalation");
    let mut store = dir.open(100);
    let op = operation("escalate");
    let id = store
        .request(op.clone(), policy(), 100)
        .unwrap()
        .request
        .request_id;
    let mut wrong_reviewer = reviewer();
    wrong_reviewer.harness_id = "other".into();
    assert!(matches!(
        store.assess(
            &id,
            assessment(&id, ApprovalDecision::Approve),
            wrong_reviewer,
            &policy(),
            101
        ),
        Err(ApprovalError::Conflict)
    ));
    assert!(matches!(
        store.assess(
            &id,
            assessment("other-request", ApprovalDecision::Approve),
            reviewer(),
            &policy(),
            101
        ),
        Err(ApprovalError::Conflict)
    ));
    store
        .assess(
            &id,
            assessment(&id, ApprovalDecision::Escalate),
            reviewer(),
            &policy(),
            101,
        )
        .unwrap();
    assert_eq!(store.get(&id).unwrap().state, ApprovalState::WaitingHuman);
    assert!(
        store
            .assess(
                &id,
                assessment(&id, ApprovalDecision::Approve),
                reviewer(),
                &policy(),
                102
            )
            .is_err()
    );
    store
        .decide_human(
            &id,
            ApprovalDecision::Approve,
            "reviewed exact patch".into(),
            "operator".into(),
            &policy(),
            102,
        )
        .unwrap();
    assert_eq!(store.get(&id).unwrap().state, ApprovalState::Approved);
    let fail = store
        .request(operation("failed-review"), policy(), 100)
        .unwrap()
        .request
        .request_id;
    let failed = store
        .mark_waiting_human(&fail, "reviewer timeout".into(), 101)
        .unwrap();
    assert!(failed.assessment.is_none());
    assert_eq!(failed.state, ApprovalState::WaitingHuman);
    assert!(
        store
            .assess(
                &fail,
                assessment(&fail, ApprovalDecision::Approve),
                reviewer(),
                &policy(),
                102
            )
            .is_err()
    );
}

#[test]
fn direct_human_policy_rejects_model_approval() {
    let dir = TestDir::new("human");
    let mut store = dir.open(100);
    let mut human_policy = policy();
    human_policy.reviewer = ReviewerConfig::Human;
    let id = store
        .request(operation("human"), human_policy.clone(), 100)
        .unwrap()
        .request
        .request_id;
    assert!(
        store
            .assess(
                &id,
                assessment(&id, ApprovalDecision::Approve),
                reviewer(),
                &human_policy,
                101
            )
            .is_err()
    );
    let result = store
        .decide_human(
            &id,
            ApprovalDecision::Approve,
            "reviewed by operator".into(),
            "operator".into(),
            &human_policy,
            101,
        )
        .unwrap();
    assert_eq!(result.state, ApprovalState::Approved);
}

#[test]
fn reviewer_timeout_at_expiry_persists_expired_instead_of_waiting_for_human() {
    let dir = TestDir::new("reviewer-expiry");
    let mut store = dir.open(100);
    let id = store
        .request(operation("expired-review"), policy(), 100)
        .unwrap()
        .request
        .request_id;
    assert!(matches!(
        store.mark_waiting_human(&id, "reviewer timeout".into(), 160),
        Err(ApprovalError::Expired)
    ));
    assert_eq!(store.get(&id).unwrap().state, ApprovalState::Expired);
    drop(store);
    let mut reopened = dir.open(161);
    assert_eq!(reopened.get(&id).unwrap().state, ApprovalState::Expired);
    assert!(matches!(
        reopened.decide_human(
            &id,
            ApprovalDecision::Approve,
            "late decision".into(),
            "operator".into(),
            &policy(),
            161
        ),
        Err(ApprovalError::Expired)
    ));
}

#[test]
fn hard_linked_journal_and_writer_lock_are_rejected_before_mutation() {
    for name in ["approvals.jsonl", "approvals.lock"] {
        let dir = TestDir::new("state-hardlink");
        let original = dir.path.join("other-file");
        std::fs::write(&original, b"must not be modified").unwrap();
        std::fs::hard_link(&original, dir.path.join(name)).unwrap();
        assert!(matches!(
            ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 100),
            Err(ApprovalError::Invalid(_))
        ));
        assert_eq!(std::fs::read(original).unwrap(), b"must not be modified");
    }
}

#[cfg(unix)]
#[test]
fn linked_data_directory_ancestor_is_rejected() {
    let dir = TestDir::new("state-symlink");
    let actual = dir.path.join("actual");
    std::fs::create_dir(&actual).unwrap();
    let alias = dir.path.join("alias");
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    assert!(matches!(
        ApprovalStore::open(alias.join("new-store"), ApprovalStoreConfig::default(), 100),
        Err(ApprovalError::Invalid(_))
    ));
    assert!(!actual.join("new-store").exists());
}

#[test]
fn expired_revoked_and_canceled_approvals_cannot_be_revived() {
    let dir = TestDir::new("terminal");
    let mut store = dir.open(100);
    let op = operation("expired");
    let id = approved(&mut store, &op);
    assert!(matches!(
        store.consume(&id, &op, &policy(), 160),
        Err(ApprovalError::Expired)
    ));
    assert_eq!(store.get(&id).unwrap().state, ApprovalState::Expired);
    for (name, cancel) in [("revoked", false), ("canceled", true)] {
        let op = operation(name);
        let id = approved(&mut store, &op);
        if cancel {
            store.cancel(&id, "user canceled".into(), 102).unwrap();
        } else {
            store
                .revoke(&id, "authorization revoked".into(), 102)
                .unwrap();
        }
        assert!(store.consume(&id, &op, &policy(), 103).is_err());
        assert!(
            store
                .assess(
                    &id,
                    assessment(&id, ApprovalDecision::Approve),
                    reviewer(),
                    &policy(),
                    103
                )
                .is_err()
        );
        assert!(
            store
                .decide_human(
                    &id,
                    ApprovalDecision::Approve,
                    "late approval".into(),
                    "operator".into(),
                    &policy(),
                    103
                )
                .is_err()
        );
    }
    let pending = store
        .request(operation("late"), policy(), 100)
        .unwrap()
        .request
        .request_id;
    assert!(matches!(
        store.assess(
            &pending,
            assessment(&pending, ApprovalDecision::Approve),
            reviewer(),
            &policy(),
            160
        ),
        Err(ApprovalError::Expired)
    ));
}

#[test]
fn restart_preserves_approvals_without_dispatch_and_blocks_unknown_targets() {
    let dir = TestDir::new("restart");
    let mut store = dir.open(100);
    let op = operation("interrupted");
    let id = approved(&mut store, &op);
    let waiting_op = operation("waiting");
    let waiting = approved(&mut store, &waiting_op);
    let _permit = store.consume(&id, &op, &policy(), 102).unwrap();
    drop(store);
    let mut reopened = dir.open(103);
    assert_eq!(reopened.get(&id).unwrap().state, ApprovalState::Unknown);
    assert_eq!(
        reopened.get(&waiting).unwrap().state,
        ApprovalState::Approved
    );
    assert!(matches!(
        reopened.consume(&waiting, &waiting_op, &policy(), 104),
        Err(ApprovalError::TargetBusy)
    ));
    assert!(reopened.consume(&id, &op, &policy(), 104).is_err());
    reopened
        .reconcile_unknown(
            &id,
            ExecutionOutcome::Executed,
            "old executor stopped; content independently verified".into(),
            "operator".into(),
            104,
        )
        .unwrap();
    assert!(reopened.consume(&id, &op, &policy(), 105).is_err());
    let permit = reopened
        .consume(&waiting, &waiting_op, &policy(), 105)
        .unwrap();
    reopened
        .complete(
            permit,
            ExecutionOutcome::Failed,
            "precondition mismatch; no write performed".into(),
            106,
        )
        .unwrap();
}

#[test]
fn cancel_during_execution_keeps_target_blocked_and_rejects_late_completion() {
    let dir = TestDir::new("cancel-executing");
    let mut store = dir.open(100);
    let op = operation("executing");
    let id = approved(&mut store, &op);
    let other_op = operation("other");
    let other = approved(&mut store, &other_op);
    let permit = store.consume(&id, &op, &policy(), 102).unwrap();
    store
        .cancel(&id, "executor cancellation requested".into(), 103)
        .unwrap();
    assert_eq!(store.get(&id).unwrap().state, ApprovalState::Unknown);
    assert!(
        store
            .complete(
                permit,
                ExecutionOutcome::Executed,
                "late receipt".into(),
                104
            )
            .is_err()
    );
    assert!(matches!(
        store.consume(&other, &other_op, &policy(), 104),
        Err(ApprovalError::TargetBusy)
    ));
    assert!(
        store
            .reconcile_unknown(
                &id,
                ExecutionOutcome::Unknown,
                "still unsure".into(),
                "operator".into(),
                104
            )
            .is_err()
    );
}

#[test]
fn journal_capacity_fails_closed_before_execution_intent() {
    let dir = TestDir::new("capacity");
    let mut store = dir.open(100);
    let op = operation("bounded");
    let id = approved(&mut store, &op);
    drop(store);
    let current = std::fs::metadata(dir.journal()).unwrap().len();
    let mut bounded = ApprovalStore::open(
        &dir.path,
        ApprovalStoreConfig {
            max_requests: 10,
            max_journal_bytes: current + 1,
        },
        102,
    )
    .unwrap();
    assert!(matches!(
        bounded.consume(&id, &op, &policy(), 102),
        Err(ApprovalError::Capacity)
    ));
    assert_eq!(bounded.get(&id).unwrap().state, ApprovalState::Approved);
    assert_eq!(std::fs::metadata(dir.journal()).unwrap().len(), current);
}

#[test]
fn failed_completion_record_recovers_unknown_without_repeating_work() {
    let dir = TestDir::new("completion-capacity");
    let mut store = dir.open(100);
    let op = operation("unconfirmed-completion");
    let id = approved(&mut store, &op);
    drop(store);
    let current = std::fs::metadata(dir.journal()).unwrap().len();
    let mut bounded = ApprovalStore::open(
        &dir.path,
        ApprovalStoreConfig {
            max_requests: 10,
            max_journal_bytes: current + 512,
        },
        102,
    )
    .unwrap();
    let permit = bounded.consume(&id, &op, &policy(), 102).unwrap();
    assert!(matches!(
        bounded.complete(
            permit,
            ExecutionOutcome::Executed,
            "verified ".repeat(250),
            103
        ),
        Err(ApprovalError::Capacity)
    ));
    assert_eq!(bounded.get(&id).unwrap().state, ApprovalState::Executing);
    drop(bounded);
    let mut reopened = dir.open(104);
    assert_eq!(reopened.get(&id).unwrap().state, ApprovalState::Unknown);
    assert!(reopened.consume(&id, &op, &policy(), 105).is_err());
}

#[test]
fn exclusive_lock_corrupt_records_and_partial_intents_fail_closed() {
    let dir = TestDir::new("corruption");
    let mut store = dir.open(100);
    assert!(matches!(
        ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 100),
        Err(ApprovalError::Locked(_))
    ));
    let op = operation("corrupt");
    approved(&mut store, &op);
    drop(store);
    let valid = std::fs::read(dir.journal()).unwrap();
    OpenOptions::new()
        .append(true)
        .open(dir.journal())
        .unwrap()
        .write_all(b"{\"format\":1")
        .unwrap();
    assert!(matches!(
        ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 102),
        Err(ApprovalError::Corrupt(_))
    ));
    std::fs::write(dir.journal(), &valid).unwrap();
    let mut lines: Vec<serde_json::Value> = String::from_utf8(valid)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    lines[1]["event"]["change"]["assessment"]["reviewer"]["harness_id"] = json!("forged");
    let altered = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    std::fs::write(dir.journal(), altered).unwrap();
    assert!(matches!(
        ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 102),
        Err(ApprovalError::Corrupt(_))
    ));
}

#[test]
fn model_cannot_supply_reviewer_identity_and_operation_ids_cannot_change() {
    assert!(
        serde_json::from_value::<ModelAssessment>(json!({
            "request_id":"x", "decision":"approve", "reason":"safe", "reviewer":"human"
        }))
        .is_err()
    );
    let dir = TestDir::new("duplicates");
    let mut store = dir.open(100);
    let op = operation("same");
    let original = store.request(op.clone(), policy(), 100).unwrap();
    assert_eq!(original, store.request(op.clone(), policy(), 101).unwrap());
    let mut different = op;
    different.action["expected"] = json!("different baseline");
    assert!(matches!(
        store.request(different, policy(), 101),
        Err(ApprovalError::Conflict)
    ));
    assert_eq!(store.list().len(), 1);
}
