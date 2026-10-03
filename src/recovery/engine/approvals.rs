use crate::operation::{CommitReceipt, Prepared};
use crate::recovery::approval::*;
#[derive(Clone, Debug)]
pub(super) struct Approvals {
    pub ledger: ApprovalLedger,
    pub entries: Vec<ApprovalEntry>,
}
impl Approvals {
    pub fn new(limits: ApprovalLimits) -> Result<Self, ApprovalError> {
        Ok(Self {
            ledger: ApprovalLedger::new(limits)?,
            entries: Vec::new(),
        })
    }
    fn next_id(&self) -> String {
        format!("approval-{}", self.ledger.revision())
    }
    fn commit_prepared(
        &mut self,
        pending: Prepared<ApprovalLedger, ApprovalEffect>,
    ) -> Result<Vec<ApprovalEffect>, ApprovalError> {
        self.entries.push(
            pending
                .state()
                .latest_entry()
                .ok_or(ApprovalError::Conflict)?
                .clone(),
        );
        // Subtransitions stay private inside the outer atomic proposal. No capability
        // is exposed until the enclosing session commit has been confirmed.
        let receipt = CommitReceipt::confirmed(pending.request());
        let committed = pending.confirm(receipt)?;
        self.ledger = committed.state;
        Ok(committed.effects)
    }
    pub fn recover(&mut self, now: u64) -> Result<(), ApprovalError> {
        if self.ledger.recovery_required() {
            let pending = self.ledger.prepare_recovery(self.next_id(), now)?;
            self.commit_prepared(pending)?;
        }
        Ok(())
    }
    fn change(
        &mut self,
        id: &str,
        change: ApprovalChange,
        policy: Option<&ApprovalPolicy>,
        now: u64,
    ) -> Result<Vec<ApprovalEffect>, ApprovalError> {
        let pending = self.ledger.prepare(
            self.next_id(),
            ApprovalEvent::Changed {
                request_id: id.into(),
                change,
            },
            policy,
            now,
        )?;
        self.commit_prepared(pending)
    }
    fn changed_record(
        &mut self,
        id: &str,
        change: ApprovalChange,
        policy: Option<&ApprovalPolicy>,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.change(id, change, policy, now)?;
        self.get(id).cloned().ok_or(ApprovalError::NotFound)
    }
    pub fn get(&self, id: &str) -> Option<&ApprovalRecord> {
        self.ledger.get(id)
    }
    pub fn list(&self) -> Vec<ApprovalRecord> {
        self.ledger.list()
    }
    pub fn request(
        &mut self,
        operation: ProposedOperation,
        policy: ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        if let Some(old) = self
            .ledger
            .find_operation(&operation.task_id, &operation.operation_id)
        {
            return if old.request.operation == operation && old.request.policy == policy {
                Ok(old.clone())
            } else {
                Err(ApprovalError::Conflict)
            };
        }
        let pending =
            self.ledger
                .prepare_request(self.next_id(), operation.clone(), policy, now)?;
        self.commit_prepared(pending)?;
        self.ledger
            .find_operation(&operation.task_id, &operation.operation_id)
            .cloned()
            .ok_or(ApprovalError::NotFound)
    }

    pub fn decide_human_at_revision(
        &mut self,
        id: &str,
        expected_revision: u64,
        assessment: ApprovalAssessment,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(
            id,
            ApprovalChange::HumanDecision {
                expected_revision,
                assessment,
            },
            Some(policy),
            now,
        )
    }
    pub fn begin_harness_review(
        &mut self,
        id: &str,
        expected_revision: u64,
        policy: &ApprovalPolicy,
        timeout_secs: u64,
        now: u64,
    ) -> Result<ReviewAttempt, ApprovalError> {
        self.change(
            id,
            ApprovalChange::BeginReview {
                expected_revision,
                timeout_secs,
            },
            Some(policy),
            now,
        )?
        .into_iter()
        .find_map(|e| {
            if let ApprovalEffect::Review(attempt) = e {
                Some(attempt)
            } else {
                None
            }
        })
        .ok_or(ApprovalError::Conflict)
    }
    pub fn assess_attempt(
        &mut self,
        attempt: &ReviewAttempt,
        assessment: ModelAssessment,
        reviewer: ReviewerIdentity,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        if assessment.request_id != attempt.request_id {
            return Err(ApprovalError::Conflict);
        }
        self.changed_record(
            &attempt.request_id,
            ApprovalChange::AssessAttempt {
                attempt: attempt.clone(),
                assessment: ApprovalAssessment {
                    decision: assessment.decision,
                    reason: assessment.reason,
                    reviewer: AssessmentSource::Harness {
                        harness_id: reviewer.harness_id,
                        session_id: reviewer.session_id,
                    },
                },
            },
            Some(policy),
            now,
        )
    }
    pub fn fail_review_attempt(
        &mut self,
        attempt: &ReviewAttempt,
        reason: String,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(
            &attempt.request_id,
            ApprovalChange::FailReview {
                attempt: attempt.clone(),
                reason,
            },
            Some(policy),
            now,
        )
    }
    pub fn expire_review(
        &mut self,
        id: &str,
        expected_revision: u64,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let record = self.get(id).ok_or(ApprovalError::NotFound)?;
        if record.revision != expected_revision {
            return Err(ApprovalError::Conflict);
        }
        let attempt = record
            .active_review_attempt()
            .ok_or(ApprovalError::Conflict)?;
        if now < attempt.deadline {
            return Err(ApprovalError::ReviewNotDue);
        }
        self.fail_review_attempt(
            &attempt,
            "harness review deadline elapsed".into(),
            policy,
            now,
        )
    }

    pub fn cancel(
        &mut self,
        id: &str,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(id, ApprovalChange::Cancel { reason }, None, now)
    }

    pub fn consume(
        &mut self,
        id: &str,
        operation: &ProposedOperation,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ExecutionPermit, ApprovalError> {
        if self
            .get(id)
            .ok_or(ApprovalError::NotFound)?
            .request
            .operation
            != *operation
        {
            return Err(ApprovalError::Conflict);
        }
        self.change(id, ApprovalChange::Consume, Some(policy), now)?
            .into_iter()
            .find_map(|e| {
                if let ApprovalEffect::Execute(permit) = e {
                    Some(permit)
                } else {
                    None
                }
            })
            .ok_or(ApprovalError::Conflict)
    }
    pub fn complete(
        &mut self,
        permit: ExecutionPermit,
        outcome: ExecutionOutcome,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let id = permit.request_id().to_owned();
        let pending = self
            .ledger
            .prepare_complete(self.next_id(), permit, outcome, reason, now)?;
        self.commit_prepared(pending)?;
        self.get(&id).cloned().ok_or(ApprovalError::NotFound)
    }

    pub fn expire(&mut self, id: &str, now: u64) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(id, ApprovalChange::Expire, None, now)
    }
    pub fn reconcile_unknown(
        &mut self,
        id: &str,
        outcome: ExecutionOutcome,
        reason: String,
        actor: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(
            id,
            ApprovalChange::Reconcile {
                outcome,
                reason,
                actor,
            },
            None,
            now,
        )
    }
}
