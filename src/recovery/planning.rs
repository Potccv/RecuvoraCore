//! Deterministic request construction. Descriptive constraints do not authorize actions.
use super::{BusinessError, knowledge::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const FAULT_FINGERPRINT_CONDITION: &str = "fault_fingerprint";
fn invalid(reason: &str) -> BusinessError {
    BusinessError::Invalid(reason.into())
}
fn text(value: &str, max: usize) -> Result<(), BusinessError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        Err(invalid("empty, oversized or NUL-containing value"))
    } else {
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TargetBinding {
    pub target_id: String,
    /// Stable logical executor identity; no transport or routing configuration.
    pub executor_id: String,
    pub allowed_action_kinds: Vec<String>,
    pub verification_profile: String,
    pub required_facts: BTreeMap<String, String>,
    pub action_timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProblemContext {
    /// Describes the factual intake contract, never execution permission.
    #[serde(default, skip_serializing_if = "ProblemOrigin::is_incident")]
    pub origin: ProblemOrigin,
    /// Original bounded report evidence; the raw log text remains in summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<ErrorLogEvidence>,
    pub incident_id: String,
    pub incident_revision: u64,
    pub target_id: String,
    pub fingerprint: String,
    pub summary: String,
    /// Abnormal observation samples within this incident, not repeated episodes.
    pub occurrences: u64,
    pub keywords: Vec<String>,
    pub conditions: BTreeMap<String, String>,
    pub evidence_refs: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProblemOrigin {
    #[default]
    Incident,
    /// An immutable received error report, not a claim of current target health.
    ErrorLog,
}
impl ProblemOrigin {
    fn is_incident(&self) -> bool {
        *self == Self::Incident
    }
}

/// Descriptive source identity and original payload, never a provider route or
/// current-health assertion. Relative age is retained as reported, not compared
/// with the caller's clock or used to discard historical reports.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ErrorLogEvidence {
    pub source_id: String,
    pub generation: String,
    pub record_id: String,
    pub sequence: u64,
    pub age_ms: u64,
    pub evidence: serde_json::Value,
}
impl ErrorLogEvidence {
    pub fn validate(&self) -> Result<(), BusinessError> {
        for value in [&self.source_id, &self.generation, &self.record_id] {
            text(value, 128)?;
        }
        if self.sequence == 0 || !self.evidence.is_object() {
            return Err(invalid(
                "error report requires sequence and evidence object",
            ));
        }
        if serde_json::to_vec(&self.evidence)
            .map_err(|_| invalid("error report evidence cannot be encoded"))?
            .len()
            > 4096
        {
            return Err(BusinessError::Capacity);
        }
        let mut pending = vec![(&self.evidence, 0)];
        while let Some((value, depth)) = pending.pop() {
            if depth > 24 {
                return Err(invalid("error report evidence nesting exceeds 24"));
            }
            match value {
                serde_json::Value::Array(values) => {
                    pending.extend(values.iter().map(|value| (value, depth + 1)));
                }
                serde_json::Value::Object(values) => {
                    pending.extend(values.values().map(|value| (value, depth + 1)));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl ProblemContext {
    pub fn validate(&self) -> Result<(), BusinessError> {
        match (self.origin, &self.report) {
            (ProblemOrigin::Incident, None) => {}
            (ProblemOrigin::ErrorLog, Some(report)) => report.validate()?,
            _ => return Err(invalid("problem origin and report evidence differ")),
        }
        for value in [&self.incident_id, &self.target_id, &self.fingerprint] {
            text(value, 128)?;
        }
        text(&self.summary, 8192)?;
        if self.incident_revision == 0 || self.occurrences == 0 || self.keywords.len() > 32 {
            return Err(BusinessError::Invalid(
                "invalid problem revision or limits".into(),
            ));
        }
        let mut words = BTreeSet::new();
        for word in &self.keywords {
            text(word, 128)?;
            if !words.insert(word) {
                return Err(BusinessError::Invalid("duplicate problem keyword".into()));
            }
        }
        facts(&self.conditions)?;
        if self.conditions.contains_key(FAULT_FINGERPRINT_CONDITION) {
            return Err(invalid("fault fingerprint condition is reserved"));
        }
        evidence(&self.evidence_refs)
    }
}

pub fn stable_conditions(
    problem: &ProblemContext,
    target: &TargetBinding,
) -> Result<BTreeMap<String, String>, BusinessError> {
    if problem.conditions.contains_key(FAULT_FINGERPRINT_CONDITION)
        || target
            .required_facts
            .contains_key(FAULT_FINGERPRINT_CONDITION)
    {
        return Err(invalid("fault fingerprint condition is reserved"));
    }
    let mut values = problem.conditions.clone();
    for (key, value) in &target.required_facts {
        if values.get(key).is_some_and(|existing| existing != value) {
            return Err(invalid("problem and target conditions conflict"));
        }
        values.insert(key.clone(), value.clone());
    }
    values.insert(
        FAULT_FINGERPRINT_CONDITION.into(),
        problem.fingerprint.clone(),
    );
    facts(&values)?;
    Ok(values)
}

pub(super) fn facts(values: &BTreeMap<String, String>) -> Result<(), BusinessError> {
    if values.len() > 32 {
        return Err(BusinessError::Capacity);
    }
    for (key, value) in values {
        text(key, 128)?;
        text(value, 1024)?;
    }
    Ok(())
}
pub(super) fn evidence(values: &[String]) -> Result<(), BusinessError> {
    if values.is_empty() || values.len() > 32 {
        return Err(BusinessError::Invalid(
            "evidence required, maximum 32 references".into(),
        ));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        text(value, 1024)?;
        if !unique.insert(value) {
            return Err(BusinessError::Invalid(
                "duplicate evidence reference".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetObservation {
    pub target_id: String,
    pub facts: BTreeMap<String, String>,
    pub evidence_refs: Vec<String>,
    pub observed_at_ms: u64,
}

/// Maximum serialized repair request, including fault, observation and references.
pub const MAX_REPAIR_REQUEST_BYTES: usize = 32 * 1024;

/// A single bounded repair session. Experience is reference material, not authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRepairRequest {
    pub problem: ProblemContext,
    pub observation: TargetObservation,
    pub matched_experience_count: usize,
    pub experiences: Vec<RepairExperience>,
    pub harness_id: String,
    pub delegation: String,
    pub target: TargetBinding,
    pub max_tool_calls: usize,
    pub summarize_experience: bool,
    pub assess_scriptability: bool,
}

impl HarnessRepairRequest {
    pub fn within_size_limit(&self) -> Result<bool, BusinessError> {
        serde_json::to_vec(self)
            .map(|encoded| encoded.len() <= MAX_REPAIR_REQUEST_BYTES)
            .map_err(|_| invalid("cannot encode repair request"))
    }

    fn within_size_limit_with(
        &self,
        experiences: &[&RepairExperience],
    ) -> Result<bool, BusinessError> {
        #[derive(Serialize)]
        struct RequestView<'a> {
            problem: &'a ProblemContext,
            observation: &'a TargetObservation,
            matched_experience_count: usize,
            experiences: &'a [&'a RepairExperience],
            harness_id: &'a str,
            delegation: &'a str,
            target: &'a TargetBinding,
            max_tool_calls: usize,
            summarize_experience: bool,
            assess_scriptability: bool,
        }

        let view = RequestView {
            problem: &self.problem,
            observation: &self.observation,
            matched_experience_count: self.matched_experience_count,
            experiences,
            harness_id: &self.harness_id,
            delegation: &self.delegation,
            target: &self.target,
            max_tool_calls: self.max_tool_calls,
            summarize_experience: self.summarize_experience,
            assess_scriptability: self.assess_scriptability,
        };
        serde_json::to_vec(&view)
            .map(|encoded| encoded.len() <= MAX_REPAIR_REQUEST_BYTES)
            .map_err(|_| invalid("cannot encode repair request"))
    }
}

/// Explicit business input. Routing, freshness and policy are managed by the caller.
pub struct RepairRequestInput {
    pub problem: ProblemContext,
    pub observation: TargetObservation,
    pub harness_id: String,
    pub delegation: String,
    pub target: TargetBinding,
    pub max_tool_calls: usize,
}

/// Uses one path for known and unknown faults. Whole experiences are selected
/// within four records and 32 KiB; skipped large records still count as matches.
pub fn prepare_repair<'a>(
    input: RepairRequestInput,
    experiences: impl IntoIterator<Item = &'a RepairExperience>,
) -> Result<HarnessRepairRequest, BusinessError> {
    input.problem.validate()?;
    let query = KnowledgeQuery {
        conditions: stable_conditions(&input.problem, &input.target)?,
        keywords: input.problem.keywords.clone(),
        limit: 4,
    };
    let matches = matching_experiences(&query, experiences)?;
    let mut request = HarnessRepairRequest {
        problem: input.problem,
        observation: input.observation,
        matched_experience_count: matches.len(),
        experiences: Vec::new(),
        harness_id: input.harness_id,
        delegation: input.delegation,
        target: input.target,
        max_tool_calls: input.max_tool_calls,
        summarize_experience: true,
        assess_scriptability: true,
    };
    if !request.within_size_limit()? {
        return Err(invalid("repair request exceeds context budget"));
    }
    let mut selected = Vec::with_capacity(4);
    for experience in matches {
        if selected.len() == 4 {
            break;
        }
        selected.push(experience);
        if !request.within_size_limit_with(&selected)? {
            selected.pop();
        }
    }
    request.experiences = selected.into_iter().cloned().collect();
    Ok(request)
}
