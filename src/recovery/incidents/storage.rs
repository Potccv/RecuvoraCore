//! Exclusive journal ownership, replay and sync-before-memory transaction commit.
use super::*;
use fs2::FileExt;
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
};

impl IncidentStore {
    /// `path` is an absolute journal file path outside the project source tree.
    pub fn open(
        path: impl AsRef<Path>,
        config: IncidentStoreConfig,
    ) -> Result<Self, IncidentError> {
        config.validate()?;
        let path = path.as_ref().to_path_buf();
        let (journal, directories) = paths::open(&path)?;
        journal
            .try_lock_exclusive()
            .map_err(|error| IncidentError::Unavailable(format!("journal writer lock: {error}")))?;
        let bytes = journal.metadata()?.len();
        if bytes > config.max_journal_bytes {
            return Err(IncidentError::Capacity("journal bytes".into()));
        }
        let mut store = Self {
            journal: Some(journal),
            path,
            _directories: directories,
            config,
            bytes,
            sequence: 0,
            poisoned: false,
            records: BTreeMap::new(),
            active: BTreeMap::new(),
            monitors: BTreeMap::new(),
        };
        store.replay()?;
        Ok(store)
    }

    /// Releases storage ownership while keeping the final in-memory read view.
    /// All accepted events have already been synced; closing never accepts more.
    pub fn close(&mut self) -> Result<(), IncidentError> {
        self.poisoned = true;
        self.journal.take();
        self._directories.clear();
        Ok(())
    }

    pub(super) fn append(&mut self, event: Event) -> Result<(), IncidentError> {
        if self.poisoned {
            return Err(IncidentError::Unavailable(
                "previous write failed; reopen after checking storage".into(),
            ));
        }
        let sequence = next(self.sequence)?;
        let Some(prepared) = self.prepare(&event, sequence)? else {
            return Ok(());
        };
        let entry = JournalEntry {
            format: 1,
            sequence,
            event,
        };
        let mut bytes = serde_json::to_vec(&entry)
            .map_err(|error| IncidentError::Invalid(error.to_string()))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(IncidentError::Capacity(
                "single journal record bytes".into(),
            ));
        }
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|bytes| *bytes <= self.config.max_journal_bytes)
            .ok_or_else(|| IncidentError::Capacity("journal bytes".into()))?;
        // A failed append may have written a partial record. Stop all subsequent
        // mutations; never advance in-memory facts or the monitor checkpoint.
        self.poisoned = true;
        let journal = self
            .journal
            .as_mut()
            .ok_or_else(|| IncidentError::Unavailable("store closed".into()))?;
        paths::validate_current(&self.path, journal)?;
        if journal.metadata()?.len() != self.bytes {
            return Err(IncidentError::Unavailable(
                "journal length changed externally".into(),
            ));
        }
        journal.write_all(&bytes)?;
        journal.sync_data()?;
        self.install(prepared);
        self.sequence = sequence;
        self.bytes = total;
        self.poisoned = false;
        Ok(())
    }

    fn replay(&mut self) -> Result<(), IncidentError> {
        // read_until is bounded by Take, so a malicious oversized line cannot
        // allocate up to the total journal limit before rejection.
        use std::io::Read;
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| IncidentError::Unavailable("store closed".into()))?;
        let mut reader = BufReader::new(journal.try_clone()?);
        let mut read_bytes = 0u64;
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
                return Err(IncidentError::Corrupt(
                    "oversized or incomplete record".into(),
                ));
            }
            read_bytes += count as u64;
            if read_bytes > self.config.max_journal_bytes {
                return Err(IncidentError::Capacity(
                    "journal bytes during replay".into(),
                ));
            }
            let entry: JournalEntry = serde_json::from_slice(&bytes)
                .map_err(|error| IncidentError::Corrupt(error.to_string()))?;
            if entry.format != 1 || entry.sequence != next(self.sequence)? {
                return Err(IncidentError::Corrupt("format or journal sequence".into()));
            }
            let prepared = self
                .prepare(&entry.event, entry.sequence)
                .map_err(|error| IncidentError::Corrupt(error.to_string()))?
                .ok_or_else(|| IncidentError::Corrupt("duplicate committed event".into()))?;
            self.install(prepared);
            self.sequence = entry.sequence;
        }
        if read_bytes != self.bytes {
            return Err(IncidentError::Corrupt(
                "journal length changed during replay".into(),
            ));
        }
        Ok(())
    }
}
