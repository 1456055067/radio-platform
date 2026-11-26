//! Scanner telemetry and event types shared by agents, the gateway and storage.
//!
//! Every metadata field is optional. A value the scanner did not report is
//! `None`, never fabricated.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::ids::{ScannerId, SiteId, TransmissionId};

/// Display areas from the scanner's `ViewDescription`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct DisplayText {
    /// `InfoArea1`.
    pub info_area1: Option<String>,
    /// `InfoArea2`.
    pub info_area2: Option<String>,
    /// `OverWrite` message.
    pub overwrite: Option<String>,
    /// Popup text.
    pub popup: Option<String>,
}

/// Point-in-time scanner state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TelemetrySnapshot {
    /// Time the agent observed this state (UTC).
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub observed_at: OffsetDateTime,
    /// Scanner mode (`ScannerInfo@Mode`).
    pub mode: Option<String>,
    /// Screen type (`ScannerInfo@V_Screen`).
    pub screen: Option<String>,
    /// System name.
    pub system: Option<String>,
    /// System index (for hold commands).
    pub system_index: Option<u32>,
    /// System type.
    pub system_type: Option<String>,
    /// Department name.
    pub department: Option<String>,
    /// Department index.
    pub department_index: Option<u32>,
    /// Site name.
    pub site: Option<String>,
    /// Channel name.
    pub channel: Option<String>,
    /// Channel index.
    pub channel_index: Option<u32>,
    /// Source element of the channel (`ConvFrequency`, `TGID`, ...).
    pub channel_kind: Option<String>,
    /// Active frequency in Hz.
    pub frequency_hz: Option<u64>,
    /// Talkgroup ID.
    pub tgid: Option<String>,
    /// Radio unit ID.
    pub unit_id: Option<String>,
    /// Modulation.
    pub modulation: Option<String>,
    /// Service type.
    pub service_type: Option<String>,
    /// Signal bars (0–4).
    pub signal_bars: Option<u8>,
    /// RSSI as reported by the scanner (scale undocumented).
    pub rssi: Option<f32>,
    /// Audio muted (squelch closed), if reported.
    pub muted: Option<bool>,
    /// Volume level.
    pub volume: Option<u8>,
    /// Squelch level.
    pub squelch: Option<u8>,
    /// Scanner reports a hold.
    pub holding: bool,
    /// Scanner reports recording.
    pub recording: Option<bool>,
    /// Display areas.
    pub display: DisplayText,
}

impl TelemetrySnapshot {
    /// An empty snapshot observed at `observed_at`.
    #[must_use]
    pub fn empty(observed_at: OffsetDateTime) -> Self {
        Self {
            observed_at,
            mode: None,
            screen: None,
            system: None,
            system_index: None,
            system_type: None,
            department: None,
            department_index: None,
            site: None,
            channel: None,
            channel_index: None,
            channel_kind: None,
            frequency_hz: None,
            tgid: None,
            unit_id: None,
            modulation: None,
            service_type: None,
            signal_bars: None,
            rssi: None,
            muted: None,
            volume: None,
            squelch: None,
            holding: false,
            recording: None,
            display: DisplayText::default(),
        }
    }
}

/// Link health as seen by the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScannerHealth {
    /// No exchange has completed yet.
    Unknown,
    /// Reachable.
    Healthy,
    /// Recent failures.
    Degraded {
        /// Consecutive failures.
        consecutive_failures: u32,
    },
    /// Unreachable.
    Unreachable {
        /// Consecutive failures.
        consecutive_failures: u32,
    },
}

/// Metadata captured when a transmission starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TransmissionStart {
    /// Transmission identifier.
    pub transmission_id: TransmissionId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Site, if known to the agent.
    pub site_id: Option<SiteId>,
    /// Start (UTC).
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: OffsetDateTime,
    /// Frequency in Hz.
    pub frequency_hz: Option<u64>,
    /// Talkgroup ID.
    pub tgid: Option<String>,
    /// Radio unit ID.
    pub unit_id: Option<String>,
    /// System name.
    pub system: Option<String>,
    /// Department name.
    pub department: Option<String>,
    /// Channel name.
    pub channel: Option<String>,
    /// Modulation.
    pub modulation: Option<String>,
    /// Signal bars at start.
    pub signal_bars: Option<u8>,
    /// RSSI at start.
    pub rssi: Option<f32>,
}

/// A discrete scanner event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScannerEvent {
    /// A transmission began (single receiver: at most one is active).
    TransmissionStarted(TransmissionStart),
    /// The active transmission ended.
    TransmissionEnded {
        /// Transmission identifier.
        transmission_id: TransmissionId,
        /// End (UTC).
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        ended_at: OffsetDateTime,
        /// Duration in milliseconds.
        duration_ms: u64,
    },
    /// Link health changed.
    HealthChanged {
        /// New health.
        health: ScannerHealth,
    },
}
