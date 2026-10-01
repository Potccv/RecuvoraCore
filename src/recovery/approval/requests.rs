//! Request identity, reviewer evidence and trusted human decisions.
use super::*;

impl ApprovalStore {
    /// Persist the review handoff before dispatching an external Harness call.
    /// The caller schedules deadlines; this store alone validates authority.
    pub fn begin_harness_review(
        &mut self,
        request_id: &str,
        expected_revision: u64,
        current_policy: &ApprovalPolicy,
        timeout_secs: u64,
        now: u64,
    ) -> Result<ReviewAttempt, ApprovalError> {
        self.check_current(request_id, current_policy, now)?;
        let record = self.change(
            request_id,
            Change::BeginReview {
                expected_revision,
                timeout_secs,
            },
            now,
        )?;
        record
            .active_review_attempt()
            .ok_or(ApprovalError::Conflict)
    }

    pub fn assess_attempt(
        &mut self,
        attempt: &ReviewAttempt,
        assessment: ModelAssessment,
        reviewer: ReviewerIdentity,
        current_policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.check_current(&attempt.request_id, current_policy, now)?;
        let record = self
            .records
            .get(&attempt.request_id)
            .ok_or(ApprovalError::NotFound)?;
        super::transitions::validate_attempt(record, attempt)?;
        if now >= attempt.deadline {
            self.fail_review_attempt(
                attempt,
                "harness review deadline elapsed".into(),
                current_policy,
                now,
            )?;
            return Err(ApprovalError::ReviewTimedOut);
        }
        if assessment.request_id != attempt.request_id {
            return Err(ApprovalError::Conflict);
        }
        self.change(
            &attempt.request_id,
            Change::AssessAttempt {
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
            now,
        )
    }

    /// An unavailable or uncertain reviewer needs human intervention; this
    /// stage has no automatic fallback and cannot repeat the review loop.
    pub fn fail_review_attempt(
        &mut self,
        attempt: &ReviewAttempt,
        reason: String,
        current_policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.check_current(&attempt.request_id, current_policy, now)?;
        self.change(
            &attempt.request_id,
            Change::FailReview {
                attempt: attempt.clone(),
                reason,
            },
            now,
        )
    }

    pub fn expire_review(
        &mut self,
        request_id: &str,
        expected_revision: u64,
        current_policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.check_current(request_id, current_policy, now)?;
        let record = self
            .records
            .get(request_id)
            .ok_or(ApprovalError::NotFound)?;
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
            current_policy,
            now,
        )
    }

    /// Trusted human decisions can take over a running review. Revision binding
    /// prevents a decision from an obsolete screen or concurrent timer winning.
    pub fn decide_human_at_revision(
        &mut self,
        request_id: &str,
        expected_revision: u64,
        assessment: ApprovalAssessment,
        current_policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.check_current(request_id, current_policy, now)?;
        if self
            .records
            .get(request_id)
            .ok_or(ApprovalError::NotFound)?
            .revision
            != expected_revision
        {
            return Err(ApprovalError::Conflict);
        }
        self.change(request_id, Change::HumanDecision { assessment }, now)
    }

    pub fn get(&self, request_id: &str) -> Option<&ApprovalRecord> {
        self.records.get(request_id)
    }
    pub fn list(&self) -> Vec<ApprovalRecord> {
        self.records.values().cloned().collect()
    }

    pub fn request(
        &mut self,
        operation: ProposedOperation,
        policy: ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.available()?;
        operation.validate()?;
        policy.validate()?;
        if let Some(previous) = self.records.values().find(|record| {
            record.request.operation.task_id == operation.task_id
                && record.request.operation.operation_id == operation.operation_id
        }) {
            return if previous.request.operation == operation && previous.request.policy == policy {
                Ok(previous.clone())
            } else {
                Err(ApprovalError::Conflict)
            };
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ApprovalError::Capacity)?;
        let request = ApprovalRequest {
            request_id: format!("approval-{sequence:016x}"),
            operation,
            policy,
            created_at: now,
            expires_at: 0,
        };
        let request = ApprovalRequest {
            expires_at: now
                .checked_add(request.policy.ttl_secs)
                .ok_or(ApprovalError::Invalid("expiry overflow"))?,
            ..request
        };
        self.append(Event::Requested { request }, now)
    }

    pub fn assess(
        &mut self,
        request_id: &str,
        assessment: ModelAssessment,
        reviewer: ReviewerIdentity,
        current_policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.check_current(request_id, current_policy, now)?;
        if assessment.request_id != request_id {
            return Err(ApprovalError::Conflict);
        }
        self.change(
            request_id,
            Change::Assess {
                assessment: ApprovalAssessment {
                    decision: assessment.decision,
                    reason: assessment.reason,
                    reviewer: AssessmentSource::Harness {
                        harness_id: reviewer.harness_id,
                        session_id: reviewer.session_id,
                    },
                },
            },
            now,
        )
    }

    /// Only a trusted interactive host/CLI handler may call this. `actor` is audit
    /// attribution, not authentication. Never derive it from a model response.
    pub fn decide_human(
        &mut self,
        request_id: &str,
        decision: ApprovalDecision,
        reason: String,
        actor: String,
        current_policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.check_current(request_id, current_policy, now)?;
        self.change(
            request_id,
            Change::HumanDecision {
                assessment: ApprovalAssessment {
                    decision,
                    reason,
                    reviewer: AssessmentSource::Human { actor },
                },
            },
            now,
        )
    }
}

impl ApprovalStore {
    pub fn revoke(
        &mut self,
        request_id: &str,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.change(request_id, Change::Revoke { reason }, now)
    }

    pub fn cancel(
        &mut self,
        request_id: &str,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.change(request_id, Change::Cancel { reason }, now)
    }

    /// Record a host-observed reviewer failure without inventing model output.
    pub fn mark_waiting_human(
        &mut self,
        request_id: &str,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.available()?;
        let policy = self
            .records
            .get(request_id)
            .ok_or(ApprovalError::NotFound)?
            .request
            .policy
            .clone();
        self.check_current(request_id, &policy, now)?;
        self.change(request_id, Change::WaitingHuman { reason }, now)
    }
}
