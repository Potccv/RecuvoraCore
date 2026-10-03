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
    pub fn validate(&self) -> Result<(), BusinessError> {
        validation::text(&self.summary, 4096, "experience summary")?;
        validation::text(&self.lessons, 8192, "experience lessons")?;
        let ids: std::collections::BTreeSet<_> = self.related_experience_ids.iter().collect();
        if ids.len() != self.related_experience_ids.len() || ids.len() > 4 {
            return Err(BusinessError::Invalid("invalid related experiences".into()));
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
