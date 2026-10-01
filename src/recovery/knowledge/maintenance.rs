//! Script-free operations projections and complete offline checkpoints.
use super::storage::{Entry, Event, MAX_RECORD_BYTES, next};
use super::{validation, *};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckpointMetadata {
    source_sequence: u64,
    source_bytes: u64,
    records: usize,
    scripts: usize,
    cases: usize,
    quarantined_versions: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckpointRecord {
    candidate: KnowledgeCandidate,
    revision: u64,
    status: KnowledgeStatus,
    case_count: usize,
    disabled: Option<KnowledgeDisablement>,
}

pub(super) struct CheckpointReplay {
    metadata: CheckpointMetadata,
    records: BTreeMap<String, CheckpointRecord>,
}

pub(super) fn verification_rank(record: &KnowledgeRecord) -> (usize, Option<u64>) {
    let verified = record
        .cases
        .iter()
        .filter(|case| case.result.outcome == RepairOutcome::Verified)
        .filter_map(|case| case.verification.as_ref());
    verified.fold((0, None), |(count, latest), proof| {
        (
            count + 1,
            Some(latest.map_or(proof.verified_at_ms, |time: u64| {
                time.max(proof.verified_at_ms)
            })),
        )
    })
}

impl KnowledgeStore {
    pub fn projection(&self) -> KnowledgeStoreProjection {
        let mut statuses = KnowledgeStatusCounts::default();
        for record in self.records.values() {
            match record.status {
                KnowledgeStatus::Candidate => statuses.candidate += 1,
                KnowledgeStatus::Verified => statuses.verified += 1,
                KnowledgeStatus::Failed => statuses.failed += 1,
                KnowledgeStatus::Unknown => statuses.unknown += 1,
                KnowledgeStatus::Disabled => statuses.disabled += 1,
            }
        }
        KnowledgeStoreProjection {
            records: self.records.len(),
            scripts: self.scripts.len(),
            cases: self.cases.len(),
            quarantined_versions: self.quarantined.len(),
            statuses,
            journal_bytes: self.bytes,
            max_journal_bytes: self.config.max_journal_bytes,
            max_records: self.config.max_records,
            max_cases_per_record: self.config.max_cases_per_record,
            writable: !self.poisoned && self.journal.is_some(),
        }
    }

    /// Stable pagination includes quarantined and disabled records for operations
    /// review; these reads never grant script reuse or execution authority.
    pub fn inspect(
        &self,
        query: &KnowledgeInspectionQuery,
    ) -> Result<Vec<KnowledgeRecordProjection>, KnowledgeError> {
        if !(1..=100).contains(&query.limit) {
            return Err(KnowledgeError::Invalid(
                "inspection limit must be 1..100".into(),
            ));
        }
        if let Some(id) = &query.after_id {
            validation::text(id, 128, "inspection cursor")?;
        }
        Ok(self
            .records
            .values()
            .filter_map(|record| {
                let script = &record.candidate.script;
                let quarantined = self
                    .quarantined
                    .contains(&(script.id.clone(), script.version));
                if query.after_id.as_ref().is_some_and(|id| &record.id <= id)
                    || query.status.is_some_and(|status| record.status != status)
                    || query
                        .quarantined
                        .is_some_and(|expected| quarantined != expected)
                {
                    return None;
                }
                let (verified_case_count, latest_verified_at_ms) = verification_rank(record);
                Some(KnowledgeRecordProjection {
                    id: record.id.clone(),
                    revision: record.revision,
                    incident_id: record.candidate.incident_id.clone(),
                    summary: record.candidate.summary.clone(),
                    status: record.status,
                    script_id: script.id.clone(),
                    script_version: script.version,
                    script_bytes: script.source.len(),
                    reusable: record.candidate.reusable,
                    quarantined,
                    case_count: record.cases.len(),
                    verified_case_count,
                    latest_verified_at_ms,
                    disabled: record.disabled.clone(),
                })
            })
            .take(query.limit)
            .collect())
    }

    /// Exports every authoritative case and identity in a bounded checkpoint.
    /// Export requires the reliable writer lock. The Host freezes the old log,
    /// exports and verifies a new protected file, then handles offline replacement
    /// and retention without accepting stale or uncertain in-memory snapshots.
    /// Byte capacity can be reclaimed; record and case limits cannot be evaded.
    pub fn export_compacted(&self) -> Result<CompactJournal, KnowledgeError> {
        self.validate_export_source()?;
        let projection = self.projection();
        let metadata = CheckpointMetadata {
            source_sequence: self.sequence,
            source_bytes: self.bytes,
            records: projection.records,
            scripts: projection.scripts,
            cases: projection.cases,
            quarantined_versions: projection.quarantined_versions,
        };
        let mut bytes = Vec::new();
        let mut sequence = 0;
        push(
            &mut bytes,
            &mut sequence,
            Event::CheckpointBegin { metadata },
            &self.config,
        )?;
        for record in self.records.values() {
            push(
                &mut bytes,
                &mut sequence,
                Event::CheckpointRecord {
                    record: CheckpointRecord {
                        candidate: record.candidate.clone(),
                        revision: record.revision,
                        status: record.status,
                        case_count: record.cases.len(),
                        disabled: record.disabled.clone(),
                    },
                },
                &self.config,
            )?;
            let mut chunk = Vec::new();
            let mut chunk_bytes = 0usize;
            for case in &record.cases {
                let case_bytes = serde_json::to_vec(case)
                    .map_err(|error| KnowledgeError::Invalid(error.to_string()))?
                    .len()
                    + 1;
                if case_bytes > MAX_RECORD_BYTES - 1024 {
                    return Err(KnowledgeError::Capacity(
                        "single checkpoint case bytes".into(),
                    ));
                }
                if chunk_bytes + case_bytes > MAX_RECORD_BYTES - 1024 {
                    push(
                        &mut bytes,
                        &mut sequence,
                        Event::CheckpointCases {
                            record_id: record.id.clone(),
                            cases: chunk,
                        },
                        &self.config,
                    )?;
                    chunk = Vec::new();
                    chunk_bytes = 0;
                }
                chunk.push(case.clone());
                chunk_bytes += case_bytes;
            }
            if !chunk.is_empty() {
                push(
                    &mut bytes,
                    &mut sequence,
                    Event::CheckpointCases {
                        record_id: record.id.clone(),
                        cases: chunk,
                    },
                    &self.config,
                )?;
            }
        }
        push(
            &mut bytes,
            &mut sequence,
            Event::CheckpointCommit,
            &self.config,
        )?;
        Ok(CompactJournal {
            source_sequence: self.sequence,
            source_bytes: self.bytes,
            compacted_bytes: bytes.len() as u64,
            records: projection.records,
            scripts: projection.scripts,
            cases: projection.cases,
            quarantined_versions: projection.quarantined_versions,
            bytes,
        })
    }
}

fn encoded(sequence: u64, event: &Event) -> Result<Vec<u8>, KnowledgeError> {
    #[derive(Serialize)]
    struct BorrowedEntry<'a> {
        format: u32,
        sequence: u64,
        event: &'a Event,
    }
    let mut bytes = serde_json::to_vec(&BorrowedEntry {
        format: 2,
        sequence,
        event,
    })
    .map_err(|error| KnowledgeError::Invalid(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn push(
    bytes: &mut Vec<u8>,
    sequence: &mut u64,
    event: Event,
    config: &KnowledgeStoreConfig,
) -> Result<(), KnowledgeError> {
    let next_sequence = next(*sequence)?;
    let entry = encoded(next_sequence, &event)?;
    if entry.len() > MAX_RECORD_BYTES
        || (bytes.len() as u64)
            .checked_add(entry.len() as u64)
            .is_none_or(|total| total > config.max_journal_bytes)
    {
        return Err(KnowledgeError::Capacity("compacted journal bytes".into()));
    }
    bytes.extend_from_slice(&entry);
    *sequence = next_sequence;
    Ok(())
}

pub(super) fn replay_checkpoint(
    store: &mut KnowledgeStore,
    entry: &Entry,
    replay: &mut Option<CheckpointReplay>,
) -> Result<(), KnowledgeError> {
    let fail = |message: &str| KnowledgeError::Corrupt(message.into());
    match &entry.event {
        Event::CheckpointBegin { metadata } => {
            if entry.sequence != 1
                || replay.is_some()
                || !store.records.is_empty()
                || metadata.records > store.config.max_records
                || metadata.scripts > metadata.records
                || metadata.quarantined_versions > metadata.scripts
                || metadata.cases
                    > metadata
                        .records
                        .checked_mul(store.config.max_cases_per_record)
                        .ok_or_else(|| fail("checkpoint count overflow"))?
                || metadata.source_bytes > 1024 * 1024 * 1024
            {
                return Err(fail("invalid checkpoint metadata"));
            }
            *replay = Some(CheckpointReplay {
                metadata: metadata.clone(),
                records: BTreeMap::new(),
            });
        }
        Event::CheckpointRecord { record } => {
            let state = replay
                .as_mut()
                .ok_or_else(|| fail("checkpoint has no beginning"))?;
            if state.records.len() >= state.metadata.records
                || state.records.contains_key(&record.candidate.id)
                || record.case_count > store.config.max_cases_per_record
                || record.revision
                    != 1 + record.case_count as u64 + u64::from(record.disabled.is_some())
            {
                return Err(fail("checkpoint record count, revision or identity"));
            }
            let prepared = store
                .prepare(&Event::Candidate {
                    candidate: record.candidate.clone(),
                })
                .map_err(|error| fail(&error.to_string()))?
                .ok_or_else(|| fail("duplicate checkpoint candidate"))?;
            store.install(prepared);
            state
                .records
                .insert(record.candidate.id.clone(), record.clone());
        }
        Event::CheckpointCases { record_id, cases } => {
            let state = replay
                .as_ref()
                .ok_or_else(|| fail("checkpoint has no beginning"))?;
            let expected = state
                .records
                .get(record_id)
                .ok_or_else(|| fail("checkpoint case has no record"))?;
            let current = store
                .records
                .get(record_id)
                .ok_or_else(|| fail("checkpoint record is missing"))?;
            if cases.is_empty()
                || current
                    .cases
                    .len()
                    .checked_add(cases.len())
                    .is_none_or(|count| count > expected.case_count)
            {
                return Err(fail("checkpoint case count"));
            }
            for case in cases {
                let prepared = store
                    .prepare(&Event::Outcome {
                        record_id: record_id.clone(),
                        case: case.result.clone(),
                        verification: case.verification.clone(),
                    })
                    .map_err(|error| fail(&error.to_string()))?
                    .ok_or_else(|| fail("duplicate checkpoint case"))?;
                store.install(prepared);
            }
        }
        Event::CheckpointCommit => {
            let state = replay
                .take()
                .ok_or_else(|| fail("checkpoint has no beginning"))?;
            if state.records.len() != state.metadata.records {
                return Err(fail("checkpoint omits records"));
            }
            for (id, expected) in state.records {
                let actual = store
                    .records
                    .get(&id)
                    .ok_or_else(|| fail("checkpoint record is missing"))?;
                if actual.cases.len() != expected.case_count {
                    return Err(fail("checkpoint omits cases"));
                }
                if let Some(disabled) = expected.disabled {
                    let prepared = store
                        .prepare(&Event::Disable {
                            record_id: id.clone(),
                            expected_revision: actual.revision,
                            actor: disabled.actor,
                            reason: disabled.reason,
                        })
                        .map_err(|error| fail(&error.to_string()))?
                        .ok_or_else(|| fail("duplicate checkpoint disablement"))?;
                    store.install(prepared);
                }
                let actual = store
                    .records
                    .get(&id)
                    .ok_or_else(|| fail("checkpoint record is missing"))?;
                if actual.status != expected.status || actual.revision != expected.revision {
                    return Err(fail(
                        "checkpoint status or revision differs from authoritative cases",
                    ));
                }
            }
            if store.cases.len() != state.metadata.cases
                || store.scripts.len() != state.metadata.scripts
                || store.quarantined.len() != state.metadata.quarantined_versions
            {
                return Err(fail("checkpoint identity or quarantine counts differ"));
            }
        }
        _ => return Err(fail("ordinary event before checkpoint commit")),
    }
    Ok(())
}
