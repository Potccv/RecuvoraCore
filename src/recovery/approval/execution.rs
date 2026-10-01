//! One-use durable execution intent, completion and explicit reconciliation.
use super::*;

impl ApprovalStore {
    /// Atomically persist one execution intent before returning its capability.
    /// The caller must additionally revalidate the current target/file state.
    pub fn consume(
        &mut self,
        request_id: &str,
        operation: &ProposedOperation,
        current_policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ExecutionPermit, ApprovalError> {
        self.check_current(request_id, current_policy, now)?;
        let record = self
            .records
            .get(request_id)
            .ok_or(ApprovalError::NotFound)?;
        if &record.request.operation != operation {
            return Err(ApprovalError::Conflict);
        }
        let record = self.change(request_id, Change::Consume, now)?;
        Ok(ExecutionPermit {
            request_id: request_id.into(),
            revision: record.revision,
            operation: record.request.operation,
            store_identity: self.identity.clone(),
        })
    }

    pub fn complete(
        &mut self,
        permit: ExecutionPermit,
        outcome: ExecutionOutcome,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.available()?;
        let record = self
            .records
            .get(&permit.request_id)
            .ok_or(ApprovalError::NotFound)?;
        if !Arc::ptr_eq(&self.identity, &permit.store_identity)
            || record.revision != permit.revision
            || record.request.operation != permit.operation
        {
            return Err(ApprovalError::Conflict);
        }
        self.change(
            &permit.request_id,
            Change::Complete { outcome, reason },
            now,
        )
    }

    /// Call only after independently verifying the target and stopping the old
    /// executor. This records a terminal fact; it never renews an execution permit.
    pub fn reconcile_unknown(
        &mut self,
        request_id: &str,
        outcome: ExecutionOutcome,
        reason: String,
        actor: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.change(
            request_id,
            Change::Reconcile {
                outcome,
                reason,
                actor,
            },
            now,
        )
    }
}
