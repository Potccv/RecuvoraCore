//! Pure recovery decisions, state transitions and Host commit proposals.
//!
//! The trusted Host owns durable storage, atomic version checks, authentication,
//! scheduling, target ownership, external calls and process supervision. Core
//! validates their domain inputs and releases effects after commit confirmation.
mod identity;
pub mod operation;
pub mod recovery;
