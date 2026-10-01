//! Bounded domain queries for operational callers, with no presentation or script source.
use super::*;

#[derive(Clone, Debug)]
pub struct RecoveryQuery {
    pub stages: Vec<RecoveryStage>,
    pub after_id: Option<String>,
    pub limit: usize,
}
impl RecoveryQuery {
    pub(super) fn validate(&self) -> Result<(), RecoveryError> {
        if !(1..=100).contains(&self.limit)
            || self.stages.len() > 12
            || self.stages.iter().collect::<BTreeSet<_>>().len() != self.stages.len()
        {
            return Err(RecoveryError::Invalid(
                "invalid operational query limits".into(),
            ));
        }
        if let Some(id) = &self.after_id {
            text(id, 128)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryTaskSummary {
    pub task_id: String,
    pub revision: u64,
    pub incident_id: String,
    pub target_id: String,
    pub stage: RecoveryStage,
    pub sample_count: u64,
    pub episode_count: u64,
    pub diagnosis_attempts: u32,
    pub approval_id: Option<String>,
    pub operation_id: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
impl From<&RecoveryTask> for RecoveryTaskSummary {
    fn from(task: &RecoveryTask) -> Self {
        Self {
            task_id: task.id.clone(),
            revision: task.revision,
            incident_id: task.problem.incident_id.clone(),
            target_id: task.problem.target_id.clone(),
            stage: task.stage.clone(),
            sample_count: task.problem.occurrences,
            episode_count: task.episode_count,
            diagnosis_attempts: task.diagnosis_attempts,
            approval_id: task.approval_id.clone(),
            operation_id: task
                .operation
                .as_ref()
                .map(|operation| operation.operation_id.clone()),
            created_at_ms: task.created_at_ms,
            updated_at_ms: task.updated_at_ms,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryOverview {
    pub task_count: usize,
    pub task_limit: usize,
    pub journal_bytes: u64,
    pub journal_limit_bytes: u64,
    pub stages: BTreeMap<RecoveryStage, usize>,
    pub unknown_approvals: usize,
}
