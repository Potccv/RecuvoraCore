use super::*;
impl KnowledgeQuery {
    pub fn validate(&self) -> Result<(), BusinessError> {
        validation::query(self)
    }
}

/// Matches all applicable records and sorts newest first, then by identity.
/// The query limit is applied by consumers after matching, so total count is preserved.
pub fn matching_experiences<'a>(
    query: &KnowledgeQuery,
    experiences: impl IntoIterator<Item = &'a RepairExperience>,
) -> Result<Vec<&'a RepairExperience>, BusinessError> {
    query.validate()?;
    let mut matches: Vec<_> = experiences
        .into_iter()
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
