//! Trusted Recuvora recovery decisions and durable domain facts.
//!
//! Applications own authentication, configuration, scheduling, composition,
//! routing and interfaces. Nodes/plugins own collection, execution and business
//! verification. This library keeps approval, incidents, knowledge and the
//! recovery workflow behind stable trusted ports.

mod identity;
pub mod operation;
pub mod recovery;
