//! Business computations over caller-supplied data; no permissions or mutable state.
mod experience;
pub mod knowledge;
pub mod planning;
pub use experience::{ExperienceInput, build_experience};

#[derive(Debug, thiserror::Error)]
pub enum BusinessError {
    #[error("invalid business data: {0}")]
    Invalid(String),
    #[error("business data capacity exceeded")]
    Capacity,
}

pub mod approval;
pub mod engine;
pub mod workflow;
