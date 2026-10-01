//! Durable monitoring facts. Acknowledgement is attribution, never repair authority.
mod contract;
mod paths;
mod storage;
mod transitions;
mod validation;

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::File, path::PathBuf};

const MAX_RECORD_BYTES: usize = 524_288;
const MAX_CHECKPOINT_BYTES: usize = 131_072;
const MAX_EVIDENCE_BYTES: usize = 16_384;
pub const MAX_ACK_NOTE_BYTES: usize = 4096;

pub use contract::{
    Checkpoint, IncidentAcknowledgement, IncidentError, IncidentKind, IncidentRecord,
    IncidentSignal, IncidentStatus, IncidentStoreConfig, MonitorCommit, SignalCondition,
};

type IncidentKey = (String, String, String, IncidentKind);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Monitor {
        commit: MonitorCommit,
    },
    Acknowledge {
        id: String,
        expected_revision: u64,
        actor: String,
        note: String,
        now_ms: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalEntry {
    format: u32,
    sequence: u64,
    event: Event,
}

struct Prepared {
    records: Vec<IncidentRecord>,
    monitor: Option<MonitorState>,
}

struct MonitorState {
    commit: MonitorCommit,
    updated_at_ms: u64,
}

/// Single synchronous writer. The monitor engine owns this through one mutex;
/// read projections and acknowledgement use the same authority and lock.
pub struct IncidentStore {
    journal: Option<File>,
    path: PathBuf,
    _directories: Vec<File>,
    config: IncidentStoreConfig,
    bytes: u64,
    sequence: u64,
    poisoned: bool,
    records: BTreeMap<String, IncidentRecord>,
    active: BTreeMap<IncidentKey, String>,
    monitors: BTreeMap<String, MonitorState>,
}

impl IncidentStore {
    pub fn checkpoint(&self, monitor_id: &str) -> Option<Checkpoint> {
        self.monitors.get(monitor_id).map(|state| Checkpoint {
            sequence: state.commit.sequence,
            value: state.commit.checkpoint.clone(),
            updated_at_ms: state.updated_at_ms,
        })
    }

    /// Starts at one and advances exactly one sequence per monitor. Only the
    /// latest identical full commit can be retried without another journal write.
    pub fn commit(&mut self, commit: MonitorCommit) -> Result<(), IncidentError> {
        self.append(Event::Monitor { commit })
    }

    pub fn get(&self, id: &str) -> Option<IncidentRecord> {
        self.records.get(id).cloned()
    }

    pub fn list(&self) -> Vec<IncidentRecord> {
        self.map_records(Clone::clone)
    }

    /// Projects summaries without cloning every stored evidence object.
    pub fn map_records<T>(&self, projection: impl FnMut(&IncidentRecord) -> T) -> Vec<T> {
        self.records.values().map(projection).collect()
    }

    /// The caller supplies its authenticated host identity. Acknowledgement
    /// records attention only; it does not clear the condition or grant actions.
    pub fn acknowledge(
        &mut self,
        id: &str,
        expected_revision: u64,
        actor: &str,
        note: &str,
        now_ms: u64,
    ) -> Result<IncidentRecord, IncidentError> {
        self.append(Event::Acknowledge {
            id: id.into(),
            expected_revision,
            actor: actor.into(),
            note: note.into(),
            now_ms,
        })?;
        self.get(id)
            .ok_or_else(|| IncidentError::NotFound(id.into()))
    }
}

fn next(value: u64) -> Result<u64, IncidentError> {
    value
        .checked_add(1)
        .ok_or_else(|| IncidentError::Capacity("sequence overflow".into()))
}

#[cfg(test)]
#[path = "../../tests/incident_storage.rs"]
mod storage_tests;
