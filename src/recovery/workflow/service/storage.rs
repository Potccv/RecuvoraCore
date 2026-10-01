//! Independent bounded workflow journal; never substitutes for approval facts.
use super::*;
use fs2::FileExt;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

use super::storage_paths as paths;

pub(super) const MAX_ENTRY_BYTES: usize = 256 * 1024;

pub(super) struct TaskStore {
    file: File,
    path: PathBuf,
    lock_file: File,
    lock_path: PathBuf,
    _directories: Vec<File>,
    pub tasks: BTreeMap<String, RecoveryTask>,
    sequence: u64,
    bytes: u64,
    max_tasks: usize,
    max_bytes: u64,
    config: RecoveryConfig,
    poisoned: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalEntry {
    pub format: u32,
    pub sequence: u64,
    pub config: RecoveryConfig,
    pub task: RecoveryTask,
}

pub(super) fn validate_dir(dir: &Path) -> Result<(), RecoveryError> {
    if !dir.is_absolute()
        || dir
            .components()
            .any(|value| matches!(value, Component::ParentDir | Component::CurDir))
    {
        return Err(RecoveryError::Invalid(
            "state directory must be absolute without traversal".into(),
        ));
    }
    for ancestor in dir.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(RecoveryError::Invalid("linked state directory".into()));
                    }
                }
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(RecoveryError::Invalid(
                        "linked or non-directory state ancestor".into(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let existing = dir
        .ancestors()
        .find(|path| path.exists())
        .ok_or_else(|| service("no state ancestor"))?;
    let resolved = existing.canonicalize()?;
    match Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize() {
        Ok(source) if resolved.starts_with(&source) => Err(RecoveryError::Invalid(
            "state must be outside library source".into(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn validate_storage_layout(dir: &Path) -> Result<(), RecoveryError> {
    for name in ["pipeline.jsonl", "pipeline.lock"] {
        match std::fs::symlink_metadata(dir.join(name)) {
            Ok(_) => {
                return Err(RecoveryError::Invalid(
                    "state directory contains unsupported storage files".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

impl TaskStore {
    pub fn open(dir: &Path, config: &RecoveryConfig) -> Result<Self, RecoveryError> {
        config.validate()?;
        validate_dir(dir)?;
        validate_storage_layout(dir)?;
        let lock_path = dir.join("recovery.lock");
        let (lock_file, mut directories) = paths::open(&lock_path)?;
        lock_file.try_lock_exclusive()?;
        if lock_file.metadata()?.len() != 0 {
            return Err(RecoveryError::Corrupt(
                "lock file contains unexpected data".into(),
            ));
        }
        let path = dir.join("recovery.jsonl");
        let (file, journal_directories) = paths::open(&path)?;
        file.try_lock_exclusive()?;
        directories.extend(journal_directories);
        let bytes = file.metadata()?.len();
        if bytes > config.max_journal_bytes {
            return Err(RecoveryError::Capacity);
        }
        let mut store = Self {
            file,
            path,
            lock_file,
            lock_path,
            _directories: directories,
            tasks: BTreeMap::new(),
            sequence: 0,
            bytes,
            max_tasks: config.max_tasks,
            max_bytes: config.max_journal_bytes,
            config: config.clone(),
            poisoned: false,
        };
        store.replay(config)?;
        Ok(store)
    }

    fn replay(&mut self, config: &RecoveryConfig) -> Result<(), RecoveryError> {
        let mut reader = BufReader::new(self.file.try_clone()?);
        let mut total = 0u64;
        let mut version_2_seen = false;
        loop {
            let mut line = Vec::new();
            let count = reader
                .by_ref()
                .take((MAX_ENTRY_BYTES + 1) as u64)
                .read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            if count > MAX_ENTRY_BYTES || line.last() != Some(&b'\n') {
                return Err(RecoveryError::Corrupt(
                    "oversized or incomplete entry".into(),
                ));
            }
            total = total
                .checked_add(count as u64)
                .ok_or(RecoveryError::Capacity)?;
            if total > self.max_bytes {
                return Err(RecoveryError::Capacity);
            }
            let mut entry: JournalEntry = serde_json::from_slice(&line)
                .map_err(|error| RecoveryError::Corrupt(error.to_string()))?;
            validate_format(entry.format, &mut version_2_seen)?;
            if entry.sequence
                != self
                    .sequence
                    .checked_add(1)
                    .ok_or(RecoveryError::Capacity)?
                || entry.config != *config
            {
                return Err(RecoveryError::Corrupt(
                    "sequence or trusted configuration changed; explicit migration required".into(),
                ));
            }
            normalize_legacy_episode(&mut entry.task, entry.format, &self.tasks)?;
            self.validate(&entry.task)
                .map_err(|error| RecoveryError::Corrupt(error.to_string()))?;
            self.sequence = entry.sequence;
            self.tasks.insert(entry.task.id.clone(), entry.task);
        }
        if total != self.bytes || self.file.metadata()?.len() != self.bytes {
            return Err(RecoveryError::Corrupt(
                "journal changed during replay".into(),
            ));
        }
        paths::validate_current(&self.path, &self.file)?;
        paths::validate_current(&self.lock_path, &self.lock_file)?;
        Ok(())
    }

    fn validate(&self, task: &RecoveryTask) -> Result<(), RecoveryError> {
        validate_task(&self.tasks, &self.config, task)
    }

    pub(super) fn usage(&self) -> (u64, usize, u64, usize) {
        (self.bytes, self.tasks.len(), self.max_bytes, self.max_tasks)
    }

    pub(super) fn ensure_current(&self) -> Result<(), RecoveryError> {
        if self.poisoned {
            return Err(service("recovery journal unavailable after I/O failure"));
        }
        paths::validate_current(&self.path, &self.file)?;
        paths::validate_current(&self.lock_path, &self.lock_file)?;
        if self.file.metadata()?.len() != self.bytes || self.lock_file.metadata()?.len() != 0 {
            return Err(service(
                "recovery journal or lock length changed externally",
            ));
        }
        Ok(())
    }
    pub fn save(
        &mut self,
        mut task: RecoveryTask,
        config: &RecoveryConfig,
        now: u64,
    ) -> Result<RecoveryTask, RecoveryError> {
        if self.poisoned {
            return Err(service("recovery journal unavailable after I/O failure"));
        }
        if config != &self.config {
            return Err(RecoveryError::Invalid(
                "trusted configuration changed during ownership".into(),
            ));
        }
        if let Some(old) = self.tasks.get(&task.id) {
            if old.revision != task.revision {
                return Err(RecoveryError::Busy);
            }
            task.revision = task
                .revision
                .checked_add(1)
                .ok_or(RecoveryError::Capacity)?;
            task.updated_at_ms = now.max(old.updated_at_ms);
        } else {
            task.revision = 1;
        }
        self.validate(&task)?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(RecoveryError::Capacity)?;
        let mut bytes = serde_json::to_vec(&JournalEntry {
            format: 2,
            sequence,
            config: config.clone(),
            task: task.clone(),
        })?;
        bytes.push(b'\n');
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= self.max_bytes)
            .ok_or(RecoveryError::Capacity)?;
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err(RecoveryError::Capacity);
        }
        // Any identity, length, write or sync error poisons the owner before
        // another mutation can be acknowledged from an uncertain journal.
        self.poisoned = true;
        paths::validate_current(&self.path, &self.file)?;
        paths::validate_current(&self.lock_path, &self.lock_file)?;
        if self.file.metadata()?.len() != self.bytes || self.lock_file.metadata()?.len() != 0 {
            return Err(service(
                "recovery journal or lock length changed externally",
            ));
        }
        self.file.write_all(&bytes)?;
        self.file.sync_data()?;
        self.poisoned = false;
        self.bytes = total;
        self.sequence = sequence;
        self.tasks.insert(task.id.clone(), task.clone());
        Ok(task)
    }
}

fn same_plan(left: Option<&RepairPlan>, right: Option<&RepairPlan>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.summary == right.summary
                && left.script == right.script
                && left.reusable == right.reusable
        }
        _ => false,
    }
}

fn valid_transition(old: &RecoveryStage, new: &RecoveryStage) -> bool {
    use RecoveryStage::*;
    if *new == Unknown {
        return true;
    }
    match old {
        Queued => matches!(new, Diagnosing | AwaitingApproval | Canceled),
        Diagnosing => matches!(new, Diagnosing | AwaitingApproval | Failed | Canceled),
        AwaitingApproval => matches!(new, Executing | Denied | Canceled | Diagnosing | Paused),
        Executing => matches!(new, Verifying | Publishing | Denied | Paused),
        Verifying => matches!(new, Publishing),
        Publishing => matches!(new, Completed | Failed | Diagnosing),
        Unknown => matches!(new, Publishing | Canceled),
        Paused => matches!(new, AwaitingApproval),
        Completed | Failed | Denied | Canceled => false,
    }
}

// Pure validation is shared by live saves, startup replay and offline migration.
struct TaskValidator<'a> {
    tasks: &'a BTreeMap<String, RecoveryTask>,
    config: &'a RecoveryConfig,
    max_tasks: usize,
}

pub(super) fn validate_task(
    tasks: &BTreeMap<String, RecoveryTask>,
    config: &RecoveryConfig,
    task: &RecoveryTask,
) -> Result<(), RecoveryError> {
    TaskValidator {
        tasks,
        config,
        max_tasks: config.max_tasks,
    }
    .validate(task)
}

impl TaskValidator<'_> {
    fn validate(&self, task: &RecoveryTask) -> Result<(), RecoveryError> {
        text(&task.id, 128)?;
        task.problem.validate()?;
        self.validate_shape(task)?;
        if task.updated_at_ms < task.created_at_ms {
            return Err(RecoveryError::Invalid("task time moved backwards".into()));
        }
        if let Some(old) = self.tasks.get(&task.id) {
            if Some(task.revision) != old.revision.checked_add(1)
                || task.problem != old.problem
                || task.episode_count != old.episode_count
                || task.created_at_ms != old.created_at_ms
                || task.updated_at_ms < old.updated_at_ms
            {
                return Err(RecoveryError::Invalid(
                    "task revision or identity conflict".into(),
                ));
            }
            let attaching_approval = old.stage == RecoveryStage::AwaitingApproval
                && task.stage == RecoveryStage::AwaitingApproval
                && old.approval_id.is_none()
                && task.approval_id.is_some();
            if !valid_transition(&old.stage, &task.stage) && !attaching_approval {
                return Err(RecoveryError::Invalid(
                    "invalid recovery stage transition".into(),
                ));
            }
            if task.diagnosis_attempts != old.diagnosis_attempts
                && !(old.stage == RecoveryStage::Diagnosing
                    && task.stage == RecoveryStage::Diagnosing
                    && old.diagnosis_attempts.checked_add(1) == Some(task.diagnosis_attempts))
            {
                return Err(RecoveryError::Invalid(
                    "diagnosis budget transition mismatch".into(),
                ));
            }
            if old.operation.is_some()
                && old.stage != RecoveryStage::Diagnosing
                && task.stage != RecoveryStage::Diagnosing
                && (old.operation != task.operation
                    || (old.approval_id != task.approval_id && !attaching_approval)
                    || !same_plan(old.plan.as_ref(), task.plan.as_ref()))
            {
                return Err(RecoveryError::Invalid(
                    "approved operation or script changed across stages".into(),
                ));
            }
            if attaching_approval
                && (serde_json::to_value(&old.observation)?
                    != serde_json::to_value(&task.observation)?
                    || old.reused_script != task.reused_script
                    || old.knowledge_id != task.knowledge_id
                    || serde_json::to_value(&old.receipt)? != serde_json::to_value(&task.receipt)?
                    || serde_json::to_value(&old.verification)?
                        != serde_json::to_value(&task.verification)?
                    || old.result_check != task.result_check
                    || old.diagnosis_attempts != task.diagnosis_attempts)
            {
                return Err(RecoveryError::Invalid(
                    "approval association changed durable plan evidence".into(),
                ));
            }
            if old.stage == RecoveryStage::Publishing
                && task.stage != RecoveryStage::Unknown
                && (serde_json::to_value(&old.receipt)? != serde_json::to_value(&task.receipt)?
                    || serde_json::to_value(&old.verification)?
                        != serde_json::to_value(&task.verification)?)
            {
                return Err(RecoveryError::Invalid(
                    "publication changed durable execution or verification evidence".into(),
                ));
            }
            if old.result_check != task.result_check
                && task.stage != RecoveryStage::Diagnosing
                && old.stage != RecoveryStage::Diagnosing
                && (old.stage != RecoveryStage::Unknown
                    || task.stage != RecoveryStage::Unknown
                    || old.result_check.as_ref().is_some_and(|previous| {
                        previous.execution.outcome != CheckedExecution::Unknown
                            && task.result_check.as_ref().is_none_or(|next| {
                                previous.execution.outcome != next.execution.outcome
                            })
                    }))
            {
                return Err(RecoveryError::Invalid(
                    "result check facts changed outside explicit unknown recovery".into(),
                ));
            }
        } else if task.revision != 1 || self.tasks.len() >= self.max_tasks {
            return Err(RecoveryError::Capacity);
        } else if task.episode_count
            != self
                .tasks
                .values()
                .filter(|old| {
                    old.problem.target_id == task.problem.target_id
                        && old.problem.fingerprint == task.problem.fingerprint
                        && old.problem.conditions == task.problem.conditions
                })
                .count() as u64
                + 1
            || task.stage != RecoveryStage::Queued
            || task.diagnosis_attempts != 0
            || task.plan.is_some()
            || task.operation.is_some()
            || task.approval_id.is_some()
            || task.observation.is_some()
            || task.receipt.is_some()
            || task.verification.is_some()
            || task.result_check.is_some()
            || task.knowledge_id.is_some()
            || task.reused_script
            || self
                .tasks
                .values()
                .any(|old| old.problem.incident_id == task.problem.incident_id)
        {
            return Err(RecoveryError::Invalid(
                "initial task must be a unique queued incident".into(),
            ));
        }
        Ok(())
    }

    fn validate_shape(&self, task: &RecoveryTask) -> Result<(), RecoveryError> {
        use RecoveryStage::*;
        if task.episode_count == 0
            || task.problem.target_id != self.config.target.target_id
            || task.diagnosis_attempts > self.config.max_diagnoses
        {
            return Err(RecoveryError::Invalid(
                "task target or diagnosis budget differs from configuration".into(),
            ));
        }
        if let Some(note) = &task.note {
            text(note, 64 * 1024)?;
        }
        for id in [&task.approval_id, &task.knowledge_id]
            .into_iter()
            .flatten()
        {
            text(id, 256)?;
        }
        if matches!(
            task.stage,
            AwaitingApproval | Executing | Verifying | Publishing | Completed | Unknown | Paused
        ) && (task.plan.is_none()
            || task.operation.is_none()
            || (task.approval_id.is_none() && task.stage != AwaitingApproval)
            || task.observation.is_none())
        {
            return Err(RecoveryError::Invalid(
                "stage lacks plan, operation, approval or observation".into(),
            ));
        }
        if task.reused_script && (task.plan.is_none() || task.knowledge_id.is_none()) {
            return Err(RecoveryError::Invalid(
                "reused script lacks a knowledge record".into(),
            ));
        }
        if task.approval_id.is_some() != task.operation.is_some()
            && !(task.stage == AwaitingApproval && task.operation.is_some())
        {
            return Err(RecoveryError::Invalid(
                "operation and approval references differ".into(),
            ));
        }
        if let Some(plan) = &task.plan {
            text(&plan.summary, 4096)?;
            let script = &plan.script;
            text(&script.id, 128)?;
            text(&script.source, MAX_SCRIPT_BYTES)?;
            text(&script.generated_by_harness, 128)?;
            text(&script.generated_in_session, 256)?;
            contract::facts(&script.preconditions)?;
            if script.version == 0
                || script.preconditions.is_empty()
                || script.platform != self.config.target.platform
                || !self
                    .config
                    .target
                    .allowed_languages
                    .contains(&script.language)
            {
                return Err(RecoveryError::Invalid(
                    "invalid persisted script contract".into(),
                ));
            }
        }

        if let Some(observation) = &task.observation {
            contract::facts(&observation.facts)?;
            contract::evidence(&observation.evidence_refs)?;
            if observation.target_id != task.problem.target_id
                || observation.observed_at_ms > task.updated_at_ms
            {
                return Err(RecoveryError::Invalid(
                    "persisted observation identity or time mismatch".into(),
                ));
            }
        }
        if let Some(operation) = &task.operation {
            operation.validate()?;
            let plan = task
                .plan
                .as_ref()
                .ok_or_else(|| RecoveryError::Invalid("operation lacks script".into()))?;
            let action = serde_json::json!({"kind":"execute_script","executor_id":self.config.target.executor_id,
                "script":plan.script,"verification_profile":self.config.target.verification_profile,
                "required_facts":self.config.target.required_facts,"timeout_secs":self.config.target.action_timeout_secs,
                "incident_id":task.problem.incident_id,"incident_revision":task.problem.incident_revision});
            if operation.task_id != task.id
                || operation.target != task.problem.target_id
                || operation.task_revision > task.revision
                || operation.action != action
            {
                return Err(RecoveryError::Invalid(
                    "persisted operation does not bind task, policy or script".into(),
                ));
            }
        }
        if let Some(receipt) = &task.receipt {
            contract::evidence(&receipt.evidence_refs)?;
            text(&receipt.summary, 8192)?;
            if receipt.target_id != task.problem.target_id
                || task
                    .operation
                    .as_ref()
                    .is_some_and(|operation| receipt.operation_id != operation.operation_id)
            {
                return Err(RecoveryError::Invalid(
                    "receipt identity differs from task operation".into(),
                ));
            }
        }
        if let Some(verification) = &task.verification {
            contract::evidence(&verification.evidence_refs)?;
            if verification.target_id != task.problem.target_id
                || verification.profile != self.config.target.verification_profile
                || verification.verified_at_ms > task.updated_at_ms
                || task
                    .operation
                    .as_ref()
                    .is_some_and(|operation| verification.operation_id != operation.operation_id)
            {
                return Err(RecoveryError::Invalid(
                    "verification identity or time differs from task".into(),
                ));
            }
        }
        if let Some(record) = &task.result_check {
            let execution = &record.execution;
            text(&record.actor, 128)?;
            contract::evidence(&execution.evidence_refs)?;
            let operation = task.operation.as_ref().ok_or_else(|| {
                RecoveryError::Invalid("execution result check lacks operation".into())
            })?;
            if execution.operation_id != operation.operation_id
                || execution.target_id != operation.target
                || execution.executor_id != self.config.target.executor_id
                || execution.checked_at_ms > task.updated_at_ms
                || execution.checked_at_ms < task.created_at_ms
                || (execution.outcome != CheckedExecution::Unknown && !execution.executor_stopped)
                || task.verification.as_ref().is_none_or(|verification| {
                    verification.executor_stopped != execution.executor_stopped
                })
                || (execution.outcome == CheckedExecution::Unknown && task.stage != Unknown)
                || (execution.outcome == CheckedExecution::NotExecuted
                    && !matches!(task.stage, Unknown | Canceled))
                || task.receipt.as_ref().is_some_and(|receipt| {
                    receipt.executor_stopped
                        && match receipt.outcome {
                            ScriptOutcome::Executed => {
                                execution.outcome != CheckedExecution::Executed
                            }
                            ScriptOutcome::Failed => execution.outcome != CheckedExecution::Failed,
                            ScriptOutcome::Unknown => false,
                        }
                })
            {
                return Err(RecoveryError::Invalid(
                    "persisted execution result check identity, time or state mismatch".into(),
                ));
            }
            if matches!(task.stage, Publishing | Completed | Failed)
                && task.receipt.as_ref().is_none_or(|receipt| {
                    !receipt.executor_stopped
                        || receipt.outcome
                            != match execution.outcome {
                                CheckedExecution::Executed => ScriptOutcome::Executed,
                                CheckedExecution::Failed => ScriptOutcome::Failed,
                                _ => ScriptOutcome::Unknown,
                            }
                        || receipt.evidence_refs != execution.evidence_refs
                })
            {
                return Err(RecoveryError::Invalid(
                    "receipt differs from independent execution result check".into(),
                ));
            }
        }
        if matches!(task.stage, Verifying | Publishing | Completed) && task.receipt.is_none() {
            return Err(RecoveryError::Invalid(
                "post-execution stage lacks receipt".into(),
            ));
        }
        if task.stage == Verifying
            && !task.receipt.as_ref().is_some_and(|receipt| {
                receipt.outcome == ScriptOutcome::Executed && receipt.executor_stopped
            })
        {
            return Err(RecoveryError::Invalid(
                "verification requires confirmed stopped execution".into(),
            ));
        }
        if task.stage == Completed
            && (!task.receipt.as_ref().is_some_and(|receipt| {
                receipt.outcome == ScriptOutcome::Executed && receipt.executor_stopped
            }) || !task.verification.as_ref().is_some_and(|verification| {
                verification.healthy == Some(true) && verification.executor_stopped
            }))
        {
            return Err(RecoveryError::Invalid(
                "completion requires successful independent business verification".into(),
            ));
        }
        Ok(())
    }
}

fn validate_format(format: u32, version_2_seen: &mut bool) -> Result<(), RecoveryError> {
    match format {
        1 if !*version_2_seen => Ok(()),
        2 => {
            *version_2_seen = true;
            Ok(())
        }
        _ => Err(RecoveryError::Corrupt(
            "unsupported or decreasing journal format".into(),
        )),
    }
}

fn normalize_legacy_episode(
    task: &mut RecoveryTask,
    format: u32,
    tasks: &BTreeMap<String, RecoveryTask>,
) -> Result<(), RecoveryError> {
    if format == 1 && task.episode_count == 0 {
        task.episode_count = if let Some(previous) = tasks.get(&task.id) {
            previous.episode_count
        } else {
            (tasks
                .values()
                .filter(|previous| {
                    previous.problem.target_id == task.problem.target_id
                        && previous.problem.fingerprint == task.problem.fingerprint
                        && previous.problem.conditions == task.problem.conditions
                })
                .count() as u64)
                .checked_add(1)
                .ok_or(RecoveryError::Capacity)?
        };
    }
    Ok(())
}

/// Bounded, complete parsing and normal replay without opening runtime files.
pub(super) fn validated_journal(
    journal: &[u8],
    config: &RecoveryConfig,
) -> Result<Vec<JournalEntry>, RecoveryError> {
    config.validate()?;
    if journal.len() as u64 > config.max_journal_bytes {
        return Err(RecoveryError::Capacity);
    }
    let mut tasks = BTreeMap::new();
    let mut sequence = 0u64;
    let mut version_2_seen = false;
    let mut entries = Vec::new();
    for line in journal.split_inclusive(|byte| *byte == b'\n') {
        if line.len() > MAX_ENTRY_BYTES || line.last() != Some(&b'\n') {
            return Err(RecoveryError::Corrupt(
                "oversized or incomplete entry".into(),
            ));
        }
        let mut entry: JournalEntry = serde_json::from_slice(line)
            .map_err(|error| RecoveryError::Corrupt(error.to_string()))?;
        validate_format(entry.format, &mut version_2_seen)?;
        if sequence.checked_add(1) != Some(entry.sequence) || entry.config != *config {
            return Err(RecoveryError::Corrupt(
                "sequence or trusted configuration changed; explicit migration required".into(),
            ));
        }
        normalize_legacy_episode(&mut entry.task, entry.format, &tasks)?;
        validate_task(&tasks, config, &entry.task)
            .map_err(|error| RecoveryError::Corrupt(error.to_string()))?;
        sequence = entry.sequence;
        tasks.insert(entry.task.id.clone(), entry.task.clone());
        entries.push(entry);
    }
    Ok(entries)
}
