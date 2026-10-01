//! Bounded recovery knowledge. Stored scripts are proposals, never execution permits.
//!
//! The embedding application owns authenticated write access and independent business
//! verification. Models may propose candidates; they must not receive the attestation
//! or outcome-writing APIs as tools. Reading a verified case does not authorize reuse.
mod contract;
mod maintenance;
mod paths;
mod source;
mod storage;
mod validation;

pub use contract::*;
pub use storage::KnowledgeStore;
