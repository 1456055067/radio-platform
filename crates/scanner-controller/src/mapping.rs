//! Execution of platform [`ScannerCommand`]s through the SDK.

use platform_common::{CommandError, CommandOutcome, HoldKind, KeyAction, ScannerCommand, now_utc};
use serde_json::json;
use uniden_sds::protocol::{
    Arg, Command, Frame, Frequency, KeyCode, KeyMode, RawCommand, Reply, ScannerError, Squelch,
    Target, Volume,
};
use uniden_sds::{Error, Scanner, Support};

fn invalid(e: impl std::fmt::Display) -> CommandError {
    CommandError::Invalid {
        message: e.to_string(),
    }
}

/// Maps an SDK error to a platform command error.
#[must_use]
pub fn map_error(e: &Error) -> CommandError {
    match e {
        Error::Timeout {
            state_change_unknown,
            ..
        } => CommandError::Timeout {
            state_change_unknown: *state_change_unknown,
        },
        Error::Rejected {
            error: ScannerError::CommandError,
            ..
        } => CommandError::Unsupported,
        Error::Rejected { error, .. } => CommandError::Rejected {
            message: error.to_string(),
        },
        Error::Encode(err) => invalid(err),
        Error::QueueFull | Error::Closed | Error::Io(_) => CommandError::Unavailable {
            message: e.to_string(),
        },
        Error::Decode { .. } | Error::UnexpectedReply { .. } => CommandError::Internal {
            message: e.to_string(),
        },
    }
}

fn frame_json(frame: &Frame) -> serde_json::Value {
    match frame {
        Frame::Line(l) => json!({ "format": "line", "command": l.command, "fields": l.fields }),
        Frame::Xml(x) => json!({ "format": "xml", "command": x.command, "documents": x.documents }),
    }
}

fn support_json(s: &Support) -> serde_json::Value {
    match s {
        Support::Supported => json!({ "status": "supported" }),
        Support::Rejected(e) => json!({ "status": "rejected", "detail": e.to_string() }),
        Support::NoReply => json!({ "status": "no_reply" }),
        Support::Undecodable(d) => json!({ "status": "undecodable", "detail": d }),
    }
}

/// Executes a command and returns JSON data.
///
/// # Errors
/// Returns a [`CommandError`] describing validation or scanner failures.
pub async fn execute(scanner: &Scanner, command: &ScannerCommand) -> CommandOutcome {
    let ok = || Ok(json!({ "ok": true }));
    let r = |res: Result<(), Error>| res.map_err(|e| map_error(&e)).and_then(|()| ok());
    match command {
        ScannerCommand::GetStatus => {
            let info = scanner.scanner_info().await.map_err(|e| map_error(&e))?;
            let snap = scanner_telemetry::snapshot_from_info(&info, now_utc());
            serde_json::to_value(snap).map_err(|e| CommandError::Internal {
                message: e.to_string(),
            })
        }
        ScannerCommand::ProbeCapabilities => {
            let caps = scanner.probe_capabilities().await;
            let commands: serde_json::Map<String, serde_json::Value> = caps
                .commands
                .iter()
                .map(|(k, v)| (k.clone(), support_json(v)))
                .collect();
            Ok(json!({
                "model": caps.model.map(|m| m.0),
                "firmware": caps.firmware.map(|f| f.raw),
                "commands": commands,
            }))
        }
        ScannerCommand::GetVolume => {
            let v = scanner.volume().await.map_err(|e| map_error(&e))?;
            Ok(json!({ "level": v.get(), "max": Volume::MAX }))
        }
        ScannerCommand::SetVolume { level } => r(scanner
            .set_volume(Volume::new(*level).map_err(invalid)?)
            .await),
        ScannerCommand::GetSquelch => {
            let s = scanner.squelch().await.map_err(|e| map_error(&e))?;
            Ok(json!({ "level": s.get(), "max": Squelch::MAX }))
        }
        ScannerCommand::SetSquelch { level } => r(scanner
            .set_squelch(Squelch::new(*level).map_err(invalid)?)
            .await),
        ScannerCommand::Hold { kind, index } => {
            let target = match kind {
                HoldKind::System => Target::system(*index),
                HoldKind::Department => Target::department(*index, None),
                HoldKind::Site => Target::site(*index),
                HoldKind::Channel => Target::conventional_channel(*index),
                HoldKind::TalkgroupChannel => Target::tgid_channel(*index),
            };
            r(scanner.hold(target).await)
        }
        ScannerCommand::Resume => r(scanner.resume_scan().await),
        ScannerCommand::Key { code, action } => {
            let code = KeyCode::new(*code).map_err(invalid)?;
            let mode = match action {
                KeyAction::Press => KeyMode::Press,
                KeyAction::LongPress => KeyMode::LongPress,
            };
            r(scanner.press_key(code, mode).await)
        }
        ScannerCommand::QuickSearchHold { frequency_hz } => {
            let f = Frequency::from_hz(*frequency_hz).map_err(invalid)?;
            r(scanner.quick_search_hold(f).await)
        }
        ScannerCommand::Raw {
            name,
            args,
            read_only,
        } => {
            let args = args
                .iter()
                .map(|a| Arg::new(a.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(invalid)?;
            let mut raw = RawCommand::new(name, args).map_err(invalid)?;
            if *read_only {
                raw = raw.read_only();
            }
            match scanner
                .execute(Command::Raw(raw))
                .await
                .map_err(|e| map_error(&e))?
            {
                Reply::Raw(frame) => Ok(frame_json(&frame)),
                other => Ok(json!({ "reply": format!("{other:?}") })),
            }
        }
    }
}
