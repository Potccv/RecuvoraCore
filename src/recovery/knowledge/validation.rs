//! Bounded neutral data, immutable-script identity and verification binding checks.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn text(value: &str, max: usize, label: &str) -> Result<(), KnowledgeError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(KnowledgeError::Invalid(format!(
            "{label} is empty or exceeds bounds"
        )));
    }
    Ok(())
}

fn strings(
    values: &[String],
    max_items: usize,
    max_bytes: usize,
    label: &str,
) -> Result<(), KnowledgeError> {
    if values.len() > max_items {
        return Err(KnowledgeError::Invalid(format!("too many {label}")));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        text(value, max_bytes, label)?;
        if !seen.insert(value) {
            return Err(KnowledgeError::Invalid(format!("duplicate {label}")));
        }
    }
    Ok(())
}

fn conditions(values: &BTreeMap<String, String>) -> Result<(), KnowledgeError> {
    if values.is_empty() || values.len() > 32 {
        return Err(KnowledgeError::Invalid(
            "conditions require 1..32 exact entries".into(),
        ));
    }
    for (key, value) in values {
        text(key, 128, "condition key")?;
        text(value, 1024, "condition value")?;
    }
    Ok(())
}

pub(super) fn evidence(values: &[String]) -> Result<(), KnowledgeError> {
    if values.is_empty() {
        return Err(KnowledgeError::Invalid(
            "at least one evidence reference required".into(),
        ));
    }
    strings(values, 32, 1024, "evidence references")
}

pub(super) fn candidate(value: &KnowledgeCandidate) -> Result<(), KnowledgeError> {
    text(&value.id, 128, "record id")?;
    text(&value.incident_id, 256, "incident id")?;
    text(&value.summary, 4096, "summary")?;
    strings(&value.keywords, 32, 128, "keywords")?;
    conditions(&value.conditions)?;
    evidence(&value.evidence_refs)?;
    script(&value.script)?;
    for (key, expected) in &value.script.preconditions {
        if value
            .conditions
            .get(key)
            .is_some_and(|actual| actual != expected)
        {
            return Err(KnowledgeError::Invalid(
                "candidate and script preconditions conflict".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn script(script: &ScriptArtifact) -> Result<(), KnowledgeError> {
    text(&script.id, 128, "script id")?;
    if script.version == 0 {
        return Err(KnowledgeError::Invalid(
            "script version must be positive".into(),
        ));
    }
    if !matches!(script.language.as_str(), "powershell" | "sh" | "python") {
        return Err(KnowledgeError::Invalid(
            "unsupported script language label".into(),
        ));
    }
    text(&script.platform, 64, "platform")?;
    text(&script.source, MAX_SCRIPT_BYTES, "script source")?;
    text(&script.generated_by_harness, 128, "generating Harness")?;
    text(&script.generated_in_session, 256, "generating session")?;
    conditions(&script.preconditions)?;
    Ok(())
}

pub(super) fn verification(value: &BusinessVerificationRecord) -> Result<(), KnowledgeError> {
    text(&value.operation_id, 256, "verification operation")?;
    text(&value.target_id, 256, "verification target")?;
    text(&value.script_id, 128, "verification script")?;
    text(&value.verifier_id, 256, "trusted verifier")?;
    if value.script_version == 0 {
        return Err(KnowledgeError::Invalid(
            "verification script version must be positive".into(),
        ));
    }
    evidence(&value.evidence_refs)
}

pub(super) fn case(
    value: &RepairCase,
    proof: Option<&BusinessVerificationRecord>,
    script: &ScriptArtifact,
) -> Result<(), KnowledgeError> {
    for (field, label) in [
        (&value.id, "case id"),
        (&value.operation_id, "operation id"),
        (&value.target_id, "target id"),
    ] {
        text(field, 256, label)?;
    }
    evidence(&value.evidence_refs)?;
    if value.script_id != script.id || value.script_version != script.version {
        return Err(KnowledgeError::Conflict(
            "case refers to another script version".into(),
        ));
    }
    match (value.outcome, proof) {
        (RepairOutcome::Verified, Some(proof)) => {
            verification(proof)?;
            if proof.operation_id != value.operation_id
                || proof.target_id != value.target_id
                || proof.script_id != value.script_id
                || proof.script_version != value.script_version
                || proof.verified_at_ms > value.recorded_at_ms
            {
                return Err(KnowledgeError::Conflict(
                    "business verification does not match operation, target, script or time".into(),
                ));
            }
        }
        (RepairOutcome::Verified, None) => {
            return Err(KnowledgeError::Invalid(
                "verified outcome requires independent trusted business verification".into(),
            ));
        }
        (_, Some(_)) => {
            return Err(KnowledgeError::Invalid(
                "non-verified outcome cannot carry successful verification".into(),
            ));
        }
        (_, None) => {}
    }
    Ok(())
}

pub(super) fn query(value: &KnowledgeQuery) -> Result<(), KnowledgeError> {
    conditions(&value.conditions)?;
    strings(&value.keywords, 32, 128, "query keywords")?;
    if !(1..=100).contains(&value.limit) {
        return Err(KnowledgeError::Invalid("query limit must be 1..100".into()));
    }
    Ok(())
}
