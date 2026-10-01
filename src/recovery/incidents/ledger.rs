//! Host transaction proposals and strictly validated event restoration.
use super::*;

impl IncidentLedger {
    pub fn new(config: IncidentLimits) -> Result<Self, IncidentError> {
        config.validate()?;
        Ok(Self {
            config,
            sequence: 0,
            history: Vec::new(),
            records: BTreeMap::new(),
            active: BTreeMap::new(),
            monitors: BTreeMap::new(),
        })
    }
    pub fn revision(&self) -> u64 {
        self.sequence
    }
    pub fn entries(&self) -> &[IncidentEntry] {
        &self.history
    }

    pub fn restore(
        config: IncidentLimits,
        entries: &[IncidentEntry],
    ) -> Result<Self, IncidentError> {
        let mut ledger = Self::new(config)?;
        for entry in entries {
            ledger
                .apply_entry(entry.clone())
                .map_err(|error| IncidentError::Corrupt(error.to_string()))?;
        }
        Ok(ledger)
    }

    /// Returns None for an exact retry of the latest monitor commit. The Host
    /// must ensure this aggregate is current before acknowledging that retry.
    pub fn prepare_monitor(
        &self,
        commit_id: String,
        commit: MonitorCommit,
    ) -> Result<Option<Prepared<Self>>, IncidentError> {
        self.prepare_event(commit_id, IncidentEvent::Monitor { commit })
    }

    /// Attention attribution does not resolve the condition or grant authority.
    pub fn prepare_acknowledge(
        &self,
        commit_id: String,
        id: String,
        expected_revision: u64,
        actor: String,
        note: String,
        now_ms: u64,
    ) -> Result<Prepared<Self>, IncidentError> {
        self.prepare_event(
            commit_id,
            IncidentEvent::Acknowledge {
                id,
                expected_revision,
                actor,
                note,
                now_ms,
            },
        )?
        .ok_or_else(|| IncidentError::Conflict("acknowledgement unexpectedly unchanged".into()))
    }

    fn prepare_event(
        &self,
        commit_id: String,
        event: IncidentEvent,
    ) -> Result<Option<Prepared<Self>>, IncidentError> {
        if self
            .history
            .iter()
            .any(|entry| entry.commit_id == commit_id)
        {
            return Err(IncidentError::Conflict("commit identity reused".into()));
        }
        let input = serde_json::json!({"config":self.config,"history":self.history,"event":event});
        let sequence = next(self.sequence)?;
        let Some(transition) = self.compute(&event, sequence)? else {
            return Ok(None);
        };
        let mut proposed = self.clone();
        proposed.install(transition);
        proposed.sequence = sequence;
        proposed.history.push(IncidentEntry {
            commit_id: commit_id.clone(),
            sequence,
            event,
        });
        Ok(Some(Prepared::new_bound(
            commit_id,
            self.sequence,
            "incidents".into(),
            input,
            proposed,
            Vec::new(),
        )?))
    }

    fn apply_entry(&mut self, entry: IncidentEntry) -> Result<(), IncidentError> {
        if !crate::identity::valid_id(&entry.commit_id) {
            return Err(IncidentError::Invalid("commit identity".into()));
        }
        if self
            .history
            .iter()
            .any(|previous| previous.commit_id == entry.commit_id)
        {
            return Err(IncidentError::Corrupt("duplicate commit identity".into()));
        }
        if next(self.sequence)? != entry.sequence {
            return Err(IncidentError::Corrupt("sequence gap".into()));
        }
        let transition = self
            .compute(&entry.event, entry.sequence)?
            .ok_or_else(|| IncidentError::Corrupt("duplicate committed event".into()))?;
        self.install(transition);
        self.sequence = entry.sequence;
        self.history.push(entry);
        Ok(())
    }
}
