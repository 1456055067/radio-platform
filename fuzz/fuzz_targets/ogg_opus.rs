#![no_main]
//! Recordings are read back from object storage (which an attacker with
//! bucket access could alter) and decoded for transcription.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(ogg) = scanner_audio::ogg::read_ogg_opus(data) {
        assert!(ogg.packets.len() <= scanner_audio::ogg::MAX_PACKETS);
    }
    // Bounded output: never more than 2 s plus one frame.
    if let Ok(pcm) = scanner_transcription::decode_ogg_opus(data, 2) {
        assert!(pcm.len() <= 2 * 16_000 + 1920);
    }
});
