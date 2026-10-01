//! Locked append-only journal, bounded replay and sync-before-publish mutations.
use super::{paths, validation, *};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

pub(super) const MAX_RECORD_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Event {
    CheckpointBegin {
        metadata: super::maintenance::CheckpointMetadata,
    },
    CheckpointRecord {
        record: super::maintenance::CheckpointRecord,
    },
    CheckpointCases {
        record_id: String,
        cases: Vec<KnowledgeCase>,
    },
    CheckpointCommit,
    Candidate {
        candidate: KnowledgeCandidate,
    },
    Outcome {
        record_id: String,
        case: RepairCase,
        verification: Option<BusinessVerificationRecord>,
    },
    Disable {
        record_id: String,
        expected_revision: u64,
        actor: String,
        reason: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub(super) format: u32,
    pub(super) sequence: u64,
    pub(super) event: Event,
}

/// Single trusted writer. Wrap in one mutex when sharing across async tasks; do
/// not hold that mutex across Harness, execution or verification calls.
pub struct KnowledgeStore {
    pub(super) journal: Option<File>,
    path: PathBuf,
    directories: Vec<File>,
    pub(super) config: KnowledgeStoreConfig,
    pub(super) bytes: u64,
    pub(super) sequence: u64,
    format: u32,
    pub(super) poisoned: bool,
    pub(super) records: BTreeMap<String, KnowledgeRecord>,
    pub(super) scripts: BTreeMap<(String, u64), ScriptArtifact>,
    pub(super) cases: BTreeMap<String, (String, KnowledgeCase)>,
    pub(super) quarantined: BTreeSet<(String, u64)>,
}

impl KnowledgeStore {
    /// Requires a regular journal outside this crate's compilation source tree.
    /// The embedding application must separately protect its own sources,
    /// installation, active configuration and verifier evidence from script writes.
    pub fn open(
        path: impl AsRef<Path>,
        config: KnowledgeStoreConfig,
    ) -> Result<Self, KnowledgeError> {
        config.validate()?;
        let path = path.as_ref().to_path_buf();
        let (journal, directories) = paths::open(&path)?;
        journal
            .try_lock_exclusive()
            .map_err(|error| KnowledgeError::Unavailable(format!("writer lock: {error}")))?;
        let bytes = journal.metadata()?.len();
        if bytes > config.max_journal_bytes {
            return Err(KnowledgeError::Capacity("journal bytes".into()));
        }
        let mut store = Self {
            journal: Some(journal),
            path,
            directories,
            config,
            bytes,
            sequence: 0,
            format: 1,
            poisoned: false,
            records: BTreeMap::new(),
            scripts: BTreeMap::new(),
            cases: BTreeMap::new(),
            quarantined: BTreeSet::new(),
        };
        store.replay()?;
        Ok(store)
    }

    /// Candidate identity is immutable. An identical retry returns the current
    /// record without resetting its status or adding a journal entry.
    pub fn upsert_candidate(
        &mut self,
        candidate: KnowledgeCandidate,
    ) -> Result<KnowledgeRecord, KnowledgeError> {
        let id = candidate.id.clone();
        self.append(Event::Candidate { candidate })?;
        self.require_record(&id)
    }

    /// Case ID is the durable idempotency key across the store. Verified requires
    /// the trusted application's independent business-verification attestation;
    /// a script exit status, model statement or text readback is insufficient.
    pub fn record_outcome(
        &mut self,
        record_id: &str,
        case: RepairCase,
        verification: Option<TrustedBusinessVerification>,
    ) -> Result<KnowledgeRecord, KnowledgeError> {
        self.append(Event::Outcome {
            record_id: record_id.into(),
            case,
            verification: verification.map(|value| value.0),
        })?;
        self.require_record(record_id)
    }

    /// Trusted administrative exclusion; does not revoke an already issued permit.
    pub fn disable(
        &mut self,
        record_id: &str,
        expected_revision: u64,
        actor: &str,
        reason: &str,
    ) -> Result<KnowledgeRecord, KnowledgeError> {
        self.append(Event::Disable {
            record_id: record_id.into(),
            expected_revision,
            actor: actor.into(),
            reason: reason.into(),
        })?;
        self.require_record(record_id)
    }

    pub fn get(&self, id: &str) -> Option<KnowledgeRecord> {
        self.records.get(id).cloned()
    }

    pub(super) fn validate_export_source(&self) -> Result<(), KnowledgeError> {
        if self.poisoned {
            return Err(KnowledgeError::Unavailable(
                "store closed or prior write failed".into(),
            ));
        }
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| KnowledgeError::Unavailable("store closed".into()))?;
        paths::validate_current(&self.path, journal)?;
        if journal.metadata()?.len() != self.bytes {
            return Err(KnowledgeError::Unavailable(
                "journal length changed externally".into(),
            ));
        }
        Ok(())
    }

    /// Checks bounded content and an already known immutable version before a
    /// workflow seeks approval. This read does not reserve a new version, prove
    /// applicability, or authorize execution; accepted writes check it again.
    pub fn validate_script(&self, script: &ScriptArtifact) -> Result<(), KnowledgeError> {
        validation::script(script)?;
        if self
            .scripts
            .get(&(script.id.clone(), script.version))
            .is_some_and(|old| old != script)
        {
            return Err(KnowledgeError::Conflict(
                "script version content is immutable".into(),
            ));
        }
        Ok(())
    }

    /// Returns verified, applicable candidates ranked by trusted verification
    /// count, latest verification time, exact condition coverage, then record ID. A
    /// failure, unknown result or disablement quarantines that immutable version
    /// across all cases; a new version needs fresh verification before reuse.
    pub fn search(&self, query: &KnowledgeQuery) -> Result<Vec<KnowledgeRecord>, KnowledgeError> {
        self.search_inner(query, false)
    }

    /// Uses the same applicability checks but includes only cases explicitly
    /// marked reusable by trusted host policy, filtering before the result limit.
    /// Each match still needs a new applicability review and execution permit.
    pub fn search_reusable(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<KnowledgeRecord>, KnowledgeError> {
        self.search_inner(query, true)
    }

    fn search_inner(
        &self,
        query: &KnowledgeQuery,
        reusable_only: bool,
    ) -> Result<Vec<KnowledgeRecord>, KnowledgeError> {
        validation::query(query)?;
        let mut matches: Vec<_> = self
            .records
            .values()
            .filter(|record| {
                let candidate = &record.candidate;
                record.status == KnowledgeStatus::Verified
                    && (!reusable_only || candidate.reusable)
                    && !self
                        .quarantined
                        .contains(&(candidate.script.id.clone(), candidate.script.version))
                    && super::source::applicable(candidate, query)
            })
            .map(|record| {
                (
                    super::maintenance::verification_rank(record),
                    super::source::exact_condition_count(&record.candidate),
                    record,
                )
            })
            .collect();
        matches.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.id.cmp(&right.2.id))
        });
        Ok(matches
            .into_iter()
            .take(query.limit)
            .map(|(_, _, record)| record.clone())
            .collect())
    }

    /// Releases the single-writer lock, retaining only read projections.
    pub fn close(&mut self) -> Result<(), KnowledgeError> {
        self.poisoned = true;
        self.journal.take();
        self.directories.clear();
        Ok(())
    }

    fn require_record(&self, id: &str) -> Result<KnowledgeRecord, KnowledgeError> {
        self.get(id)
            .ok_or_else(|| KnowledgeError::NotFound(id.into()))
    }

    pub(super) fn prepare(&self, event: &Event) -> Result<Option<KnowledgeRecord>, KnowledgeError> {
        match event {
            Event::CheckpointBegin { .. }
            | Event::CheckpointRecord { .. }
            | Event::CheckpointCases { .. }
            | Event::CheckpointCommit => Err(KnowledgeError::Invalid(
                "checkpoint outside journal prefix".into(),
            )),
            Event::Candidate { candidate } => {
                validation::candidate(candidate)?;
                if let Some(old) = self.records.get(&candidate.id) {
                    return if old.candidate == *candidate {
                        Ok(None)
                    } else {
                        Err(KnowledgeError::Conflict(
                            "candidate identity is immutable".into(),
                        ))
                    };
                }
                self.validate_script(&candidate.script)?;
                if self.records.len() >= self.config.max_records {
                    return Err(KnowledgeError::Capacity("knowledge records".into()));
                }
                Ok(Some(KnowledgeRecord {
                    id: candidate.id.clone(),
                    revision: 1,
                    candidate: candidate.clone(),
                    status: KnowledgeStatus::Candidate,
                    cases: Vec::new(),
                    disabled: None,
                }))
            }
            Event::Outcome {
                record_id,
                case,
                verification,
            } => {
                let mut record = self.require_record(record_id)?;
                validation::case(case, verification.as_ref(), &record.candidate.script)?;
                let entry = KnowledgeCase {
                    result: case.clone(),
                    verification: verification.clone(),
                };
                if let Some((old_record, old_case)) = self.cases.get(&case.id) {
                    return if old_record == record_id && old_case == &entry {
                        Ok(None)
                    } else {
                        Err(KnowledgeError::Conflict(
                            "case id was used with another result".into(),
                        ))
                    };
                }
                if record.status == KnowledgeStatus::Disabled {
                    return Err(KnowledgeError::Conflict("record is disabled".into()));
                }
                if record.cases.len() >= self.config.max_cases_per_record {
                    return Err(KnowledgeError::Capacity("cases per record".into()));
                }
                if case.recorded_at_ms < record.candidate.created_at_ms
                    || record.cases.last().is_some_and(|previous| {
                        case.recorded_at_ms < previous.result.recorded_at_ms
                    })
                {
                    return Err(KnowledgeError::Conflict(
                        "case timestamp predates candidate or latest case".into(),
                    ));
                }
                record.revision = next(record.revision)?;
                record.status = match case.outcome {
                    RepairOutcome::Verified => KnowledgeStatus::Verified,
                    RepairOutcome::Failed => KnowledgeStatus::Failed,
                    RepairOutcome::Unknown => KnowledgeStatus::Unknown,
                };
                record.cases.push(entry);
                Ok(Some(record))
            }
            Event::Disable {
                record_id,
                expected_revision,
                actor,
                reason,
            } => {
                validation::text(actor, 256, "disabling actor")?;
                validation::text(reason, 4096, "disable reason")?;
                let mut record = self.require_record(record_id)?;
                let disabled = KnowledgeDisablement {
                    actor: actor.clone(),
                    reason: reason.clone(),
                };
                if record.status == KnowledgeStatus::Disabled
                    && record.disabled.as_ref() == Some(&disabled)
                    && expected_revision.checked_add(1) == Some(record.revision)
                {
                    return Ok(None);
                }
                if record.revision != *expected_revision || record.disabled.is_some() {
                    return Err(KnowledgeError::Conflict(
                        "stale revision or already disabled".into(),
                    ));
                }
                record.revision = next(record.revision)?;
                record.status = KnowledgeStatus::Disabled;
                record.disabled = Some(disabled);
                Ok(Some(record))
            }
        }
    }

    pub(super) fn install(&mut self, record: KnowledgeRecord) {
        let script = &record.candidate.script;
        let key = (script.id.clone(), script.version);
        self.scripts
            .entry(key.clone())
            .or_insert_with(|| script.clone());
        if matches!(
            record.status,
            KnowledgeStatus::Failed | KnowledgeStatus::Unknown | KnowledgeStatus::Disabled
        ) {
            self.quarantined.insert(key);
        }
        if let Some(case) = record.cases.last() {
            self.cases
                .insert(case.result.id.clone(), (record.id.clone(), case.clone()));
        }
        self.records.insert(record.id.clone(), record);
    }

    fn append(&mut self, event: Event) -> Result<(), KnowledgeError> {
        if self.poisoned || self.journal.is_none() {
            return Err(KnowledgeError::Unavailable(
                "store closed or prior write failed".into(),
            ));
        }
        let Some(record) = self.prepare(&event)? else {
            return Ok(());
        };
        let sequence = next(self.sequence)?;
        let mut bytes = serde_json::to_vec(&Entry {
            format: self.format,
            sequence,
            event,
        })
        .map_err(|error| KnowledgeError::Invalid(error.to_string()))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(KnowledgeError::Capacity(
                "single journal entry bytes".into(),
            ));
        }
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= self.config.max_journal_bytes)
            .ok_or_else(|| KnowledgeError::Capacity("journal bytes".into()))?;
        self.poisoned = true;
        let journal = self
            .journal
            .as_mut()
            .ok_or_else(|| KnowledgeError::Unavailable("store closed".into()))?;
        paths::validate_current(&self.path, journal)?;
        if journal.metadata()?.len() != self.bytes {
            return Err(KnowledgeError::Unavailable(
                "journal length changed externally".into(),
            ));
        }
        journal.write_all(&bytes)?;
        journal.sync_data()?;
        self.install(record);
        self.bytes = total;
        self.sequence = sequence;
        self.poisoned = false;
        Ok(())
    }

    fn replay(&mut self) -> Result<(), KnowledgeError> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| KnowledgeError::Unavailable("store closed".into()))?;
        let mut reader = BufReader::new(journal.try_clone()?);
        let mut read_bytes = 0u64;
        let mut checkpoint = None;
        let mut checkpoint_committed = false;
        loop {
            let mut bytes = Vec::new();
            let count = reader
                .by_ref()
                .take((MAX_RECORD_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes)?;
            if count == 0 {
                break;
            }
            if count > MAX_RECORD_BYTES || bytes.last() != Some(&b'\n') {
                return Err(KnowledgeError::Corrupt(
                    "oversized or incomplete entry".into(),
                ));
            }
            read_bytes = read_bytes
                .checked_add(count as u64)
                .ok_or_else(|| KnowledgeError::Capacity("journal bytes".into()))?;
            if read_bytes > self.config.max_journal_bytes {
                return Err(KnowledgeError::Capacity(
                    "journal bytes during replay".into(),
                ));
            }
            let entry: Entry = serde_json::from_slice(&bytes)
                .map_err(|error| KnowledgeError::Corrupt(error.to_string()))?;
            if self.sequence == 0 {
                self.format = entry.format;
            }
            if !matches!(entry.format, 1 | 2)
                || entry.format != self.format
                || entry.sequence != next(self.sequence)?
            {
                return Err(KnowledgeError::Corrupt("format or journal sequence".into()));
            }
            if entry.format == 2 && !checkpoint_committed {
                super::maintenance::replay_checkpoint(self, &entry, &mut checkpoint)?;
                checkpoint_committed = matches!(entry.event, Event::CheckpointCommit);
                self.sequence = entry.sequence;
                continue;
            }
            let record = self
                .prepare(&entry.event)
                .map_err(|error| KnowledgeError::Corrupt(error.to_string()))?
                .ok_or_else(|| KnowledgeError::Corrupt("duplicate committed event".into()))?;
            self.install(record);
            self.sequence = entry.sequence;
        }
        if checkpoint.is_some() || (self.format == 2 && !checkpoint_committed) {
            return Err(KnowledgeError::Corrupt("incomplete checkpoint".into()));
        }
        if read_bytes != self.bytes {
            return Err(KnowledgeError::Corrupt(
                "journal length changed during replay".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn next(value: u64) -> Result<u64, KnowledgeError> {
    value
        .checked_add(1)
        .ok_or_else(|| KnowledgeError::Capacity("sequence overflow".into()))
}
