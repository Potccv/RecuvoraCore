//! External knowledge remains untrusted diagnostic evidence.
use super::{validation, *};
use std::collections::BTreeSet;

impl KnowledgeState {
    /// Validates a bounded batch against the Host-bound source identity and local
    /// immutable versions. Inapplicable and quarantined proposals are excluded;
    /// malformed or conflicting evidence rejects the whole batch. No write occurs.
    pub fn validate_external_candidates(
        &self,
        expected_source_id: &str,
        query: &KnowledgeQuery,
        proposals: Vec<KnowledgeProposal>,
    ) -> Result<Vec<KnowledgeCandidate>, KnowledgeError> {
        validation::text(expected_source_id, 128, "knowledge source identity")?;
        validation::query(query)?;
        if proposals.len() > query.limit {
            return Err(KnowledgeError::Capacity("external candidate count".into()));
        }
        let mut bytes = 0usize;
        let mut ids = BTreeSet::new();
        let mut scripts = std::collections::BTreeMap::new();
        let mut accepted = Vec::new();
        for proposal in proposals {
            if proposal.source_id != expected_source_id {
                return Err(KnowledgeError::Conflict(
                    "knowledge source identity changed".into(),
                ));
            }
            validation::candidate(&proposal.candidate)?;
            validation::evidence(&proposal.evidence_refs)?;
            bytes = bytes
                .checked_add(
                    serde_json::to_vec(&proposal)
                        .map_err(|error| KnowledgeError::Invalid(error.to_string()))?
                        .len(),
                )
                .filter(|total| *total <= MAX_EXTERNAL_KNOWLEDGE_BYTES)
                .ok_or_else(|| KnowledgeError::Capacity("external candidate bytes".into()))?;
            let mut candidate = proposal.candidate;
            if !ids.insert(candidate.id.clone()) {
                return Err(KnowledgeError::Conflict(
                    "duplicate external candidate identity".into(),
                ));
            }
            candidate.reusable = false;
            let references: BTreeSet<_> = candidate
                .evidence_refs
                .into_iter()
                .chain(proposal.evidence_refs)
                .chain([format!("knowledge-source:{expected_source_id}")])
                .collect();
            candidate.evidence_refs = references.into_iter().collect();
            validation::candidate(&candidate)?;
            if self
                .records
                .get(&candidate.id)
                .is_some_and(|old| old.candidate != candidate)
            {
                return Err(KnowledgeError::Conflict(
                    "external candidate changes a local identity".into(),
                ));
            }
            self.validate_script(&candidate.script)?;
            let key = (candidate.script.id.clone(), candidate.script.version);
            if scripts
                .get(&key)
                .is_some_and(|previous| previous != &candidate.script)
            {
                return Err(KnowledgeError::Conflict(
                    "external script version content changed".into(),
                ));
            }
            scripts.insert(key.clone(), candidate.script.clone());
            if self.quarantined.contains(&key) || !applicable(&candidate, query) {
                continue;
            }
            accepted.push(candidate);
        }
        accepted.sort_by(|left, right| {
            exact_condition_count(right)
                .cmp(&exact_condition_count(left))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(accepted)
    }
}

pub(super) fn applicable(candidate: &KnowledgeCandidate, query: &KnowledgeQuery) -> bool {
    candidate
        .conditions
        .iter()
        .chain(&candidate.script.preconditions)
        .all(|(key, expected)| query.conditions.get(key) == Some(expected))
        && query
            .keywords
            .iter()
            .all(|keyword| candidate.keywords.contains(keyword))
}

pub(super) fn exact_condition_count(candidate: &KnowledgeCandidate) -> usize {
    candidate
        .conditions
        .keys()
        .chain(candidate.script.preconditions.keys())
        .collect::<BTreeSet<_>>()
        .len()
}
