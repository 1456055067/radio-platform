//! Scanner management: the agent-side [`ScannerRegistry`], execution of
//! platform commands through the SDK, and exclusive operator control
//! lease types used by the gateway.
//!
//! Per-device command queues are provided by `uniden-sds`. Each scanner has
//! one bounded queue and one command in flight.

#![warn(missing_docs)]

mod lease;
mod mapping;
mod registry;

pub use lease::{Lease, LeaseError, LeasePolicy};
pub use mapping::{execute, map_error};
pub use registry::{ApplyResult, RegistryConfig, ScannerRegistry};
