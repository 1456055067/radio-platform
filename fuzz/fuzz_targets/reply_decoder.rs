#![no_main]
//! Decodes arbitrary datagrams as replies to every read-only command.
use libfuzzer_sys::fuzz_target;
use uniden_protocol::xml::XmlLimits;
use uniden_protocol::{Command, FrameAssembler, ListRequest, decode_reply};

fuzz_target!(|data: &[u8]| {
    let commands = [
        Command::GetModel,
        Command::GetFirmwareVersion,
        Command::GetStatus,
        Command::GetScannerInfo,
        Command::GetList(ListRequest::FavoritesLists),
        Command::GetFavoritesQuickKeys,
        Command::GetSystemQuickKeys { favorites_quick_key: 1 },
        Command::GetDateTime,
        Command::GetLocation,
        Command::GetUserRecord,
        Command::GetVolume,
        Command::GetSquelch,
    ];
    let mut a = FrameAssembler::default();
    if let Ok(frames) = a.push(data) {
        for f in frames {
            for c in &commands {
                let _ = decode_reply(c, f.clone(), &XmlLimits::default());
            }
        }
    }
});
