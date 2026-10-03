use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Verified,
    Failed,
    Unknown,
}

/// Every experience condition must exactly match a supplied condition. Keywords
/// use case-sensitive AND matching. At least one condition must be supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnowledgeQuery {
    pub conditions: BTreeMap<String, String>,
    pub keywords: Vec<String>,
    pub limit: usize,
}
