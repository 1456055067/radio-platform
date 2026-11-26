//! Strongly typed protocol values.

use std::fmt;
use std::str::FromStr;

use crate::error::{DecodeError, EncodeError};

/// A radio frequency with 1 Hz resolution.
///
/// The remote command specification encodes frequency *arguments* as an
/// integer count of 100 Hz units (for example `4060000` = 406.0 MHz, from the
/// `AVD` example in the BCDx36HP specification v1.03), and frequency
/// *attributes* in XML as `xxxx.xxxxMHz`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Frequency {
    hz: u64,
}

impl Frequency {
    /// Highest frequency accepted (10 GHz). This is a sanity bound, not a
    /// statement about receiver coverage.
    pub const MAX_HZ: u64 = 10_000_000_000;

    /// Creates a frequency from hertz.
    ///
    /// # Errors
    /// Returns an error if the value is zero or above [`Self::MAX_HZ`].
    pub fn from_hz(hz: u64) -> Result<Self, EncodeError> {
        if hz == 0 || hz > Self::MAX_HZ {
            return Err(EncodeError::InvalidArgument {
                name: "frequency",
                reason: format!("{hz} Hz is outside 1..={}", Self::MAX_HZ),
            });
        }
        Ok(Self { hz })
    }

    /// Frequency in hertz.
    #[must_use]
    pub const fn hz(self) -> u64 {
        self.hz
    }

    /// Frequency in the 100 Hz units used for command arguments. Any
    /// sub-100 Hz remainder is truncated.
    #[must_use]
    pub const fn as_command_units(self) -> u64 {
        self.hz / 100
    }

    /// Parses a 100 Hz-unit command argument such as `4060000`.
    ///
    /// # Errors
    /// Returns an error if the field is not a positive integer in range.
    pub fn from_command_units(field: &str) -> Result<Self, DecodeError> {
        let units: u64 = field
            .trim()
            .parse()
            .map_err(|_| DecodeError::InvalidField {
                field: "frequency",
                reason: format!("`{field}` is not an integer"),
            })?;
        let hz = units.checked_mul(100).ok_or(DecodeError::InvalidField {
            field: "frequency",
            reason: "overflow".into(),
        })?;
        Self::from_hz(hz).map_err(|e| DecodeError::InvalidField {
            field: "frequency",
            reason: e.to_string(),
        })
    }

    /// Parses an XML attribute value such as `" 154.4150MHz"`.
    ///
    /// # Errors
    /// Returns an error if the value is not a decimal MHz value.
    pub fn from_mhz_str(value: &str) -> Result<Self, DecodeError> {
        let invalid = |reason: &str| DecodeError::InvalidField {
            field: "frequency",
            reason: format!("`{value}`: {reason}"),
        };
        let trimmed = value.trim();
        let number = trimmed
            .strip_suffix("MHz")
            .or_else(|| trimmed.strip_suffix("mhz"))
            .unwrap_or(trimmed)
            .trim();
        let (int_part, frac_part) = match number.split_once('.') {
            Some((i, f)) => (i, f),
            None => (number, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(invalid("empty"));
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
            || frac_part.len() > 6
            || int_part.len() > 5
        {
            return Err(invalid("not a decimal MHz value"));
        }
        let mhz: u64 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().map_err(|_| invalid("integer part"))?
        };
        let mut frac_hz: u64 = 0;
        let mut scale: u64 = 100_000;
        for b in frac_part.bytes() {
            frac_hz += u64::from(b - b'0') * scale;
            scale /= 10;
        }
        let hz = mhz * 1_000_000 + frac_hz;
        Self::from_hz(hz).map_err(|e| invalid(&e.to_string()))
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:04}MHz",
            self.hz / 1_000_000,
            (self.hz % 1_000_000) / 100
        )
    }
}

macro_rules! bounded_level {
    ($(#[$meta:meta])* $name:ident, $field:literal, $max:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u8);

        impl $name {
            /// Highest accepted level.
            pub const MAX: u8 = $max;

            /// Creates a validated level.
            ///
            /// # Errors
            /// Returns an error if `level` exceeds [`Self::MAX`].
            pub fn new(level: u8) -> Result<Self, EncodeError> {
                if level > Self::MAX {
                    return Err(EncodeError::InvalidArgument {
                        name: $field,
                        reason: format!("{level} exceeds maximum {}", Self::MAX),
                    });
                }
                Ok(Self(level))
            }

            /// The raw level.
            #[must_use]
            pub const fn get(self) -> u8 {
                self.0
            }
        }

        impl FromStr for $name {
            type Err = DecodeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let v: u8 = s.trim().parse().map_err(|_| DecodeError::InvalidField {
                    field: $field,
                    reason: format!("`{s}` is not an integer"),
                })?;
                Self::new(v).map_err(|e| DecodeError::InvalidField {
                    field: $field,
                    reason: e.to_string(),
                })
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

bounded_level!(
    /// Volume level.
    ///
    /// The BCDx36HP specification lists the GSI `Property VOL` range as
    /// "0-29 or 0-15" depending on model. The SDS200 range is **not yet
    /// hardware-verified**; 0–29 is accepted here as the documented upper bound.
    Volume,
    "volume",
    29
);

bounded_level!(
    /// Squelch level.
    ///
    /// The BCDx36HP specification lists the GSI `Property SQL` range as
    /// "0-19 or 0-15" depending on model. The SDS200 range is **not yet
    /// hardware-verified**; 0–19 is accepted here as the documented upper bound.
    Squelch,
    "squelch",
    19
);

/// A front-panel key code for the `KEY` command.
///
/// Key codes are single printable ASCII characters. The official key-code
/// table ("key code for KEY Command" sheet) was not present in the located
/// copy of the specification. `E` and `.` appear verbatim in the BCDx36HP
/// v1.03 popup-button example; all other constants are **community-reported**
/// (identical tables in the `noctule-scan`, `sds200ha` and `sdsctl` projects,
/// which note the source table's columns are labelled BCD536HP/SDS100) and
/// require SDS200 hardware verification. Arbitrary codes are accepted after
/// framing validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCode(u8);

impl KeyCode {
    /// `E` — E / Yes key (documented in the popup-button example).
    pub const E_YES: Self = Self(b'E');
    /// `.` — decimal / No key (documented in the popup-button example).
    pub const DOT_NO: Self = Self(b'.');
    /// `M` — Menu (community-reported).
    pub const MENU: Self = Self(b'M');
    /// `F` — Function (community-reported).
    pub const FUNCTION: Self = Self(b'F');
    /// `L` — Avoid (community-reported).
    pub const AVOID: Self = Self(b'L');
    /// `>` — rotary knob clockwise (community-reported).
    pub const ROTARY_RIGHT: Self = Self(b'>');
    /// `<` — rotary knob counter-clockwise (community-reported).
    pub const ROTARY_LEFT: Self = Self(b'<');
    /// `^` — rotary knob push (community-reported).
    pub const ROTARY_PUSH: Self = Self(b'^');
    /// `V` — volume knob push (community-reported).
    pub const VOLUME_PUSH: Self = Self(b'V');
    /// `Q` — squelch knob push (community-reported; flagged unverified on
    /// SDS200 by community sources).
    pub const SQUELCH_PUSH: Self = Self(b'Q');
    /// `Y` — Replay (community-reported).
    pub const REPLAY: Self = Self(b'Y');
    /// `A` — System soft key (community-reported).
    pub const SOFT_SYSTEM: Self = Self(b'A');
    /// `B` — Department soft key (community-reported).
    pub const SOFT_DEPARTMENT: Self = Self(b'B');
    /// `C` — Channel soft key (community-reported).
    pub const SOFT_CHANNEL: Self = Self(b'C');
    /// `Z` — ZIP (community-reported).
    pub const ZIP: Self = Self(b'Z');
    /// `T` — Service type (community-reported; flagged unverified on SDS200).
    pub const SERVICE_TYPE: Self = Self(b'T');
    /// `R` — Range (community-reported).
    pub const RANGE: Self = Self(b'R');

    /// Numeric key `0`–`9`.
    ///
    /// # Errors
    /// Returns an error if `digit > 9`.
    pub fn digit(digit: u8) -> Result<Self, EncodeError> {
        if digit > 9 {
            return Err(EncodeError::InvalidArgument {
                name: "key_code",
                reason: format!("{digit} is not a single digit"),
            });
        }
        Ok(Self(b'0' + digit))
    }

    /// Creates a key code from a single printable ASCII character.
    ///
    /// # Errors
    /// Rejects non-ASCII, control characters, whitespace and the `,`
    /// field separator.
    pub fn new(c: char) -> Result<Self, EncodeError> {
        if !c.is_ascii_graphic() || c == ',' {
            return Err(EncodeError::InvalidArgument {
                name: "key_code",
                reason: format!("{c:?} is not a permitted key code character"),
            });
        }
        // `c` is ASCII-graphic, so it fits in a byte.
        Ok(Self(c as u8))
    }

    /// The key code character.
    #[must_use]
    pub const fn as_char(self) -> char {
        self.0 as char
    }
}

/// How a key is actuated by the `KEY` command.
///
/// Only `P` (press) appears verbatim in the BCDx36HP v1.03 specification
/// (`KEY,E,P`). `L` (long press) is community-reported. Other modes are
/// accepted through [`KeyMode::Other`] for hardware verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyMode {
    /// Momentary press (`P`).
    Press,
    /// Long press (`L`, community-reported).
    LongPress,
    /// Another single-character mode, not documented in located sources.
    Other(KeyCode),
}

impl KeyMode {
    pub(crate) fn as_char(self) -> char {
        match self {
            Self::Press => 'P',
            Self::LongPress => 'L',
            Self::Other(code) => code.as_char(),
        }
    }
}

/// Scanner model name returned by `MDL`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Model(pub String);

impl Model {
    /// True if the model string identifies an SDS200.
    #[must_use]
    pub fn is_sds200(&self) -> bool {
        self.0.trim().eq_ignore_ascii_case("SDS200")
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Firmware version returned by `VER` (documented as `Version x.xx.xx`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FirmwareVersion {
    /// The raw string as reported by the scanner.
    pub raw: String,
    /// Parsed numeric components, if the string follows `Version a.b.c`.
    pub components: Option<(u16, u16, u16)>,
}

impl FirmwareVersion {
    /// Parses the `[VERSION]` field.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim().to_owned();
        let numeric = raw
            .strip_prefix("Version")
            .map_or(raw.as_str(), str::trim)
            .trim_start_matches(['V', 'v']);
        let mut parts = numeric.split('.').map(|p| p.trim().parse::<u16>());
        let components = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Ok(a)), Some(Ok(b)), Some(Ok(c)), None) => Some((a, b, c)),
            (Some(Ok(a)), Some(Ok(b)), None, None) => Some((a, b, 0)),
            _ => None,
        };
        Self { raw, components }
    }
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_round_trips() {
        let f = Frequency::from_mhz_str(" 154.4150MHz").unwrap();
        assert_eq!(f.hz(), 154_415_000);
        assert_eq!(f.as_command_units(), 1_544_150);
        assert_eq!(f.to_string(), "154.4150MHz");
        assert_eq!(
            Frequency::from_command_units("4060000").unwrap().hz(),
            406_000_000
        );
    }

    #[test]
    fn frequency_rejects_garbage() {
        for bad in [
            "",
            "MHz",
            "abc",
            "1.2.3MHz",
            "-5MHz",
            "999999MHz",
            "1.1234567MHz",
        ] {
            assert!(Frequency::from_mhz_str(bad).is_err(), "{bad}");
        }
        assert!(Frequency::from_command_units("0").is_err());
        assert!(Frequency::from_command_units("18446744073709551615").is_err());
    }

    #[test]
    fn levels_are_bounded() {
        assert!(Volume::new(29).is_ok());
        assert!(Volume::new(30).is_err());
        assert!(Squelch::new(19).is_ok());
        assert!(Squelch::new(20).is_err());
        assert_eq!("7".parse::<Volume>().unwrap().get(), 7);
        assert!("x".parse::<Squelch>().is_err());
    }

    #[test]
    fn key_code_rejects_framing_characters() {
        assert!(KeyCode::new(',').is_err());
        assert!(KeyCode::new('\r').is_err());
        assert!(KeyCode::new(' ').is_err());
        assert!(KeyCode::new('é').is_err());
        assert_eq!(KeyCode::new('M').unwrap().as_char(), 'M');
    }

    #[test]
    fn firmware_version_parsing() {
        let v = FirmwareVersion::parse("Version 1.23.04");
        assert_eq!(v.components, Some((1, 23, 4)));
        assert_eq!(FirmwareVersion::parse("garbage").components, None);
    }
}
