use super::*;
use crate::recovery::{approval::*, knowledge::*, workflow::*};
use std::{future::Future, pin::Pin};
pub type CapabilityFuture<'a, T> = Pin<Box<dyn Future<Output = EngineResult<T>> + Send + 'a>>;

pub struct ExecutionContext {
    pub incident: IncidentEvidence,
    pub authority: TargetAuthority,
}
/// Trusted runtime adapter. Implementations supply facts and perform capabilities;
/// the engine owns sequencing and business decisions. Commit MUST atomically save
/// and confirm the complete SessionCommand before returning any effects.
pub trait RecoveryPlatform: Send + Sync {
    fn config(&self) -> &RecoveryConfig;
    fn now_ms(&self) -> u64;
    fn cancelled(&self) -> bool;
    fn task(&self, id: &str) -> EngineResult<RecoveryTask>;
    fn approval(&self, id: &str) -> EngineResult<Option<ApprovalRecord>>;
    fn pending_experiences(&self) -> EngineResult<Vec<ExperienceJob>>;
    fn commit(&self, command: SessionCommand) -> EngineResult<Vec<SessionEffect>>;
    fn inspect(&self, timeout_secs: u64) -> CapabilityFuture<'_, TargetObservation>;
    fn review(&self, input: ReviewInput, timeout_secs: u64) -> CapabilityFuture<'_, ReviewOutput>;
    /// Retain intake-identity and target-ownership protection until release_execution.
    fn acquire_execution<'a>(
        &'a self,
        task: &'a RecoveryTask,
    ) -> CapabilityFuture<'a, ExecutionContext>;
    /// No retries. Validate the retained gate at the actual send, then drain the call.
    fn execute<'a>(
        &'a self,
        permit: &'a ExecutionPermit,
        timeout_secs: u64,
    ) -> CapabilityFuture<'a, RepairReceipt>;
    fn release_execution(&self);
    fn verify(
        &self,
        input: VerificationInput,
        timeout_secs: u64,
    ) -> CapabilityFuture<'_, BusinessVerification>;
    fn summarize(
        &self,
        job: ExperienceJob,
        timeout_secs: u64,
    ) -> CapabilityFuture<'_, ExperienceReport>;
}

/// Drives the entire repair lifecycle through injected capabilities. No runtime,
/// network client, concrete storage, system clock or provider implementation.
pub struct RecoveryEngine;
impl RecoveryEngine {
    pub async fn advance(platform: &impl RecoveryPlatform, id: &str) -> EngineResult<RecoveryTask> {
        for _ in 0..64 {
            let task = platform.task(id)?;
            if task.stage.terminal()
                || matches!(task.stage, RecoveryStage::Paused | RecoveryStage::Unknown)
            {
                // Summary failure remains in durable jobs and never changes business completion.
                let _ = Self::summarize_pending(platform, false).await;
                return Ok(task);
            }
            if platform.cancelled()
                && !matches!(
                    task.stage,
                    RecoveryStage::Executing | RecoveryStage::Verifying
                )
            {
                platform.commit(SessionCommand::Cancel {
                    task_id: task.id,
                    revision: task.revision,
                })?;
                return platform.task(id);
            }
            match task.stage {
                RecoveryStage::Queued => {
                    let observation = platform
                        .inspect(platform.config().target.action_timeout_secs)
                        .await?;
                    platform.commit(SessionCommand::Start {
                        task_id: task.id,
                        revision: task.revision,
                        observation,
                    })?;
                }
                RecoveryStage::AwaitingApproval => {
                    let record = platform.approval(id)?.ok_or(EngineError::Conflict)?;
                    let now = platform.now_ms() / 1000;
                    let expired = now >= record.request.expires_at;
                    if record.state == ApprovalState::Approved && !expired {
                        Self::execute(platform, &task).await?;
                    } else {
                        let timeout_due = record.review_stage == ReviewStage::ReviewingHarness
                            && record.review_deadline.is_some_and(|d| now >= d);
                        let waiting =
                            matches!(record.request.policy.reviewer, ReviewerConfig::Human)
                                || record.review_stage == ReviewStage::NeedsHuman
                                || record.review_stage == ReviewStage::ReviewingHarness
                                || (record.review_stage == ReviewStage::WaitingHuman
                                    && record.human_deadline.is_some_and(|d| now < d));
                        if waiting && !expired && !timeout_due {
                            return Ok(task);
                        }
                        let observation = if expired || timeout_due {
                            None
                        } else {
                            Some(
                                platform
                                    .inspect(platform.config().review_timeout_secs)
                                    .await?,
                            )
                        };
                        let effects = platform.commit(SessionCommand::Review {
                            task_id: task.id.clone(),
                            revision: task.revision,
                            observation,
                        })?;
                        if let Some(SessionEffect::Review(input)) = effects.into_iter().next() {
                            let attempt = input.attempt.clone();
                            let timeout = attempt
                                .deadline
                                .saturating_sub(platform.now_ms() / 1000)
                                .max(1);
                            let result = platform
                                .review(*input, timeout)
                                .await
                                .map_err(|e| bounded_reason(&e));
                            platform.commit(SessionCommand::Reviewed {
                                task_id: task.id,
                                attempt,
                                result,
                            })?;
                        } else if platform.task(id)?.stage == RecoveryStage::AwaitingApproval {
                            return platform.task(id);
                        }
                    }
                }
                RecoveryStage::Verifying => {
                    let input = VerificationInput {
                        target: platform.config().target.clone(),
                        operation: task.operation.clone().ok_or(EngineError::Conflict)?,
                        receipt: task.receipt.clone().ok_or(EngineError::Conflict)?,
                    };
                    let result = platform
                        .verify(input, platform.config().target.action_timeout_secs)
                        .await
                        .map_err(|e| bounded_reason(&e));
                    platform.commit(SessionCommand::Verified {
                        task_id: task.id,
                        revision: task.revision,
                        result,
                    })?;
                }
                _ => return Err(EngineError::Busy),
            }
        }
        Err(EngineError::Capacity)
    }
    async fn execute(platform: &impl RecoveryPlatform, task: &RecoveryTask) -> EngineResult<()> {
        let observation = platform
            .inspect(platform.config().target.action_timeout_secs)
            .await?;
        if platform.cancelled() {
            platform.commit(SessionCommand::Cancel {
                task_id: task.id.clone(),
                revision: task.revision,
            })?;
            return Ok(());
        }
        let context = platform.acquire_execution(task).await?;
        // Release physical protection on every return or future drop. Actual executor
        // supervision remains the runtime's responsibility until its original call drains.
        let _guard = ExecutionRelease(platform);
        if platform.cancelled() {
            platform.commit(SessionCommand::Cancel {
                task_id: task.id.clone(),
                revision: task.revision,
            })?;
            return Ok(());
        }
        let effects = platform.commit(SessionCommand::Authorize {
            task_id: task.id.clone(),
            revision: task.revision,
            observation,
            incident: context.incident,
            authority: context.authority,
        })?;
        let Some(SessionEffect::Execute {
            permit,
            timeout_secs,
        }) = effects.into_iter().next()
        else {
            return Err(EngineError::Conflict);
        };
        let result = platform
            .execute(&permit, timeout_secs)
            .await
            .map_err(|e| bounded_reason(&e));
        platform.release_execution();
        platform.commit(SessionCommand::Executed {
            task_id: task.id.clone(),
            permit,
            result,
        })?;
        Ok(())
    }
    pub async fn summarize_pending(
        platform: &impl RecoveryPlatform,
        explicit: bool,
    ) -> EngineResult<()> {
        for job in platform
            .pending_experiences()?
            .into_iter()
            .filter(|j| j.report.is_none() && j.call_id.is_none() && (explicit || j.attempt < 3))
            .take(4)
        {
            if platform.cancelled() {
                break;
            }
            let effects = platform.commit(SessionCommand::BeginSummary { job_id: job.id })?;
            let Some(SessionEffect::Summarize { job, call_id }) = effects.into_iter().next() else {
                return Err(EngineError::Conflict);
            };
            let job_id = job.id.clone();
            let result = platform
                .summarize(*job, platform.config().summary_timeout_secs)
                .await
                .map_err(|e| bounded_reason(&e));
            platform.commit(SessionCommand::Summarized {
                job_id,
                call_id,
                result,
            })?;
        }
        if platform
            .pending_experiences()?
            .iter()
            .any(|j| j.report.is_some())
        {
            platform.commit(SessionCommand::Deliver)?;
        }
        Ok(())
    }
}
struct ExecutionRelease<'a, P: RecoveryPlatform>(&'a P);
impl<P: RecoveryPlatform> Drop for ExecutionRelease<'_, P> {
    fn drop(&mut self) {
        self.0.release_execution();
    }
}
pub(super) fn bounded_reason(error: &impl std::fmt::Display) -> String {
    let text = error.to_string();
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
