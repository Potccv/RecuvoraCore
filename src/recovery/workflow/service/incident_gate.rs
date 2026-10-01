//! Trusted read-only fault validity checks before a workflow consumes authority.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncidentReadiness {
    Active { revision: u64 },
    Resolved { revision: u64 },
    Unavailable { reason: String },
}

/// Trusted host integration, never a model tool or an assertion from log text.
/// Check the same incident episode against current authoritative facts and
/// freshness. Acknowledgements may advance its revision without ending a fault.
/// Checks must be synchronous and bounded; they must not perform target actions.
/// Hold the source's state-change gate through `commit`, which persists workflow
/// intent and consumes authority. Do not reenter that source from the callback.
pub trait IncidentGuard: Send + Sync {
    /// Call `commit` exactly once with current facts, or fail without calling it.
    fn with_current(
        &self,
        problem: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError>;

    /// Read a snapshot; final authorization uses `with_current` instead.
    fn check(&self, problem: &ProblemContext) -> Result<IncidentReadiness, RecoveryError> {
        let mut readiness = None;
        self.with_current(problem, &mut |current| {
            if readiness.is_some() {
                return Err(service("incident guard repeated its callback"));
            }
            readiness = Some(current);
            Ok(())
        })?;
        readiness.ok_or_else(|| service("incident guard did not check current facts"))
    }
}
