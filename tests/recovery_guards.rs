use super::*;
use recuvora_core::recovery::knowledge::{KnowledgeFuture, KnowledgeProposal, KnowledgeSource};

struct EpisodeGuard(Mutex<IncidentReadiness>);
impl IncidentGuard for EpisodeGuard {
    fn with_current(
        &self,
        _: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError> {
        commit(self.0.lock().unwrap().clone())
    }
}

#[tokio::test]
async fn registering_an_episode_requires_current_authority_and_dedup_survives_resolution() {
    let dir = TestDir::new("episode-authority");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery =
        RecoveryService::open_with_clock(&dir.path, config(), backend.clone(), clock).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("authority")).unwrap(),
        ))
        .unwrap();
    let context = problem("current-episode");
    assert!(recovery.submit(context.clone()).is_err());
    let guard = Arc::new(EpisodeGuard(Mutex::new(IncidentReadiness::Unavailable {
        reason: "authoritative observations unavailable".into(),
    })));
    recovery.bind_incident_guard(guard.clone()).unwrap();
    for readiness in [
        IncidentReadiness::Unavailable {
            reason: "stale authority".into(),
        },
        IncidentReadiness::Resolved {
            revision: context.incident_revision,
        },
        IncidentReadiness::Active {
            revision: context.incident_revision + 1,
        },
    ] {
        *guard.0.lock().unwrap() = readiness;
        assert!(recovery.submit(context.clone()).is_err());
        assert!(recovery.tasks().unwrap().is_empty());
    }
    *guard.0.lock().unwrap() = IncidentReadiness::Active {
        revision: context.incident_revision,
    };
    let task = recovery.submit(context.clone()).unwrap();
    assert_eq!(task.episode_count, 1);
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Completed
    );
    *guard.0.lock().unwrap() = IncidentReadiness::Resolved {
        revision: context.incident_revision + 1,
    };
    let duplicate = recovery.submit(context).unwrap();
    assert_eq!(duplicate.id, task.id);
    assert_eq!(duplicate.episode_count, 1);
    assert_eq!(recovery.tasks().unwrap().len(), 1);
    assert!(recovery.submit(problem("unconfirmed-new-episode")).is_err());
    assert_eq!(backend.count("execute"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn abnormal_samples_never_replace_distinct_episode_reuse_policy() {
    let dir = TestDir::new("episode-policy");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let mut cfg = config();
    cfg.minimum_script_occurrences = 2;
    let recovery = open(&dir.path, cfg.clone(), backend.clone(), clock.clone()).unwrap();
    let mut sampled = problem("single-episode");
    sampled.occurrences = 100_000;
    let first = recovery.submit(sampled.clone()).unwrap();
    assert_eq!(first.episode_count, 1);
    let completed = drive(&recovery, &first.id).await;
    assert_eq!(completed.problem.occurrences, 100_000);
    assert_eq!(completed.episode_count, 1);
    assert_eq!(recovery.submit(sampled).unwrap().episode_count, 1);
    let query = KnowledgeQuery {
        conditions: BTreeMap::from([
            ("runtime_version".into(), "1".into()),
            ("fault".into(), "not-ready".into()),
            ("fault_fingerprint".into(), "readiness-rule".into()),
            ("platform".into(), "linux".into()),
        ]),
        keywords: vec!["readiness".into()],
        limit: 4,
    };
    assert!(!recovery.knowledge(&query).unwrap()[0].candidate.reusable);
    recovery.shutdown().await.unwrap();
    drop(recovery);
    let recovery = open(&dir.path, cfg, backend.clone(), clock).unwrap();
    let second = recovery.submit(problem("second-episode")).unwrap();
    assert_eq!(second.episode_count, 2);
    assert!(!drive(&recovery, &second.id).await.reused_script);
    assert_eq!(backend.count("diagnose"), 2);
    let third = recovery.submit(problem("third-episode")).unwrap();
    assert_eq!(third.episode_count, 3);
    assert!(drive(&recovery, &third.id).await.reused_script);
    assert_eq!(backend.count("diagnose"), 2);
    recovery.shutdown().await.unwrap();
}

struct Source {
    wrong_identity: bool,
}
impl KnowledgeSource for Source {
    fn identity(&self) -> &str {
        "external-source"
    }
    fn query(&self, query: KnowledgeQuery, _: Cancellation) -> KnowledgeFuture<'_> {
        Box::pin(async move {
            Ok(vec![KnowledgeProposal {
                source_id: if self.wrong_identity {
                    "impostor"
                } else {
                    "external-source"
                }
                .into(),
                candidate: KnowledgeCandidate {
                    id: "external-case".into(),
                    incident_id: "prior-incident".into(),
                    summary: "provider observation suggests a bounded proposal".into(),
                    keywords: query.keywords,
                    conditions: query.conditions,
                    script: ScriptArtifact {
                        id: "external-script".into(),
                        version: 1,
                        language: "sh".into(),
                        platform: "linux".into(),
                        source: "exit 1".into(),
                        preconditions: facts(),
                        generated_by_harness: "external-producer".into(),
                        generated_in_session: "external-session".into(),
                    },
                    reusable: true,
                    evidence_refs: vec!["provider-observation:external".into()],
                    created_at_ms: 1,
                },
                evidence_refs: vec!["provider-observation:source".into()],
            }])
        })
    }
}

struct ChangingSource(std::sync::atomic::AtomicBool);
impl KnowledgeSource for ChangingSource {
    fn identity(&self) -> &str {
        if self.0.load(Ordering::SeqCst) {
            "impostor"
        } else {
            "external-source"
        }
    }
    fn query(&self, query: KnowledgeQuery, cancellation: Cancellation) -> KnowledgeFuture<'_> {
        self.0.store(true, Ordering::SeqCst);
        Box::pin(async move {
            Source {
                wrong_identity: true,
            }
            .query(query, cancellation)
            .await
        })
    }
}

struct UnavailableSource;
impl KnowledgeSource for UnavailableSource {
    fn identity(&self) -> &str {
        "external-source"
    }
    fn query(&self, _: KnowledgeQuery, _: Cancellation) -> KnowledgeFuture<'_> {
        Box::pin(async {
            Err(
                recuvora_core::recovery::knowledge::KnowledgeError::Unavailable(
                    "provider detail ".repeat(20_000),
                ),
            )
        })
    }
}

#[tokio::test]
async fn oversized_external_error_still_falls_back_to_local_diagnosis() {
    let dir = TestDir::new("external-source-error-bound");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery = open(&dir.path, config(), backend.clone(), clock).unwrap();
    recovery
        .bind_knowledge_source(Arc::new(UnavailableSource))
        .unwrap();
    let task = recovery.submit(problem("external-source-error")).unwrap();
    let completed = drive(&recovery, &task.id).await;
    assert_eq!(completed.stage, RecoveryStage::Completed);
    assert!(completed.note.as_ref().is_none_or(|note| note.len() < 1024));
    assert_eq!(backend.state.lock().unwrap().knowledge_seen, vec![0]);
    assert_eq!(backend.count("diagnose"), 1);
    assert_eq!(backend.count("review"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn external_source_identity_is_fixed_at_binding_even_if_provider_changes_it() {
    let dir = TestDir::new("external-source-binding");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery = open(&dir.path, config(), backend.clone(), clock).unwrap();
    recovery
        .bind_knowledge_source(Arc::new(ChangingSource(
            std::sync::atomic::AtomicBool::new(false),
        )))
        .unwrap();
    let task = recovery.submit(problem("source-binding")).unwrap();
    assert_eq!(
        drive(&recovery, &task.id).await.stage,
        RecoveryStage::Completed
    );
    assert_eq!(backend.state.lock().unwrap().knowledge_seen, vec![0]);
    assert_eq!(backend.count("review"), 1);
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn external_knowledge_is_read_only_diagnostic_evidence_and_never_a_reuse_attestation() {
    for wrong_identity in [false, true] {
        let dir = TestDir::new("external-knowledge-recovery");
        let clock = Arc::new(Clock::new());
        let backend = Arc::new(Backend::new(clock.clone()));
        let recovery = open(&dir.path, config(), backend.clone(), clock).unwrap();
        recovery
            .bind_knowledge_source(Arc::new(Source { wrong_identity }))
            .unwrap();
        assert!(
            recovery
                .bind_knowledge_source(Arc::new(Source { wrong_identity }))
                .is_err()
        );
        let first = recovery.submit(problem("external-proposal")).unwrap();
        assert_eq!(
            drive(&recovery, &first.id).await.stage,
            RecoveryStage::Completed
        );
        assert_eq!(
            backend.state.lock().unwrap().knowledge_seen,
            vec![usize::from(!wrong_identity)]
        );
        assert_eq!(backend.count("diagnose"), 1);
        assert_eq!(backend.count("review"), 1);
        recovery.shutdown().await.unwrap();
        drop(recovery);
        let knowledge = KnowledgeStore::open(
            dir.path.join("knowledge.jsonl"),
            KnowledgeStoreConfig::default(),
        )
        .unwrap();
        assert!(knowledge.get("external-case").is_none());
    }
}

#[tokio::test]
async fn recovery_target_claim_survives_unknown_shutdown_across_different_state_directories() {
    let dir = TestDir::new("shared-target-owner");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let authority = Arc::new(FileTargetOwnership::open(dir.path.join("authority")).unwrap());
    let first_dir = dir.path.join("first");
    let second_dir = dir.path.join("second");
    let first =
        RecoveryService::open_with_clock(&first_dir, config(), backend.clone(), clock.clone())
            .unwrap();
    assert!(first.submit(problem("missing-owner")).is_err());
    first.bind_incident_guard(backend.clone()).unwrap();
    first.bind_target_ownership(authority.clone()).unwrap();
    let second =
        RecoveryService::open_with_clock(&second_dir, config(), backend.clone(), clock.clone())
            .unwrap();
    assert!(matches!(
        second.bind_target_ownership(authority.clone()),
        Err(RecoveryError::Busy)
    ));
    backend
        .state
        .lock()
        .unwrap()
        .outcomes
        .push_back((ScriptOutcome::Unknown, false));
    let task = first.submit(problem("unknown-owner")).unwrap();
    let unknown = drive(&first, &task.id).await;
    first.shutdown().await.unwrap();
    drop(first);
    assert!(matches!(
        second.bind_target_ownership(authority.clone()),
        Err(RecoveryError::Busy)
    ));
    let restarted =
        RecoveryService::open_with_clock(&first_dir, config(), backend.clone(), clock.clone())
            .unwrap();
    restarted.bind_incident_guard(backend.clone()).unwrap();
    restarted.bind_target_ownership(authority.clone()).unwrap();
    let current = restarted.query(&unknown.id).unwrap().unwrap();
    restarted
        .check_result(
            &current.id,
            current.revision,
            execution_evidence(&current, CheckedExecution::Executed, &clock),
            business_evidence(&current, &clock),
            "trusted-operator".into(),
        )
        .unwrap();
    assert_eq!(
        drive(&restarted, &current.id).await.stage,
        RecoveryStage::Completed
    );
    restarted.shutdown().await.unwrap();
    drop(restarted);
    second.bind_target_ownership(authority).unwrap();
    second.bind_incident_guard(backend).unwrap();
    let task = second
        .submit(problem("new-owner-after-settlement"))
        .unwrap();
    assert_eq!(
        drive(&second, &task.id).await.stage,
        RecoveryStage::Completed
    );
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn operational_projection_is_bounded_and_keeps_samples_separate_from_episodes() {
    let dir = TestDir::new("recovery-projection");
    let clock = Arc::new(Clock::new());
    let backend = Arc::new(Backend::new(clock.clone()));
    let recovery = open(&dir.path, config(), backend, clock).unwrap();
    let mut context = problem("projection-incident");
    context.occurrences = 42;
    let task = recovery.submit(context).unwrap();
    let query = RecoveryQuery {
        stages: vec![RecoveryStage::Queued],
        after_id: None,
        limit: 1,
    };
    let projected = recovery.inspect_tasks(&query).unwrap();
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].sample_count, 42);
    assert_eq!(projected[0].episode_count, 1);
    let json = serde_json::to_value(&projected[0]).unwrap();
    assert!(json.get("plan").is_none());
    assert!(json.get("script").is_none());
    let overview = recovery.overview().unwrap();
    assert_eq!(overview.task_count, 1);
    assert_eq!(overview.stages[&RecoveryStage::Queued], 1);
    assert!(overview.journal_bytes > 0);
    assert!(
        recovery
            .inspect_tasks(&RecoveryQuery {
                limit: 0,
                ..query.clone()
            })
            .is_err()
    );
    assert!(
        recovery
            .inspect_tasks(&RecoveryQuery {
                stages: vec![RecoveryStage::Queued, RecoveryStage::Queued],
                ..query.clone()
            })
            .is_err()
    );
    assert!(
        recovery
            .inspect_tasks(&RecoveryQuery {
                after_id: Some(task.id),
                ..query
            })
            .unwrap()
            .is_empty()
    );
    recovery.shutdown().await.unwrap();
}
