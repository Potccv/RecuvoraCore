//! Pure recovery decisions, state transitions and commit proposals.
//!
//! Facts, time and evidence are explicit trusted inputs. Core performs no I/O
//! or scheduling; it releases new state and effects only after a matching commit
//! confirmation. Public modules define operation and recovery contracts.
mod binding;
mod collections;
mod identity;
pub mod operation;
pub mod recovery;
