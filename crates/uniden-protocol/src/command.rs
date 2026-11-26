//! Typed commands and their wire encoding.
//!
//! Every command carries [`Provenance`] metadata so that callers and
//! documentation can distinguish commands taken from official Uniden
//! documentation from commands that are only community-reported.

use std::fmt::{self, Write as _};

use crate::error::EncodeError;
use crate::types::{Frequency, KeyCode, KeyMode, Squelch, Volume};

/// Maximum encoded command length, including the trailing carriage return.
///
/// No limit is published; this bound protects scanners and the SDK from
/// accidentally oversized raw commands.
pub const MAX_COMMAND_LEN: usize = 512;

/// Where knowledge of a command comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Documented in Uniden's *BCD436HP/BCD536HP Remote Command Specification*
    /// v1.03 (2015-10-29), the published predecessor of the SDS-series
    /// protocol. Expected but **not verified** to behave identically on the
    /// SDS200 or over its network interface.
    PredecessorSpec,
    /// Not present in the located official documents; reported by community
    /// sources. Must be capability-probed and hardware-verified before use.
    CommunityReported,
    /// Operator-supplied raw command; behaviour unknown.
    Raw,
}

/// Whether a command may change scanner state.
///
/// The SDK never automatically retries [`Effect::StateChanging`] commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effect {
    /// Only reads state; safe to retry.
    ReadOnly,
    /// May change scanner state; never retried automatically.
    StateChanging,
}

/// How the scanner frames a command's reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplyFormat {
    /// A single `CMD,field,...\r` line.
    Line,
    /// `CMD,<XML>,\r` followed by an XML document (may span datagrams).
    Xml,
    /// Either form (used for raw commands and `PSI`).
    Any,
    /// The scanner sends no reply (`KAL`).
    None,
}

/// The "target keyword" (`tkw`) used by `HLD`, `AVD`, `NXT` and `PRV`
/// (sheet "tkd and 1st,2nd opt" in the BCDx36HP specification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum TargetKeyword {
    FavoritesList,
    System,
    Department,
    Site,
    ConventionalFrequency,
    Tgid,
    SearchTgid,
    SiteFrequency,
    AvoidingTgid,
    AvoidingFrequency,
    CloseCall,
    Weather,
    ToneOut,
    SearchWithScanFrequency,
    CloseCallHit,
    CustomSearchBank,
    CustomSearchFrequency,
    QuickSearchFrequency,
    RepeaterFindFrequency,
    InnerRecordFile,
    UserRecordFolder,
    UserRecordFile,
    TrunkDiscovery,
    ConventionalDiscovery,
    BandScope,
}

impl TargetKeyword {
    /// Wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FavoritesList => "FL",
            Self::System => "SYS",
            Self::Department => "DEPT",
            Self::Site => "SITE",
            Self::ConventionalFrequency => "CFREQ",
            Self::Tgid => "TGID",
            Self::SearchTgid => "STGID",
            Self::SiteFrequency => "SFREQ",
            Self::AvoidingTgid => "ATGID",
            Self::AvoidingFrequency => "AFREQ",
            Self::CloseCall => "CC",
            Self::Weather => "WX",
            Self::ToneOut => "FTO",
            Self::SearchWithScanFrequency => "SWS_FREQ",
            Self::CloseCallHit => "CCHIT",
            Self::CustomSearchBank => "CS_BANK",
            Self::CustomSearchFrequency => "CS_FREQ",
            Self::QuickSearchFrequency => "QS_FREQ",
            Self::RepeaterFindFrequency => "RPTR_FREQ",
            Self::InnerRecordFile => "IREC_FILE",
            Self::UserRecordFolder => "UREC_FOLDER",
            Self::UserRecordFile => "UREC_FILE",
            Self::TrunkDiscovery => "TRN_DISCOV",
            Self::ConventionalDiscovery => "CNV_DISCOV",
            Self::BandScope => "BAND_SCOPE",
        }
    }
}

/// A validated positional argument: printable ASCII without `,`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Arg(String);

impl Arg {
    /// Validates an argument string.
    ///
    /// # Errors
    /// Rejects control characters, non-ASCII bytes and the `,` separator.
    pub fn new(value: impl Into<String>) -> Result<Self, EncodeError> {
        let value = value.into();
        if let Some(bad) = value
            .chars()
            .find(|c| !(c.is_ascii_graphic() || *c == ' ') || *c == ',')
        {
            return Err(EncodeError::InvalidArgument {
                name: "argument",
                reason: format!("contains forbidden character {bad:?}"),
            });
        }
        Ok(Self(value))
    }

    /// An empty positional argument (`[none]` in the specification).
    #[must_use]
    pub fn none() -> Self {
        Self(String::new())
    }

    fn number(n: impl fmt::Display) -> Self {
        Self(n.to_string())
    }

    /// The argument text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Target of a `HLD` (hold) or `AVD` (avoid) command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    /// Target keyword.
    pub keyword: TargetKeyword,
    /// First option (`xxx1`).
    pub first: Arg,
    /// Second option (`xxx2`).
    pub second: Arg,
}

impl Target {
    /// Hold/avoid a system by its GSI/GLT `Index`.
    #[must_use]
    pub fn system(index: u32) -> Self {
        Self {
            keyword: TargetKeyword::System,
            first: Arg::number(index),
            second: Arg::none(),
        }
    }

    /// Hold/avoid a department by its `Index`. The parent system index is
    /// only required for the virtual "Unknown" department in ID Search.
    #[must_use]
    pub fn department(index: u32, parent_system: Option<u32>) -> Self {
        Self {
            keyword: TargetKeyword::Department,
            first: Arg::number(index),
            second: parent_system.map_or_else(Arg::none, Arg::number),
        }
    }

    /// Hold/avoid a site by its `Index`.
    #[must_use]
    pub fn site(index: u32) -> Self {
        Self {
            keyword: TargetKeyword::Site,
            first: Arg::number(index),
            second: Arg::none(),
        }
    }

    /// Hold/avoid a conventional channel by its `Index`.
    #[must_use]
    pub fn conventional_channel(index: u32) -> Self {
        Self {
            keyword: TargetKeyword::ConventionalFrequency,
            first: Arg::number(index),
            second: Arg::none(),
        }
    }

    /// Hold/avoid a trunked (TGID) channel by its `Index` in ID Scan.
    #[must_use]
    pub fn tgid_channel(index: u32) -> Self {
        Self {
            keyword: TargetKeyword::Tgid,
            first: Arg::number(index),
            second: Arg::none(),
        }
    }

    /// Custom target built from validated arguments.
    #[must_use]
    pub fn custom(keyword: TargetKeyword, first: Arg, second: Arg) -> Self {
        Self {
            keyword,
            first,
            second,
        }
    }
}

/// `AVD` status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvoidStatus {
    /// `1` — permanent avoid.
    Permanent,
    /// `2` — temporary avoid.
    Temporary,
    /// `3` — stop avoiding.
    Stop,
}

/// List selector for `GLT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum ListRequest {
    FavoritesLists,
    Systems { favorites_list: u32 },
    Departments { system: u32 },
    Sites { system: u32 },
    ConventionalFrequencies { department: u32 },
    Tgids { department: u32 },
    SiteFrequencies { site: u32 },
    AvoidedFrequencies,
    AvoidedTgids { system: u32 },
    ToneOut,
    CustomSearchBanks,
    UserRecordFolders,
    InnerRecordFiles,
    UserRecordFiles { folder: u32 },
    TrunkDiscovery,
    ConventionalDiscovery,
}

impl ListRequest {
    fn args(self) -> Vec<Arg> {
        let (kw, idx): (&str, Option<u32>) = match self {
            Self::FavoritesLists => ("FL", None),
            Self::Systems { favorites_list } => ("SYS", Some(favorites_list)),
            Self::Departments { system } => ("DEPT", Some(system)),
            Self::Sites { system } => ("SITE", Some(system)),
            Self::ConventionalFrequencies { department } => ("CFREQ", Some(department)),
            Self::Tgids { department } => ("TGID", Some(department)),
            Self::SiteFrequencies { site } => ("SFREQ", Some(site)),
            Self::AvoidedFrequencies => ("AFREQ", None),
            Self::AvoidedTgids { system } => ("ATGID", Some(system)),
            Self::ToneOut => ("FTO", None),
            Self::CustomSearchBanks => ("CS_BANK", None),
            Self::UserRecordFolders => ("UREC", None),
            Self::InnerRecordFiles => ("IREC_FILE", None),
            Self::UserRecordFiles { folder } => ("UREC_FILE", Some(folder)),
            Self::TrunkDiscovery => ("TRN_DISCOV", None),
            Self::ConventionalDiscovery => ("CNV_DISCOV", None),
        };
        let mut v = vec![Arg(kw.to_owned())];
        if let Some(i) = idx {
            v.push(Arg::number(i));
        }
        v
    }
}

/// Jump mode for `JPM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum JumpModeKind {
    Scan,
    CustomSearch,
    QuickSearch,
    CloseCall,
    Weather,
    ToneOut,
    InnerRecord,
    UserRecord,
    TrunkDiscovery,
    ConventionalDiscovery,
}

impl JumpModeKind {
    /// Wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "SCN_MODE",
            Self::CustomSearch => "CTM_MODE",
            Self::QuickSearch => "QSH_MODE",
            Self::CloseCall => "CC_MODE",
            Self::Weather => "WX_MODE",
            Self::ToneOut => "FTO_MODE",
            Self::InnerRecord => "IREC_MODE",
            Self::UserRecord => "UREC_MODE",
            Self::TrunkDiscovery => "TDIS_MODE",
            Self::ConventionalDiscovery => "CDIS_MODE",
        }
    }
}

/// Channel index meaning "start scanning from the top channel"
/// (`0xFFFFFFFF`, documented for `JPM,SCN_MODE`).
pub const SCAN_FROM_TOP: u32 = u32::MAX;

/// A validated raw command for administrator use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawCommand {
    name: String,
    args: Vec<Arg>,
    effect: Effect,
}

impl RawCommand {
    /// Builds a raw command. `name` must be 2–8 uppercase ASCII letters.
    /// Raw commands are treated as [`Effect::StateChanging`] unless the
    /// caller explicitly declares otherwise with [`RawCommand::read_only`].
    ///
    /// # Errors
    /// Returns an error if the name or arguments fail validation.
    pub fn new(name: &str, args: Vec<Arg>) -> Result<Self, EncodeError> {
        if !(2..=8).contains(&name.len()) || !name.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(EncodeError::InvalidArgument {
                name: "command",
                reason: format!("`{name}` must be 2-8 uppercase ASCII letters"),
            });
        }
        Ok(Self {
            name: name.to_owned(),
            args,
            effect: Effect::StateChanging,
        })
    }

    /// Declares the raw command as read-only, allowing retries.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.effect = Effect::ReadOnly;
        self
    }
}

/// A command that can be sent to a scanner.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Command {
    /// `MDL` — get model name.
    GetModel,
    /// `VER` — get firmware version.
    GetFirmwareVersion,
    /// `KEY,[KEY_CODE],[KEY_MODE]` — emulate a front-panel key.
    Key {
        /// Key code.
        code: KeyCode,
        /// Actuation mode.
        mode: KeyMode,
    },
    /// `QSH,[FRQ]` — go to quick search hold on a frequency.
    QuickSearchHold(Frequency),
    /// `STS` — legacy display status (specification recommends `PSI`/`GSI`).
    GetStatus,
    /// `JNT,[FL_TAG],[SYS_TAG],[CHAN_TAG]` — jump to number tag.
    JumpNumberTag {
        /// Favorites list number tag (0–99).
        favorites_list: u8,
        /// System number tag (0–99).
        system: u8,
        /// Channel number tag (0–999).
        channel: u16,
    },
    /// `FQK` — get favorites-list quick key status.
    GetFavoritesQuickKeys,
    /// `SQK,[FAV_QK]` — get system quick key status for a favorites list.
    GetSystemQuickKeys {
        /// Favorites list quick key.
        favorites_quick_key: u8,
    },
    /// `DQK,[FAV_QK],[SYS_QK]` — get department quick key status.
    GetDepartmentQuickKeys {
        /// Favorites list quick key.
        favorites_quick_key: u8,
        /// System quick key.
        system_quick_key: u8,
    },
    /// `PSI[,interval]` — push scanner information periodically. The
    /// interval argument (milliseconds; `0` stops pushes) is community-reported
    /// (`PSI,<ms>` → `PSI,OK`, then `PSI,<XML>,` pushes).
    PushScannerInfo {
        /// Requested interval in milliseconds; `None` sends bare `PSI`.
        interval_ms: Option<u32>,
    },
    /// `GSI` — get scanner information (XML).
    GetScannerInfo,
    /// `GLT,...` — get a list (XML).
    GetList(ListRequest),
    /// `HLD,[tkw],[xxx1],[xxx2]` — hold a system, department or channel.
    Hold(Target),
    /// `AVD,[tkw],[xxx1],[xxx2],[STATUS]` — avoid or unavoid.
    Avoid(Target, AvoidStatus),
    /// `JPM,[JUMP_MODE],[INDEX]` — jump to a mode.
    JumpMode {
        /// Target mode.
        mode: JumpModeKind,
        /// Mode-specific index (see specification).
        index: Arg,
    },
    /// `DTM` — get date and time.
    GetDateTime,
    /// `LCR` — get location and range.
    GetLocation,
    /// `URC` — get user record status.
    GetUserRecord,
    /// `URC,[STATUS]` — start (`true`) or stop user recording.
    SetUserRecord(bool),
    /// `VOL` — get volume. *Community-reported.*
    GetVolume,
    /// `VOL,[LEVEL]` — set volume. *Community-reported.*
    SetVolume(Volume),
    /// `SQL` — get squelch. *Community-reported.*
    GetSquelch,
    /// `SQL,[LEVEL]` — set squelch. *Community-reported.*
    SetSquelch(Squelch),
    /// `POF` — power off. *Community-reported.*
    PowerOff,
    /// `KAL` — keep-alive. Community sources report it was added in SDS
    /// remote command specification V2.00 (2025-07-07) and has no reply.
    KeepAlive,
    /// Administrator raw command.
    Raw(RawCommand),
}

impl Command {
    /// The three-letter command name used to correlate replies.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::GetModel => "MDL",
            Self::GetFirmwareVersion => "VER",
            Self::Key { .. } => "KEY",
            Self::QuickSearchHold(_) => "QSH",
            Self::GetStatus => "STS",
            Self::JumpNumberTag { .. } => "JNT",
            Self::GetFavoritesQuickKeys => "FQK",
            Self::GetSystemQuickKeys { .. } => "SQK",
            Self::GetDepartmentQuickKeys { .. } => "DQK",
            Self::PushScannerInfo { .. } => "PSI",
            Self::GetScannerInfo => "GSI",
            Self::GetList(_) => "GLT",
            Self::Hold(_) => "HLD",
            Self::Avoid(..) => "AVD",
            Self::JumpMode { .. } => "JPM",
            Self::GetDateTime => "DTM",
            Self::GetLocation => "LCR",
            Self::GetUserRecord | Self::SetUserRecord(_) => "URC",
            Self::GetVolume | Self::SetVolume(_) => "VOL",
            Self::GetSquelch | Self::SetSquelch(_) => "SQL",
            Self::PowerOff => "POF",
            Self::KeepAlive => "KAL",
            Self::Raw(raw) => &raw.name,
        }
    }

    /// Whether the command may change scanner state.
    #[must_use]
    pub fn effect(&self) -> Effect {
        match self {
            Self::GetModel
            | Self::GetFirmwareVersion
            | Self::GetStatus
            | Self::GetFavoritesQuickKeys
            | Self::GetSystemQuickKeys { .. }
            | Self::GetDepartmentQuickKeys { .. }
            | Self::GetScannerInfo
            | Self::GetList(_)
            | Self::GetDateTime
            | Self::GetLocation
            | Self::GetUserRecord
            | Self::GetVolume
            | Self::GetSquelch
            | Self::KeepAlive => Effect::ReadOnly,
            // PSI changes the scanner's push behaviour, so it is not retried.
            Self::PushScannerInfo { .. }
            | Self::Key { .. }
            | Self::QuickSearchHold(_)
            | Self::JumpNumberTag { .. }
            | Self::Hold(_)
            | Self::Avoid(..)
            | Self::JumpMode { .. }
            | Self::SetUserRecord(_)
            | Self::SetVolume(_)
            | Self::SetSquelch(_)
            | Self::PowerOff => Effect::StateChanging,
            Self::Raw(raw) => raw.effect,
        }
    }

    /// Source of knowledge about this command.
    #[must_use]
    pub fn provenance(&self) -> Provenance {
        match self {
            Self::GetVolume
            | Self::SetVolume(_)
            | Self::GetSquelch
            | Self::SetSquelch(_)
            | Self::PowerOff
            | Self::KeepAlive => Provenance::CommunityReported,
            Self::Raw(_) => Provenance::Raw,
            _ => Provenance::PredecessorSpec,
        }
    }

    /// Expected reply framing.
    #[must_use]
    pub fn reply_format(&self) -> ReplyFormat {
        match self {
            Self::GetScannerInfo | Self::GetList(_) => ReplyFormat::Xml,
            Self::PushScannerInfo { .. } | Self::Raw(_) => ReplyFormat::Any,
            Self::KeepAlive => ReplyFormat::None,
            _ => ReplyFormat::Line,
        }
    }

    fn args(&self) -> Vec<Arg> {
        match self {
            Self::GetModel
            | Self::GetFirmwareVersion
            | Self::GetStatus
            | Self::GetFavoritesQuickKeys
            | Self::GetScannerInfo
            | Self::GetDateTime
            | Self::GetLocation
            | Self::GetUserRecord
            | Self::GetVolume
            | Self::GetSquelch
            | Self::PowerOff
            | Self::KeepAlive => Vec::new(),
            Self::Key { code, mode } => vec![
                Arg(code.as_char().to_string()),
                Arg(mode.as_char().to_string()),
            ],
            Self::QuickSearchHold(f) => vec![Arg::number(f.as_command_units())],
            Self::JumpNumberTag {
                favorites_list,
                system,
                channel,
            } => vec![
                Arg::number(favorites_list),
                Arg::number(system),
                Arg::number(channel),
            ],
            Self::GetSystemQuickKeys {
                favorites_quick_key,
            } => {
                vec![Arg::number(favorites_quick_key)]
            }
            Self::GetDepartmentQuickKeys {
                favorites_quick_key,
                system_quick_key,
            } => {
                vec![
                    Arg::number(favorites_quick_key),
                    Arg::number(system_quick_key),
                ]
            }
            Self::PushScannerInfo { interval_ms } => {
                interval_ms.map(Arg::number).into_iter().collect()
            }
            Self::GetList(req) => req.args(),
            Self::Hold(t) => {
                vec![
                    Arg(t.keyword.as_str().to_owned()),
                    t.first.clone(),
                    t.second.clone(),
                ]
            }
            Self::Avoid(t, status) => vec![
                Arg(t.keyword.as_str().to_owned()),
                t.first.clone(),
                t.second.clone(),
                Arg::number(match status {
                    AvoidStatus::Permanent => 1,
                    AvoidStatus::Temporary => 2,
                    AvoidStatus::Stop => 3,
                }),
            ],
            Self::JumpMode { mode, index } => vec![Arg(mode.as_str().to_owned()), index.clone()],
            Self::SetUserRecord(on) => vec![Arg::number(u8::from(*on))],
            Self::SetVolume(v) => vec![Arg::number(v.get())],
            Self::SetSquelch(s) => vec![Arg::number(s.get())],
            Self::Raw(raw) => raw.args.clone(),
        }
    }

    /// Validates numeric ranges that the type system does not capture.
    fn validate(&self) -> Result<(), EncodeError> {
        let check = |ok: bool, name: &'static str, reason: &str| {
            if ok {
                Ok(())
            } else {
                Err(EncodeError::InvalidArgument {
                    name,
                    reason: reason.to_owned(),
                })
            }
        };
        match self {
            Self::JumpNumberTag {
                favorites_list,
                system,
                channel,
            } => {
                check(*favorites_list <= 99, "FL_TAG", "must be 0-99")?;
                check(*system <= 99, "SYS_TAG", "must be 0-99")?;
                check(*channel <= 999, "CHAN_TAG", "must be 0-999")
            }
            Self::GetSystemQuickKeys {
                favorites_quick_key,
            } => check(*favorites_quick_key <= 99, "FAV_QK", "must be 0-99"),
            Self::GetDepartmentQuickKeys {
                favorites_quick_key,
                system_quick_key,
            } => {
                check(*favorites_quick_key <= 99, "FAV_QK", "must be 0-99")?;
                check(*system_quick_key <= 99, "SYS_QK", "must be 0-99")
            }
            _ => Ok(()),
        }
    }

    /// Encodes the command as `NAME[,arg...]\r`.
    ///
    /// # Errors
    /// Returns an error if an argument is out of range or the encoded command
    /// exceeds [`MAX_COMMAND_LEN`].
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        self.validate()?;
        let mut out = String::with_capacity(32);
        out.push_str(self.name());
        for arg in self.args() {
            out.push(',');
            out.push_str(arg.as_str());
        }
        out.push('\r');
        if out.len() > MAX_COMMAND_LEN {
            return Err(EncodeError::TooLong {
                len: out.len(),
                max: MAX_COMMAND_LEN,
            });
        }
        Ok(out.into_bytes())
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())?;
        for arg in self.args() {
            f.write_char(',')?;
            f.write_str(arg.as_str())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(c: &Command) -> String {
        String::from_utf8(c.encode().unwrap()).unwrap()
    }

    #[test]
    fn encodes_documented_commands() {
        assert_eq!(enc(&Command::GetModel), "MDL\r");
        assert_eq!(enc(&Command::GetFirmwareVersion), "VER\r");
        assert_eq!(enc(&Command::GetScannerInfo), "GSI\r");
        assert_eq!(
            enc(&Command::Key {
                code: KeyCode::E_YES,
                mode: KeyMode::Press
            }),
            "KEY,E,P\r"
        );
        assert_eq!(
            enc(&Command::GetList(ListRequest::FavoritesLists)),
            "GLT,FL\r"
        );
        assert_eq!(
            enc(&Command::GetList(ListRequest::Departments { system: 283 })),
            "GLT,DEPT,283\r"
        );
        assert_eq!(enc(&Command::Hold(Target::system(283))), "HLD,SYS,283,\r");
        assert_eq!(
            enc(&Command::Avoid(
                Target::custom(
                    TargetKeyword::AvoidingFrequency,
                    Arg::number(4_060_000),
                    Arg::none()
                ),
                AvoidStatus::Permanent
            )),
            // Verbatim example from the specification ("Note 1").
            "AVD,AFREQ,4060000,,1\r"
        );
        assert_eq!(
            enc(&Command::QuickSearchHold(
                Frequency::from_hz(406_000_000).unwrap()
            )),
            "QSH,4060000\r"
        );
        assert_eq!(
            enc(&Command::JumpMode {
                mode: JumpModeKind::Scan,
                index: Arg::number(SCAN_FROM_TOP)
            }),
            "JPM,SCN_MODE,4294967295\r"
        );
        assert_eq!(
            enc(&Command::SetVolume(Volume::new(12).unwrap())),
            "VOL,12\r"
        );
    }

    #[test]
    fn rejects_out_of_range_tags() {
        let c = Command::JumpNumberTag {
            favorites_list: 100,
            system: 0,
            channel: 0,
        };
        assert!(c.encode().is_err());
        let c = Command::JumpNumberTag {
            favorites_list: 1,
            system: 2,
            channel: 1000,
        };
        assert!(c.encode().is_err());
    }

    #[test]
    fn raw_commands_are_validated_and_state_changing_by_default() {
        assert!(RawCommand::new("mdl", vec![]).is_err());
        assert!(RawCommand::new("MDL\r", vec![]).is_err());
        assert!(Arg::new("a,b").is_err());
        assert!(Arg::new("a\rb").is_err());
        let raw = RawCommand::new("GLG", vec![]).unwrap();
        assert_eq!(Command::Raw(raw.clone()).effect(), Effect::StateChanging);
        assert_eq!(Command::Raw(raw.read_only()).effect(), Effect::ReadOnly);
        let long = RawCommand::new("ABC", vec![Arg::new("x".repeat(600)).unwrap()]).unwrap();
        assert!(matches!(
            Command::Raw(long).encode(),
            Err(EncodeError::TooLong { .. })
        ));
    }

    #[test]
    fn mutating_commands_are_flagged() {
        assert_eq!(Command::PowerOff.effect(), Effect::StateChanging);
        assert_eq!(
            Command::PushScannerInfo { interval_ms: None }.effect(),
            Effect::StateChanging
        );
        assert_eq!(Command::GetVolume.effect(), Effect::ReadOnly);
        assert_eq!(
            Command::GetVolume.provenance(),
            Provenance::CommunityReported
        );
        assert_eq!(Command::GetModel.provenance(), Provenance::PredecessorSpec);
    }
}
