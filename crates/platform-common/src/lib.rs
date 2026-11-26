//! Shared types for the radio platform: identifiers, telemetry, commands,
//! scanner network policy and the agent ↔ gateway wire protocol.

#![warn(missing_docs)]

pub mod command;
pub mod ids;
pub mod net;
pub mod telemetry;
pub mod wire;

pub use command::{CommandError, CommandOutcome, HoldKind, KeyAction, ScannerCommand};
pub use ids::{
    AgentId, GroupId, InvalidAgentId, LeaseId, RequestId, ScannerId, SiteId, TenantId,
    TransmissionId,
};
pub use net::{NetworkPolicy, PolicyViolation};
pub use telemetry::{
    DisplayText, ScannerEvent, ScannerHealth, TelemetrySnapshot, TransmissionStart,
};

/// Current UTC time.
#[must_use]
pub fn now_utc() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}
