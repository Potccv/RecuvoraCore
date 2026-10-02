//! Repair experience is independent of whether the repair can become a script.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "assessment", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scriptability {
    Possible {
        reason: String,
        candidate: Option<RepairArtifact>,
    },
    NotSuitable {
        reason: String,
    },
    Undetermined {
        reason: String,
    },
}

/// Model output: a proposal only, never proof of business health or script safety.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceReport {
    pub summary: String,
    pub lessons: String,
    /// IDs of the supplied experiences that were reused or corrected.
    pub related_experience_ids: Vec<String>,
    pub scriptability: Scriptability,
}
impl ExperienceReport {
    pub fn validate(&self) -> Result<(), KnowledgeError> {
        validation::text(&self.summary, 4096, "experience summary")?;
        validation::text(&self.lessons, 8192, "experience lessons")?;
        let ids: std::collections::BTreeSet<_> = self.related_experience_ids.iter().collect();
        if ids.len() != self.related_experience_ids.len() || ids.len() > 4 {
            return Err(KnowledgeError::Invalid(
                "invalid related experiences".into(),
            ));
        }
        for id in ids {
            validation::text(id, 256, "experience identity")?;
        }
        let reason = match &self.scriptability {
            Scriptability::Possible { reason, candidate } => {
                if let Some(artifact) = candidate {
                    artifact.validate()?;
                }
                reason
            }
            Scriptability::NotSuitable { reason } | Scriptability::Undetermined { reason } => {
                reason
            }
        };
        validation::text(reason, 4096, "scriptability reason")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairExperience {
    pub id: String,
    pub operation_id: String,
    pub target_id: String,
    pub conditions: BTreeMap<String, String>,
    pub keywords: Vec<String>,
    pub outcome: RepairOutcome,
    pub evidence_refs: Vec<String>,
    pub recorded_at_ms: u64,
    pub actions: Vec<RepairArtifact>,
    pub report: ExperienceReport,
}

/// Only trusted Host code may attest a persisted result; model JSON cannot do so.
#[derive(Clone, Debug, Serialize)]
pub struct TrustedRepairExperience(Box<RepairExperience>);
impl TrustedRepairExperience {
    pub fn attest(experience: RepairExperience) -> Result<Self, KnowledgeError> {
        experience.report.validate()?;
        if experience.actions.len() > 1 {
            return Err(KnowledgeError::Invalid(
                "at most one repair action is allowed".into(),
            ));
        }
        for action in &experience.actions {
            action.validate()?;
        }
        for id in [
            &experience.id,
            &experience.operation_id,
            &experience.target_id,
        ] {
            validation::text(id, 256, "repair identity")?;
        }
        validation::query(&KnowledgeQuery {
            conditions: experience.conditions.clone(),
            keywords: experience.keywords.clone(),
            limit: 1,
        })?;
        validation::evidence(&experience.evidence_refs)?;
        Ok(Self(Box::new(experience)))
    }
    pub fn record(&self) -> &RepairExperience {
        &self.0
    }
}

impl KnowledgeState {
    pub(crate) fn matching_experiences<'a>(
        &'a self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<&'a RepairExperience>, KnowledgeError> {
        validation::query(query)?;
        let mut matches: Vec<_> = self
            .experiences
            .values()
            .filter(|item| {
                item.conditions
                    .iter()
                    .all(|(key, value)| query.conditions.get(key) == Some(value))
                    && query
                        .keywords
                        .iter()
                        .all(|word| item.keywords.contains(word))
            })
            .collect();
        matches.sort_by(|a, b| {
            b.recorded_at_ms
                .cmp(&a.recorded_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(matches)
    }

    /// Exact applicable experience, including failures as explicitly labelled evidence.
    /// No returned item grants permission to execute a candidate script.
    pub fn search_experiences(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<RepairExperience>, KnowledgeError> {
        Ok(self
            .matching_experiences(query)?
            .into_iter()
            .take(query.limit)
            .cloned()
            .collect())
    }
}
