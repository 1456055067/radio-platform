//! Typed decoding of reply frames.

use thiserror::Error;

use crate::command::Command;
use crate::error::DecodeError;
use crate::frame::{Frame, LineFrame};
use crate::types::{FirmwareVersion, Model, Squelch, Volume};
use crate::xml::{GltList, ScannerInfo, XmlLimits, parse_documents};

/// A command rejection reported by the scanner.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScannerError {
    /// `ERR` — command not recognised or invalid (also returned for
    /// commands the firmware does not support).
    #[error("scanner rejected the command (ERR)")]
    CommandError,
    /// `NG` — command valid but not executable in the current state.
    #[error("command not available in the current scanner state (NG)")]
    NotAvailable,
    /// `FER` — framing error.
    #[error("scanner reported a framing error (FER)")]
    FramingError,
    /// `ORER` — overrun error.
    #[error("scanner reported an overrun error (ORER)")]
    OverrunError,
    /// `CMD,ERR,<code>` with a command-specific code (for example `URC`).
    #[error("scanner reported error code {0}")]
    Code(String),
}

/// Errors decoding a reply to a specific command.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResponseError {
    /// The scanner rejected the command.
    #[error(transparent)]
    Rejected(#[from] ScannerError),
    /// The reply could not be decoded.
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// State of one quick key (`FQK`/`SQK`/`DQK`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickKeyState {
    /// `0` — quick key does not exist.
    Absent,
    /// `1` — exists and disabled.
    Disabled,
    /// `2` — exists and enabled.
    Enabled,
}

/// Quick key states 0–99.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickKeys(pub Vec<QuickKeyState>);

/// One `STS` display line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLine {
    /// Characters (latin-1 mapped; may contain scanner font codes ≥ 0x80).
    pub text: String,
    /// Display mode attribute.
    pub mode: String,
}

/// Legacy `STS` display status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayStatus {
    /// `[DSP_FORM]`.
    pub form: String,
    /// Display lines.
    pub lines: Vec<DisplayLine>,
    /// Remaining reserved and backlight fields, verbatim.
    pub trailing: Vec<String>,
}

/// `DTM` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannerDateTime {
    /// Daylight saving flag, verbatim.
    pub daylight_saving: String,
    /// Year.
    pub year: u16,
    /// Month.
    pub month: u8,
    /// Day.
    pub day: u8,
    /// Hour.
    pub hour: u8,
    /// Minute.
    pub minute: u8,
    /// Second.
    pub second: u8,
    /// `RTC Status` (`true` = RTC OK).
    pub rtc_ok: bool,
}

/// `LCR` reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// Latitude in degrees.
    pub latitude: f64,
    /// Longitude in degrees.
    pub longitude: f64,
    /// Range, verbatim.
    pub range: String,
}

/// A decoded reply.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// `CMD,OK`.
    Ok,
    /// `MDL` reply.
    Model(Model),
    /// `VER` reply.
    FirmwareVersion(FirmwareVersion),
    /// `STS` reply.
    Status(DisplayStatus),
    /// `GSI`/`PSI` XML reply.
    ScannerInfo(Box<ScannerInfo>),
    /// `GLT` reply.
    List(GltList),
    /// `FQK`/`SQK`/`DQK` reply.
    QuickKeys(QuickKeys),
    /// `DTM` reply.
    DateTime(ScannerDateTime),
    /// `LCR` reply.
    Location(Location),
    /// `URC` reply (`true` = recording).
    UserRecord(bool),
    /// `VOL` reply.
    Volume(Volume),
    /// `SQL` reply.
    Squelch(Squelch),
    /// Undecoded frame (raw commands).
    Raw(Frame),
}

/// Returns the scanner error carried by a frame, if any.
#[must_use]
pub fn scanner_error(frame: &Frame) -> Option<ScannerError> {
    let Frame::Line(LineFrame { command, fields }) = frame else {
        return None;
    };
    let token = fields.first().unwrap_or(command).trim();
    match token {
        "ERR" => Some(fields.get(1).map_or(ScannerError::CommandError, |code| {
            ScannerError::Code(code.trim().to_owned())
        })),
        "NG" => Some(ScannerError::NotAvailable),
        "FER" => Some(ScannerError::FramingError),
        "ORER" => Some(ScannerError::OverrunError),
        _ => None,
    }
}

/// True if `frame` can be a reply to `command`: either it echoes the command
/// name or it is a bare error token (which carries no name to correlate).
#[must_use]
pub fn answers(frame: &Frame, command: &Command) -> bool {
    frame.command() == command.name() || frame.is_bare_error()
}

fn line_of<'a>(frame: &'a Frame, cmd: &Command) -> Result<&'a LineFrame, DecodeError> {
    match frame {
        Frame::Line(l) if l.command == cmd.name() => Ok(l),
        Frame::Line(l) => Err(DecodeError::UnexpectedCommand {
            expected: cmd.name().to_owned(),
            received: l.command.clone(),
        }),
        Frame::Xml(x) => Err(DecodeError::Malformed(format!(
            "expected line reply to `{}`, received XML for `{}`",
            cmd.name(),
            x.command
        ))),
    }
}

fn single_field<'a>(line: &'a LineFrame, field: &'static str) -> Result<&'a str, DecodeError> {
    match line.fields.as_slice() {
        [v] => Ok(v.as_str()),
        _ => Err(DecodeError::InvalidField {
            field,
            reason: format!("expected 1 field, found {}", line.fields.len()),
        }),
    }
}

fn expect_ok(line: &LineFrame) -> Result<Reply, DecodeError> {
    match line.fields.as_slice() {
        [v] if v.trim() == "OK" => Ok(Reply::Ok),
        _ => Err(DecodeError::Malformed(format!(
            "expected `{},OK`",
            line.command
        ))),
    }
}

fn num<T: std::str::FromStr>(v: &str, field: &'static str) -> Result<T, DecodeError> {
    v.trim().parse().map_err(|_| DecodeError::InvalidField {
        field,
        reason: format!("`{v}` is not a valid number"),
    })
}

fn quick_keys(fields: &[String]) -> Result<QuickKeys, DecodeError> {
    if fields.len() > 100 {
        return Err(DecodeError::InvalidField {
            field: "quick_keys",
            reason: format!("{} entries exceeds 100", fields.len()),
        });
    }
    fields
        .iter()
        .map(|f| match f.trim() {
            "0" => Ok(QuickKeyState::Absent),
            "1" => Ok(QuickKeyState::Disabled),
            "2" => Ok(QuickKeyState::Enabled),
            other => Err(DecodeError::InvalidField {
                field: "quick_key",
                reason: format!("`{other}` is not 0, 1 or 2"),
            }),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(QuickKeys)
}

fn decode_status(line: &LineFrame) -> Result<DisplayStatus, DecodeError> {
    let mut it = line.fields.iter();
    let form = it.next().ok_or(DecodeError::InvalidField {
        field: "DSP_FORM",
        reason: "missing".into(),
    })?;
    // DSP_FORM has one character per display line in older Uniden
    // protocols; this is used only as a bound on the number of line pairs.
    let line_count = form.trim().len().min(20);
    let mut lines = Vec::with_capacity(line_count);
    for _ in 0..line_count {
        match (it.next(), it.next()) {
            (Some(text), Some(mode)) => {
                lines.push(DisplayLine {
                    text: text.clone(),
                    mode: mode.clone(),
                });
            }
            _ => break,
        }
    }
    Ok(DisplayStatus {
        form: form.clone(),
        lines,
        trailing: it.cloned().collect(),
    })
}

/// Decodes `frame` as the reply to `command`.
///
/// # Errors
/// Returns [`ResponseError::Rejected`] if the scanner reported an error, or
/// [`ResponseError::Decode`] if the frame is malformed for this command.
pub fn decode_reply(
    command: &Command,
    frame: Frame,
    limits: &XmlLimits,
) -> Result<Reply, ResponseError> {
    if let Some(err) = scanner_error(&frame) {
        return Err(err.into());
    }
    let reply = match command {
        Command::Raw(_) => Reply::Raw(frame),
        Command::GetScannerInfo | Command::PushScannerInfo { .. } => match frame {
            Frame::Xml(x) if x.command == "GSI" || x.command == "PSI" => {
                let root = parse_documents(&x.documents, limits)?;
                Reply::ScannerInfo(Box::new(ScannerInfo::from_element(root)?))
            }
            other => expect_ok(line_of(&other, command)?)?,
        },
        Command::GetList(_) => match frame {
            Frame::Xml(x) if x.command == "GLT" => Reply::List(GltList::from_element(
                parse_documents(&x.documents, limits)?,
            )?),
            other => {
                return Err(DecodeError::Malformed(format!(
                    "expected GLT XML reply, received `{}`",
                    other.command()
                ))
                .into());
            }
        },
        _ => {
            let line = line_of(&frame, command)?;
            match command {
                Command::GetModel => {
                    Reply::Model(Model(single_field(line, "MODEL_NAME")?.trim().to_owned()))
                }
                Command::GetFirmwareVersion => {
                    Reply::FirmwareVersion(FirmwareVersion::parse(single_field(line, "VERSION")?))
                }
                Command::GetStatus => Reply::Status(decode_status(line)?),
                Command::GetFavoritesQuickKeys => Reply::QuickKeys(quick_keys(&line.fields)?),
                // SQK/DQK echo the selector key(s) before the 100 states.
                Command::GetSystemQuickKeys { .. } => {
                    Reply::QuickKeys(quick_keys(line.fields.get(2..).unwrap_or_default())?)
                }
                Command::GetDepartmentQuickKeys { .. } => {
                    Reply::QuickKeys(quick_keys(line.fields.get(2..).unwrap_or_default())?)
                }
                Command::GetDateTime => match line.fields.as_slice() {
                    [dst, y, mo, d, h, mi, s, rtc] => Reply::DateTime(ScannerDateTime {
                        daylight_saving: dst.clone(),
                        year: num(y, "YYYY")?,
                        month: num(mo, "MM")?,
                        day: num(d, "DD")?,
                        hour: num(h, "hh")?,
                        minute: num(mi, "mm")?,
                        second: num(s, "ss")?,
                        rtc_ok: rtc.trim() == "1",
                    }),
                    _ => return Err(DecodeError::Malformed("DTM expects 8 fields".into()).into()),
                },
                Command::GetLocation => match line.fields.as_slice() {
                    [lat, lon, range] => Reply::Location(Location {
                        latitude: num(lat, "LATITUDE")?,
                        longitude: num(lon, "LONGITUDE")?,
                        range: range.clone(),
                    }),
                    _ => return Err(DecodeError::Malformed("LCR expects 3 fields".into()).into()),
                },
                Command::GetUserRecord => match single_field(line, "STATUS")?.trim() {
                    "0" => Reply::UserRecord(false),
                    "1" => Reply::UserRecord(true),
                    other => {
                        return Err(DecodeError::InvalidField {
                            field: "STATUS",
                            reason: format!("`{other}`"),
                        }
                        .into());
                    }
                },
                Command::GetVolume => Reply::Volume(single_field(line, "LEVEL")?.parse()?),
                Command::GetSquelch => Reply::Squelch(single_field(line, "LEVEL")?.parse()?),
                _ => expect_ok(line)?,
            }
        }
    };
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FrameAssembler, XmlFrame};

    fn frame(bytes: &[u8]) -> Frame {
        FrameAssembler::default().push(bytes).unwrap().remove(0)
    }

    fn decode(cmd: &Command, bytes: &[u8]) -> Result<Reply, ResponseError> {
        decode_reply(cmd, frame(bytes), &XmlLimits::default())
    }

    #[test]
    fn decodes_model_and_version() {
        assert_eq!(
            decode(&Command::GetModel, b"MDL,SDS200\r").unwrap(),
            Reply::Model(Model("SDS200".into()))
        );
        let Reply::FirmwareVersion(v) =
            decode(&Command::GetFirmwareVersion, b"VER,Version 1.23.15\r").unwrap()
        else {
            panic!()
        };
        assert_eq!(v.components, Some((1, 23, 15)));
    }

    #[test]
    fn decodes_errors() {
        assert_eq!(
            decode(&Command::GetVolume, b"ERR\r"),
            Err(ResponseError::Rejected(ScannerError::CommandError))
        );
        assert_eq!(
            decode(&Command::GetVolume, b"VOL,NG\r"),
            Err(ResponseError::Rejected(ScannerError::NotAvailable))
        );
        assert_eq!(
            decode(&Command::SetUserRecord(true), b"URC,ERR,0002\r"),
            Err(ResponseError::Rejected(ScannerError::Code("0002".into())))
        );
        assert!(matches!(
            decode(&Command::GetModel, b"VER,x\r"),
            Err(ResponseError::Decode(DecodeError::UnexpectedCommand { .. }))
        ));
    }

    #[test]
    fn decodes_levels_and_ok() {
        assert_eq!(
            decode(&Command::GetVolume, b"VOL,15\r").unwrap(),
            Reply::Volume(Volume::new(15).unwrap())
        );
        assert!(decode(&Command::GetVolume, b"VOL,99\r").is_err());
        assert_eq!(
            decode(&Command::SetSquelch(Squelch::new(3).unwrap()), b"SQL,OK\r").unwrap(),
            Reply::Ok
        );
        assert!(decode(&Command::SetSquelch(Squelch::new(3).unwrap()), b"SQL,3\r").is_err());
    }

    #[test]
    fn decodes_quick_keys() {
        let mut s = String::from("FQK");
        for i in 0..100 {
            s.push(',');
            s.push_str(["0", "1", "2"][i % 3]);
        }
        s.push('\r');
        let Reply::QuickKeys(q) = decode(&Command::GetFavoritesQuickKeys, s.as_bytes()).unwrap()
        else {
            panic!()
        };
        assert_eq!(q.0.len(), 100);
        assert_eq!(q.0[2], QuickKeyState::Enabled);
        assert!(decode(&Command::GetFavoritesQuickKeys, b"FQK,3\r").is_err());
    }

    #[test]
    fn decodes_dtm_lcr_urc_sts() {
        let Reply::DateTime(d) =
            decode(&Command::GetDateTime, b"DTM,1,2026,11,24,13,05,09,1\r").unwrap()
        else {
            panic!()
        };
        assert_eq!((d.year, d.month, d.second, d.rtc_ok), (2026, 11, 9, true));
        let Reply::Location(l) =
            decode(&Command::GetLocation, b"LCR,30.2241,-93.2174,5.0\r").unwrap()
        else {
            panic!()
        };
        assert!((l.latitude - 30.2241).abs() < 1e-9);
        assert_eq!(
            decode(&Command::GetUserRecord, b"URC,1\r").unwrap(),
            Reply::UserRecord(true)
        );
        let Reply::Status(s) =
            decode(&Command::GetStatus, b"STS,11,LINE ONE,  ,LINE TWO,*,0,0\r").unwrap()
        else {
            panic!()
        };
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[1].text, "LINE TWO");
        assert_eq!(s.trailing, vec!["0", "0"]);
    }

    #[test]
    fn decodes_gsi_xml() {
        let x = Frame::Xml(XmlFrame {
            command: "GSI".into(),
            documents: vec![r#"<ScannerInfo Mode="Scan"><Property VOL="4"/></ScannerInfo>"#.into()],
        });
        let Reply::ScannerInfo(info) =
            decode_reply(&Command::GetScannerInfo, x, &XmlLimits::default()).unwrap()
        else {
            panic!()
        };
        assert_eq!(info.property.unwrap().volume, Some(4));
        assert_eq!(
            decode(
                &Command::PushScannerInfo {
                    interval_ms: Some(500)
                },
                b"PSI,OK\r"
            )
            .unwrap(),
            Reply::Ok
        );
    }

    #[test]
    fn answers_correlates_by_name() {
        assert!(answers(&frame(b"MDL,SDS200\r"), &Command::GetModel));
        assert!(answers(&frame(b"ERR\r"), &Command::GetModel));
        assert!(!answers(&frame(b"VER,1\r"), &Command::GetModel));
    }
}
