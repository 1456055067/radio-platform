#![no_main]
//! Exercises scalar value parsers.
use libfuzzer_sys::fuzz_target;
use uniden_protocol::{FirmwareVersion, Frequency, Squelch, Volume};

fuzz_target!(|s: &str| {
    let _ = Frequency::from_mhz_str(s);
    let _ = Frequency::from_command_units(s);
    let _ = FirmwareVersion::parse(s);
    let _ = s.parse::<Volume>();
    let _ = s.parse::<Squelch>();
});
