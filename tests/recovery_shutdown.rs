use super::*;
use std::fs::OpenOptions;
use std::io::Write;

#[derive(Clone, Copy, Debug)]
enum Damage {
    TaskJournalLength,
    ApprovalJournalLength,
    ApprovalJournalIdentity,
    ApprovalLockIdentity,
}

fn damage(state_dir: &std::path::Path, mode: Damage) -> std::io::Result<()> {
    match mode {
        Damage::TaskJournalLength => {
            let path = state_dir.join("recovery.jsonl");
            let length = std::fs::metadata(&path)?.len();
            OpenOptions::new()
                .write(true)
                .open(path)?
                .set_len(length + 1)
        }
        Damage::ApprovalJournalLength => OpenOptions::new()
            .append(true)
            .open(state_dir.join("approvals").join("approvals.jsonl"))?
            .write_all(b"\n"),
        Damage::ApprovalJournalIdentity | Damage::ApprovalLockIdentity => {
            let filename = match mode {
                Damage::ApprovalJournalIdentity => "approvals.jsonl",
                _ => "approvals.lock",
            };
            let path = state_dir.join("approvals").join(filename);
            std::fs::rename(
                &path,
                state_dir
                    .join("approvals")
                    .join(format!("previous-{filename}")),
            )?;
            std::fs::write(path, "")
        }
    }
}

#[tokio::test]
async fn shutdown_does_not_release_target_from_stale_terminal_task_or_approval_snapshots() {
    for mode in [
        Damage::TaskJournalLength,
        Damage::ApprovalJournalLength,
        Damage::ApprovalJournalIdentity,
        Damage::ApprovalLockIdentity,
    ] {
        let dir = TestDir::new("shutdown-log-health");
        let clock = Arc::new(Clock::new());
        let backend = Arc::new(Backend::new(clock.clone()));
        let authority = Arc::new(FileTargetOwnership::open(dir.path.join("authority")).unwrap());
        let first_dir = dir.path.join("first");
        let second_dir = dir.path.join("second");
        let first =
            RecoveryService::open_with_clock(&first_dir, config(), backend.clone(), clock.clone())
                .unwrap();
        first.bind_incident_guard(backend.clone()).unwrap();
        first.bind_target_ownership(authority.clone()).unwrap();
        let task = first.submit(problem("terminal-before-log-damage")).unwrap();
        assert_eq!(
            drive(&first, &task.id).await.stage,
            RecoveryStage::Completed
        );
        assert_eq!(
            first.approval(&task.id).unwrap().unwrap().state,
            ApprovalState::Executed
        );
        let second =
            RecoveryService::open_with_clock(&second_dir, config(), backend.clone(), clock)
                .unwrap();
        if damage(&first_dir, mode).is_ok() {
            assert!(first.shutdown().await.is_err(), "{mode:?}");
            drop(first);
            // Failed close drops the process lock, but must retain the persisted
            // claim even though the previous in-memory task was Completed.
            assert!(
                matches!(
                    second.bind_target_ownership(authority.clone()),
                    Err(RecoveryError::Busy)
                ),
                "{mode:?}"
            );
        } else {
            // A platform may prohibit modifying a file held by a mandatory lock.
            // In that case the intact authority can complete normal shutdown.
            first.shutdown().await.unwrap();
            drop(first);
            second.bind_target_ownership(authority.clone()).unwrap();
        }
        second.shutdown().await.unwrap();
        assert_eq!(backend.count("execute"), 1, "{mode:?}");
    }
}
