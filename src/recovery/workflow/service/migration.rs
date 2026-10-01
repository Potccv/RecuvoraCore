//! Pure, explicit workflow-journal migration for an offline trusted Host.
//!
//! These functions never open, truncate, replace or recover runtime files. The
//! Host must stop the owner, lock and read the complete journal within the
//! configured bound, retain the approval and knowledge stores, and durably
//! switch the validated output before reopening with the replacement config.
//! Migration preserves progress and references; it cannot create lost plans,
//! approvals, execution evidence, authorization or reusable-script eligibility.

use super::storage::{JournalEntry, MAX_ENTRY_BYTES, validated_journal};
use super::*;

/// Facts about a fully replayed, bounded workflow journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryJournalSummary {
    pub entries: usize,
    pub tasks: usize,
    pub version_1_entries: usize,
    pub version_2_entries: usize,
    pub bytes: u64,
    pub maximum_diagnoses: u32,
}

/// A reviewed conversion result. The bytes contain every original revision.
/// Installing them is an offline Host operation, not a Core state transition.
#[derive(Debug)]
pub struct RecoveryJournalMigration {
    source: RecoveryJournalSummary,
    migrated: RecoveryJournalSummary,
    journal: Vec<u8>,
}

impl RecoveryJournalMigration {
    pub fn source(&self) -> &RecoveryJournalSummary {
        &self.source
    }

    pub fn migrated(&self) -> &RecoveryJournalSummary {
        &self.migrated
    }

    pub fn journal(&self) -> &[u8] {
        &self.journal
    }

    pub fn into_journal(self) -> Vec<u8> {
        self.journal
    }
}

/// Validate complete JSONL with its original trusted configuration.
/// Unknown formats, partial tails, invalid revisions and transitions are errors.
pub fn validate_recovery_journal(
    journal: &[u8],
    config: &RecoveryConfig,
) -> Result<RecoveryJournalSummary, RecoveryError> {
    let entries = validated_journal(journal, config)?;
    Ok(summary(&entries, journal.len() as u64))
}

/// Convert version 1 history to version 2 and explicitly adjust safe limits.
/// Version 2 history can use the same API for an offline limits-only migration.
/// Both the complete source and the complete output undergo normal replay
/// validation. Target, executor, Harness, policies, action contracts, timeouts
/// and script-reuse thresholds must remain identical.
pub fn plan_recovery_journal_migration(
    journal: &[u8],
    original_config: &RecoveryConfig,
    replacement_config: &RecoveryConfig,
) -> Result<RecoveryJournalMigration, RecoveryError> {
    original_config.validate()?;
    replacement_config.validate()?;
    validate_config_change(original_config, replacement_config)?;
    let entries = validated_journal(journal, original_config)?;
    let source = summary(&entries, journal.len() as u64);
    let mut output = Vec::new();
    for mut entry in entries {
        entry.format = 2;
        entry.config = replacement_config.clone();
        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        let total = (output.len() as u64)
            .checked_add(line.len() as u64)
            .filter(|total| *total <= replacement_config.max_journal_bytes)
            .ok_or(RecoveryError::Capacity)?;
        if line.len() > MAX_ENTRY_BYTES {
            return Err(RecoveryError::Capacity);
        }
        output.reserve((total as usize).saturating_sub(output.len()));
        output.extend_from_slice(&line);
    }
    let migrated_entries = validated_journal(&output, replacement_config)?;
    let migrated = summary(&migrated_entries, output.len() as u64);
    Ok(RecoveryJournalMigration {
        source,
        migrated,
        journal: output,
    })
}

fn validate_config_change(
    original: &RecoveryConfig,
    replacement: &RecoveryConfig,
) -> Result<(), RecoveryError> {
    let mut permitted = original.clone();
    permitted.max_tasks = replacement.max_tasks;
    permitted.max_journal_bytes = replacement.max_journal_bytes;
    permitted.max_diagnoses = replacement.max_diagnoses;
    permitted.max_tool_calls = replacement.max_tool_calls;
    if &permitted != replacement {
        return Err(RecoveryError::Invalid(
            "journal migration only permits task, journal, diagnosis and tool-call limits".into(),
        ));
    }
    Ok(())
}

fn summary(entries: &[JournalEntry], bytes: u64) -> RecoveryJournalSummary {
    RecoveryJournalSummary {
        entries: entries.len(),
        tasks: entries
            .iter()
            .map(|entry| entry.task.id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        version_1_entries: entries.iter().filter(|entry| entry.format == 1).count(),
        version_2_entries: entries.iter().filter(|entry| entry.format == 2).count(),
        bytes,
        maximum_diagnoses: entries
            .iter()
            .map(|entry| entry.task.diagnosis_attempts)
            .max()
            .unwrap_or(0),
    }
}
