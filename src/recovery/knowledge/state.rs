//! Deterministic knowledge decisions; the Host owns all storage and serialization.
use super::{validation, *};
use crate::collections::Map;
use crate::operation::Prepared;
use im::OrdSet;

/// Validated authority built through proposals and confirmed Host commits.
/// There is intentionally no unchecked deserialization constructor.
#[derive(Clone, Debug)]
pub struct KnowledgeState {
    pub(super) config: KnowledgeConfig,
    revision: u64,
    digest: String,
    commit_ids: OrdSet<String>,
    pub(super) records: Map<String, KnowledgeRecord>,
    pub(super) scripts: Map<(String, u64), ScriptArtifact>,
    pub(super) cases: Map<String, (String, KnowledgeCase)>,
    pub(super) quarantined: OrdSet<(String, u64)>,
}

impl KnowledgeState {
    pub fn new(config: KnowledgeConfig) -> Result<Self, KnowledgeError> {
        config.validate()?;
        Ok(Self {
            digest: crate::binding::digest(&("knowledge", &config)),
            commit_ids: OrdSet::new(),
            config,
            revision: 0,
            records: Map::new(),
            scripts: Map::new(),
            cases: Map::new(),
            quarantined: OrdSet::new(),
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns an uncommitted decision. Persist the command and request with an
    /// atomic revision comparison before confirming and installing the new state.
    pub fn propose(
        &self,
        commit_id: impl Into<String>,
        command: KnowledgeCommand,
    ) -> Result<Prepared<Self>, KnowledgeError> {
        let commit_id = commit_id.into();
        if self.commit_ids.contains(&commit_id) {
            return Err(KnowledgeError::Conflict("commit identity reused".into()));
        }
        let record = self.prepare_record(&command)?;
        let mut state = self.clone();
        if let Some(record) = record {
            state.install(record);
        }
        if let KnowledgeCommand::ExpandCapacity { target, .. } = &command {
            state.config = target.clone();
        }
        state.revision = next(self.revision)?;
        let input = serde_json::to_value((&self.digest, &self.config, &command))
            .map_err(|error| KnowledgeError::Invalid(error.to_string()))?;
        let request = crate::operation::CommitRequest::new(
            commit_id.clone(),
            self.revision,
            "knowledge".into(),
            input.clone(),
        )?;
        state.digest = crate::binding::digest(&request);
        state.commit_ids.insert(commit_id.clone());
        Ok(Prepared::new_bound(
            commit_id,
            self.revision,
            "knowledge".into(),
            input,
            state,
            Vec::new(),
        )?)
    }

    /// Repeats live validation for every confirmed historical command. Host must
    /// provide the complete ordered history from its protected persistence layer.
    pub fn replay(
        config: KnowledgeConfig,
        entries: Vec<KnowledgeReplayEntry>,
    ) -> Result<Self, KnowledgeError> {
        let mut state = Self::new(config)?;
        for entry in entries {
            let pending = state.propose(entry.request.id.clone(), entry.command)?;
            if pending.request() != &entry.request {
                return Err(KnowledgeError::Conflict(
                    "replay revision or commit identity mismatch".into(),
                ));
            }
            state = pending.confirm(entry.receipt)?.state;
        }
        Ok(state)
    }

    /// Export complete domain data without selecting any storage mechanism.
    /// Use ordered committed commands for validated replay.
    pub fn snapshot(&self) -> KnowledgeSnapshot {
        KnowledgeSnapshot {
            revision: self.revision,
            config: self.config.clone(),
            records: self.records.values().cloned().collect(),
        }
    }

    /// Permanent version exclusion, including failures recorded by other candidates.
    pub fn is_quarantined(&self, script_id: &str, version: u64) -> bool {
        self.quarantined.contains(&(script_id.into(), version))
    }

    pub fn get(&self, id: &str) -> Option<KnowledgeRecord> {
        self.records.get(id).cloned()
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
                    super::query::verification_rank(record),
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

    fn require_record(&self, id: &str) -> Result<KnowledgeRecord, KnowledgeError> {
        self.get(id)
            .ok_or_else(|| KnowledgeError::NotFound(id.into()))
    }

    fn prepare_record(
        &self,
        event: &KnowledgeCommand,
    ) -> Result<Option<KnowledgeRecord>, KnowledgeError> {
        match event {
            KnowledgeCommand::ExpandCapacity { expected, target } => {
                target.validate()?;
                if expected != &self.config
                    || target.max_records < expected.max_records
                    || target.max_cases_per_record < expected.max_cases_per_record
                    || target == expected
                {
                    return Err(KnowledgeError::Conflict("capacity expansion requires current configuration and strictly increased limits".into()));
                }
                Ok(None)
            }
            KnowledgeCommand::UpsertCandidate(candidate) => {
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
            KnowledgeCommand::RecordOutcome {
                record_id,
                case,
                verification,
            } => {
                let mut record = self.require_record(record_id)?;
                validation::case(
                    case,
                    verification.as_ref().map(|value| &value.0),
                    &record.candidate.script,
                )?;
                let entry = KnowledgeCase {
                    result: case.clone(),
                    verification: verification.as_ref().map(|value| value.0.clone()),
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
            KnowledgeCommand::Disable {
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

    fn install(&mut self, record: KnowledgeRecord) {
        let script = &record.candidate.script;
        let key = (script.id.clone(), script.version);
        if !self.scripts.contains_key(&key) {
            self.scripts.insert(key.clone(), script.clone());
        }
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
}

pub(super) fn next(value: u64) -> Result<u64, KnowledgeError> {
    value
        .checked_add(1)
        .ok_or_else(|| KnowledgeError::Capacity("sequence overflow".into()))
}
