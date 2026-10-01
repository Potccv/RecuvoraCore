//! Complete legacy approval validation, with no replayed review or execution effects.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyApprovalEntry {
    pub sequence: u64,
    pub now: u64,
    pub event: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalImportData {
    pub(super) limits: ApprovalLimits,
    history: Vec<LegacyApprovalEntry>,
}

/// Validated history capability. Neither a permit nor a deserializable authority.
pub struct ApprovalImport(ApprovalImportData);
impl ApprovalImport {
    pub fn validate(
        limits: ApprovalLimits,
        history: Vec<LegacyApprovalEntry>,
    ) -> Result<Self, ApprovalError> {
        let data = ApprovalImportData { limits, history };
        validate_history(&data, u64::MAX)?;
        Ok(Self(data))
    }
}

/// Constructed only by complete workflow migration validation.
pub struct LegacyExecutionUncertainty {
    pub(crate) operation: ProposedOperation,
    pub(crate) request_id: String,
    pub(crate) revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyUncertaintyData {
    pub(super) operation: ProposedOperation,
    pub(super) request_id: String,
    pub(super) revision: u64,
}

fn decode(
    entry: &LegacyApprovalEntry,
    ledger: &ApprovalLedger,
) -> Result<ApprovalEvent, ApprovalError> {
    let mut event = entry.event.clone();
    if event["change"]["change"] == "human_decision" {
        let id = event["request_id"]
            .as_str()
            .ok_or(ApprovalError::Invalid("missing legacy request identity"))?;
        let revision = ledger.get(id).ok_or(ApprovalError::NotFound)?.revision;
        // An old event cannot smuggle a different revision through conversion.
        if event["change"].get("expected_revision").is_some() {
            return Err(ApprovalError::Invalid(
                "unexpected legacy decision revision",
            ));
        }
        event["change"]["expected_revision"] = serde_json::json!(revision);
    }
    serde_json::from_value(event).map_err(|error| ApprovalError::Corrupt(error.to_string()))
}

pub(super) fn validate_history(
    data: &ApprovalImportData,
    now: u64,
) -> Result<ApprovalLedger, ApprovalError> {
    if data.history.len() > 1_000_000 {
        return Err(ApprovalError::Capacity);
    }
    let mut ledger = ApprovalLedger::new(data.limits.clone())?;
    for (index, entry) in data.history.iter().enumerate() {
        if entry.sequence != index as u64 + 1 || entry.now > now {
            return Err(ApprovalError::Conflict);
        }
        let event = decode(entry, &ledger)?;
        if matches!(
            event,
            ApprovalEvent::Recover
                | ApprovalEvent::Imported { .. }
                | ApprovalEvent::OriginalAuthorityUncertain { .. }
        ) {
            return Err(ApprovalError::Invalid("not a legacy event"));
        }
        let record = ledger.apply_legacy(&event, entry.now, entry.sequence)?;
        ledger
            .records
            .insert(record.request.request_id.clone(), record);
    }
    Ok(ledger)
}

impl ApprovalLedger {
    pub(crate) fn limits(&self) -> &ApprovalLimits {
        &self.config
    }

    // Request IDs keep their historical form while skipping all imported IDs.
    pub(super) fn request_id(&self, mut sequence: u64) -> Result<String, ApprovalError> {
        loop {
            let id = format!("approval-{sequence:016x}");
            if !self.records.contains_key(&id) {
                return Ok(id);
            }
            sequence = sequence.checked_add(1).ok_or(ApprovalError::Capacity)?;
        }
    }

    pub fn prepare_import(
        &self,
        commit_id: String,
        import: ApprovalImport,
        now: u64,
    ) -> Result<Prepared<Self, ApprovalEffect>, ApprovalError> {
        self.prepare_internal(
            commit_id,
            ApprovalEvent::Imported { history: import.0 },
            None,
            now,
        )
    }

    pub fn prepare_legacy_uncertain(
        &self,
        commit_id: String,
        proof: LegacyExecutionUncertainty,
        now: u64,
    ) -> Result<Prepared<Self, ApprovalEffect>, ApprovalError> {
        self.prepare_internal(
            commit_id,
            ApprovalEvent::OriginalAuthorityUncertain {
                proof: LegacyUncertaintyData {
                    operation: proof.operation,
                    request_id: proof.request_id,
                    revision: proof.revision,
                },
            },
            None,
            now,
        )
    }

    pub(crate) fn historical_changes(&self) -> Vec<(String, ApprovalChange, u64)> {
        let mut result = Vec::new();
        for entry in &self.history {
            match &entry.event {
                ApprovalEvent::Changed { request_id, change } => {
                    result.push((request_id.clone(), change.clone(), entry.now))
                }
                ApprovalEvent::Imported { history } => {
                    // Already validated on installation. Reconstruct implicit human revisions.
                    let mut ledger = ApprovalLedger::new(history.limits.clone())
                        .expect("validated import limits");
                    for old in &history.history {
                        let event = decode(old, &ledger).expect("validated imported event");
                        let record = ledger
                            .apply_legacy(&event, old.now, old.sequence)
                            .expect("validated imported transition");
                        ledger
                            .records
                            .insert(record.request.request_id.clone(), record);
                        if let ApprovalEvent::Changed { request_id, change } = event {
                            result.push((request_id, change, old.now));
                        }
                    }
                }
                _ => {}
            }
        }
        result
    }
}
