//! Bounded neutral content; concrete action formats belong to the caller.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn text(value: &str, max: usize, label: &str) -> Result<(), BusinessError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(BusinessError::Invalid(format!(
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
) -> Result<(), BusinessError> {
    if values.len() > max_items {
        return Err(BusinessError::Invalid(format!("too many {label}")));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        text(value, max_bytes, label)?;
        if !seen.insert(value) {
            return Err(BusinessError::Invalid(format!("duplicate {label}")));
        }
    }
    Ok(())
}

fn conditions(values: &BTreeMap<String, String>) -> Result<(), BusinessError> {
    if values.is_empty() || values.len() > 32 {
        return Err(BusinessError::Invalid(
            "conditions require 1..32 exact entries".into(),
        ));
    }
    for (key, value) in values {
        text(key, 128, "condition key")?;
        text(value, 1024, "condition value")?;
    }
    Ok(())
}

pub(crate) fn artifact(value: &RepairArtifact) -> Result<(), BusinessError> {
    text(&value.id, 128, "artifact id")?;
    if value.version == 0 {
        return Err(BusinessError::Invalid(
            "artifact version must be positive".into(),
        ));
    }
    text(&value.kind, 128, "artifact kind")?;
    let payload = serde_json::to_vec(&value.payload)
        .map_err(|error| BusinessError::Invalid(error.to_string()))?;
    if payload.len() > MAX_ARTIFACT_BYTES {
        return Err(BusinessError::Invalid(
            "artifact payload exceeds bounds".into(),
        ));
    }
    conditions(&value.preconditions)?;
    text(&value.generated_by_harness, 128, "generating Harness")?;
    text(&value.generated_in_session, 256, "generating session")
}

pub(crate) fn query(value: &KnowledgeQuery) -> Result<(), BusinessError> {
    conditions(&value.conditions)?;
    strings(&value.keywords, 32, 128, "query keywords")?;
    if !(1..=100).contains(&value.limit) {
        return Err(BusinessError::Invalid("query limit must be 1..100".into()));
    }
    Ok(())
}

pub(crate) fn evidence(values: &[String]) -> Result<(), BusinessError> {
    if values.is_empty() {
        return Err(BusinessError::Invalid("evidence required".into()));
    }
    strings(values, 32, 1024, "evidence")
}
