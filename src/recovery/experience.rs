//! Construct experience from explicit outcome and evidence supplied by the caller.
use super::{BusinessError, knowledge::*, planning::*};

pub struct ExperienceInput<'a> {
    pub id: &'a str,
    pub operation_id: &'a str,
    pub problem: &'a ProblemContext,
    pub target: &'a TargetBinding,
    pub outcome: RepairOutcome,
    pub actions: &'a [RepairArtifact],
    pub evidence_refs: &'a [String],
    pub recorded_at_ms: u64,
    pub report: &'a ExperienceReport,
}

/// Preserves the caller's outcome and actual actions independently of model scriptability.
/// No persistence, attestation, retries or trust decision is performed here.
pub fn build_experience(input: ExperienceInput<'_>) -> Result<RepairExperience, BusinessError> {
    Ok(RepairExperience {
        id: input.id.into(),
        operation_id: input.operation_id.into(),
        target_id: input.problem.target_id.clone(),
        conditions: stable_conditions(input.problem, input.target)?,
        keywords: input.problem.keywords.clone(),
        outcome: input.outcome,
        actions: input.actions.to_vec(),
        evidence_refs: input.evidence_refs.to_vec(),
        recorded_at_ms: input.recorded_at_ms,
        report: input.report.clone(),
    })
}
