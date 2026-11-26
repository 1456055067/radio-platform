//! Platform-level scanner commands (transport-neutral, serializable).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What a hold targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    /// System.
    System,
    /// Department.
    Department,
    /// Site.
    Site,
    /// Conventional channel.
    Channel,
    /// Trunked (TGID) channel.
    TalkgroupChannel,
}

/// Key actuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    /// Momentary press.
    Press,
    /// Long press.
    LongPress,
}

/// A command for a scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ScannerCommand {
    /// Read full scanner information (GSI).
    GetStatus,
    /// Probe capabilities (read-only probes).
    ProbeCapabilities,
    /// Read volume.
    GetVolume,
    /// Set volume.
    SetVolume {
        /// Level.
        level: u8,
    },
    /// Read squelch.
    GetSquelch,
    /// Set squelch.
    SetSquelch {
        /// Level.
        level: u8,
    },
    /// Hold on a system, department, site or channel.
    Hold {
        /// Target kind.
        kind: HoldKind,
        /// Scanner index from telemetry or lists.
        index: u32,
    },
    /// Resume scanning.
    Resume,
    /// Emulate a front-panel key.
    Key {
        /// Single-character key code.
        code: char,
        /// Actuation.
        action: KeyAction,
    },
    /// Quick search hold on a frequency.
    QuickSearchHold {
        /// Frequency in Hz.
        frequency_hz: u64,
    },
    /// Administrator raw command.
    Raw {
        /// Command name (2–8 uppercase letters).
        name: String,
        /// Arguments.
        args: Vec<String>,
        /// Declared read-only (enables retries).
        read_only: bool,
    },
}

impl ScannerCommand {
    /// True if executing the command requires an exclusive control lease.
    #[must_use]
    pub fn requires_control(&self) -> bool {
        match self {
            Self::GetStatus | Self::ProbeCapabilities | Self::GetVolume | Self::GetSquelch => false,
            Self::Raw { read_only, .. } => !read_only,
            _ => true,
        }
    }

    /// True if the command is restricted to administrators.
    #[must_use]
    pub fn requires_admin(&self) -> bool {
        matches!(self, Self::Raw { .. })
    }

    /// Short name for logs and audit records.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::GetStatus => "get_status",
            Self::ProbeCapabilities => "probe_capabilities",
            Self::GetVolume => "get_volume",
            Self::SetVolume { .. } => "set_volume",
            Self::GetSquelch => "get_squelch",
            Self::SetSquelch { .. } => "set_squelch",
            Self::Hold { .. } => "hold",
            Self::Resume => "resume",
            Self::Key { .. } => "key",
            Self::QuickSearchHold { .. } => "quick_search_hold",
            Self::Raw { .. } => "raw",
        }
    }
}

/// Classification of a command failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandError {
    /// Arguments failed validation.
    Invalid {
        /// Detail.
        message: String,
    },
    /// The scanner is not managed by this agent.
    ScannerNotFound,
    /// The scanner rejected the command as unsupported (`ERR`).
    Unsupported,
    /// The scanner rejected the command (`NG` or a command-specific code).
    Rejected {
        /// Detail.
        message: String,
    },
    /// No reply in time.
    Timeout {
        /// The command may have executed.
        state_change_unknown: bool,
    },
    /// The scanner or agent is unavailable.
    Unavailable {
        /// Detail.
        message: String,
    },
    /// Unexpected failure.
    Internal {
        /// Detail.
        message: String,
    },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { message } => write!(f, "invalid command: {message}"),
            Self::ScannerNotFound => f.write_str("scanner not found"),
            Self::Unsupported => f.write_str("command not supported by the scanner"),
            Self::Rejected { message } => write!(f, "scanner rejected the command: {message}"),
            Self::Timeout {
                state_change_unknown: true,
            } => f.write_str("timed out; scanner state may have changed"),
            Self::Timeout {
                state_change_unknown: false,
            } => f.write_str("timed out"),
            Self::Unavailable { message } => write!(f, "unavailable: {message}"),
            Self::Internal { message } => write!(f, "internal error: {message}"),
        }
    }
}

/// Result of a command: JSON data on success.
pub type CommandOutcome = Result<serde_json::Value, CommandError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_and_admin_requirements() {
        assert!(!ScannerCommand::GetStatus.requires_control());
        assert!(ScannerCommand::SetVolume { level: 1 }.requires_control());
        assert!(ScannerCommand::Resume.requires_control());
        let raw = ScannerCommand::Raw {
            name: "GLG".into(),
            args: vec![],
            read_only: true,
        };
        assert!(!raw.requires_control());
        assert!(raw.requires_admin());
    }

    #[test]
    fn serde_shape() {
        let c: ScannerCommand =
            serde_json::from_str(r#"{"command":"set_volume","level":7}"#).unwrap();
        assert_eq!(c, ScannerCommand::SetVolume { level: 7 });
        assert!(serde_json::from_str::<ScannerCommand>(r#"{"command":"format_disk"}"#).is_err());
    }
}
