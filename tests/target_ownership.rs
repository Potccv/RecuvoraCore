use recuvora_core::recovery::workflow::{
    CanonicalTarget, FileTargetOwnership, RecoveryError, TargetOwnership,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "workflow_support.rs"]
mod support;
use support::TestDir;

fn target() -> CanonicalTarget {
    CanonicalTarget::new("target-a").unwrap()
}

fn aliased_directory(root: &Path) -> PathBuf {
    // Windows PathBuf::join normalizes traversal for verbatim paths. Preserve
    // the caller's original spelling so validation sees the forbidden alias.
    let separator = std::path::MAIN_SEPARATOR;
    PathBuf::from(format!(
        "{}{separator}authority{separator}..{separator}alias",
        root.display()
    ))
}

#[test]
fn canonical_target_rejects_route_fields_and_spelling_aliases() {
    for id in [
        "",
        "TARGET-A",
        "target-a.",
        "..",
        "./target-a",
        "target/a",
        "https://target-a",
        "target:a",
    ] {
        assert!(CanonicalTarget::new(id).is_err(), "{id:?}");
    }
    assert_eq!(target().as_str(), "target-a");
}

#[test]
fn shared_authority_blocks_other_directories_and_retains_claim_after_drop() {
    let dir = TestDir::new("target-ownership-restart");
    let authority_dir = dir.path.join("authority");
    let first = FileTargetOwnership::open(&authority_dir).unwrap();
    let second = FileTargetOwnership::open(&authority_dir).unwrap();
    let owner_a = dir.path.join("state-a");
    let owner_b = dir.path.join("state-b");
    let lease = first.acquire(&target(), &owner_a).unwrap();
    lease.validate().unwrap();
    assert_eq!(lease.target(), &target());
    assert_eq!(lease.recovery_directory(), owner_a.canonicalize().unwrap());
    assert!(matches!(
        second.acquire(&target(), &owner_a),
        Err(RecoveryError::Busy)
    ));
    assert!(matches!(
        second.acquire(&target(), &owner_b),
        Err(RecoveryError::Busy)
    ));
    let other = CanonicalTarget::new("target-b").unwrap();
    let mut independent = second.acquire(&other, &owner_b).unwrap();
    independent.release().unwrap();
    let length = std::fs::metadata(authority_dir.join("target-target-a.jsonl"))
        .unwrap()
        .len();
    drop(lease);
    // Loss of the process capability is not evidence of stopped execution.
    assert!(matches!(
        second.acquire(&target(), &owner_b),
        Err(RecoveryError::Busy)
    ));
    let mut recovered = second.acquire(&target(), &owner_a).unwrap();
    recovered.validate().unwrap();
    assert_eq!(
        std::fs::metadata(authority_dir.join("target-target-a.jsonl"))
            .unwrap()
            .len(),
        length
    );
    recovered.release().unwrap();
    assert!(recovered.validate().is_err());
    assert!(recovered.release().is_err());
    // Explicit release unlocks immediately, even before the released value drops.
    let mut transferred = first.acquire(&target(), &owner_b).unwrap();
    transferred.validate().unwrap();
    transferred.release().unwrap();
}

#[test]
fn persisted_owner_binds_recovery_lock_object_as_well_as_directory() {
    let dir = TestDir::new("target-ownership-store-identity");
    let authority = FileTargetOwnership::open(dir.path.join("authority")).unwrap();
    let owner = dir.path.join("state");
    let lease = authority.acquire(&target(), &owner).unwrap();
    drop(lease);
    std::fs::rename(owner.join("recovery.lock"), owner.join("previous.lock")).unwrap();
    assert!(matches!(
        authority.acquire(&target(), &owner),
        Err(RecoveryError::Busy)
    ));
    // Reinstating the original identity restores only the original owner's claim.
    std::fs::remove_file(owner.join("recovery.lock")).unwrap();
    std::fs::rename(owner.join("previous.lock"), owner.join("recovery.lock")).unwrap();
    let mut recovered = authority.acquire(&target(), &owner).unwrap();
    recovered.release().unwrap();
}

#[test]
fn ownership_paths_reject_aliases_hard_links_and_live_replacement() {
    let dir = TestDir::new("target-ownership-paths");
    assert!(FileTargetOwnership::open(aliased_directory(&dir.path)).is_err());
    assert!(
        FileTargetOwnership::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("ownership")).is_err()
    );
    let authority_dir = dir.path.join("authority");
    let authority = FileTargetOwnership::open(&authority_dir).unwrap();
    let owner = dir.path.join("state");
    assert!(
        authority
            .acquire(&target(), &aliased_directory(&dir.path))
            .is_err()
    );
    let original = dir.path.join("original.jsonl");
    std::fs::write(&original, "").unwrap();
    let journal = authority_dir.join("target-target-a.jsonl");
    assert!(
        !journal.exists(),
        "invalid directory must not create ownership state"
    );
    std::fs::hard_link(&original, &journal).unwrap();
    assert!(matches!(
        authority.acquire(&target(), &owner),
        Err(RecoveryError::Invalid(_))
    ));
    std::fs::remove_file(&journal).unwrap();
    let mut lease = authority.acquire(&target(), &owner).unwrap();
    let moved = authority_dir.join("previous.jsonl");
    let replacement = std::fs::rename(&journal, &moved);
    #[cfg(windows)]
    {
        assert!(
            replacement.is_err(),
            "live ownership journal must remain pinned"
        );
        lease.validate().unwrap();
        lease.release().unwrap();
    }
    #[cfg(not(windows))]
    {
        replacement.unwrap();
        std::fs::write(&journal, "").unwrap();
        assert!(lease.validate().is_err());
        assert!(lease.release().is_err());
    }
}

#[test]
fn corrupt_or_partial_ownership_events_fail_closed_without_truncation() {
    for suffix in [b"{\"format\":".as_slice(), b"duplicate".as_slice()] {
        let dir = TestDir::new("target-ownership-corrupt");
        let authority_dir = dir.path.join("authority");
        let authority = FileTargetOwnership::open(&authority_dir).unwrap();
        let owner = dir.path.join("state");
        let lease = authority.acquire(&target(), &owner).unwrap();
        drop(lease);
        let journal = authority_dir.join("target-target-a.jsonl");
        let suffix = if suffix == b"duplicate" {
            let mut entry: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
            entry["sequence"] = 2.into();
            let mut bytes = serde_json::to_vec(&entry).unwrap();
            bytes.push(b'\n');
            bytes
        } else {
            suffix.to_vec()
        };
        OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(&suffix)
            .unwrap();
        let bytes = std::fs::read(&journal).unwrap();
        assert!(matches!(
            authority.acquire(&target(), &owner),
            Err(RecoveryError::Corrupt(_))
        ));
        assert_eq!(std::fs::read(&journal).unwrap(), bytes);
    }
}

struct ChildProcess(Child);
impl Drop for ChildProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "child ownership handshake timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ownership_child_claims_target() {
    let Some(root) = std::env::var_os("RECUVORA_OWNERSHIP_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let authority = FileTargetOwnership::open(root.join("authority")).unwrap();
    let _lease = authority.acquire(&target(), &root.join("state-a")).unwrap();
    std::fs::write(root.join("child-ready"), "ready").unwrap();
    wait_for(&root.join("child-exit"));
    // Simulate process loss without an orderly release or Rust destructors.
    std::process::exit(0);
}

#[test]
fn another_process_cannot_take_over_a_live_or_crashed_owner() {
    let dir = TestDir::new("target-ownership-process");
    let mut child = ChildProcess(
        Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("ownership_child_claims_target")
            .arg("--nocapture")
            .env("RECUVORA_OWNERSHIP_CHILD_ROOT", &dir.path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(&dir.path.join("child-ready"));
    let authority = FileTargetOwnership::open(dir.path.join("authority")).unwrap();
    let owner_a = dir.path.join("state-a");
    let owner_b = dir.path.join("state-b");
    assert!(matches!(
        authority.acquire(&target(), &owner_b),
        Err(RecoveryError::Busy)
    ));
    std::fs::write(dir.path.join("child-exit"), "exit").unwrap();
    assert!(child.0.wait().unwrap().success());
    assert!(matches!(
        authority.acquire(&target(), &owner_b),
        Err(RecoveryError::Busy)
    ));
    let mut recovered = authority.acquire(&target(), &owner_a).unwrap();
    recovered.release().unwrap();
    let mut transferred = authority.acquire(&target(), &owner_b).unwrap();
    transferred.release().unwrap();
}
