//! Pure script-free domain queries.
use super::{validation, *};

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

impl KnowledgeState {
    pub fn projection(&self) -> KnowledgeProjection {
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
        KnowledgeProjection {
            records: self.records.len(),
            scripts: self.scripts.len(),
            cases: self.cases.len(),
            quarantined_versions: self.quarantined.len(),
            statuses,
            max_records: self.config.max_records,
            max_cases_per_record: self.config.max_cases_per_record,
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
}
