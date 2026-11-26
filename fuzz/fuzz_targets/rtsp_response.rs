#![no_main]
//! RTSP responses and SDP from the scanner's audio server are untrusted.
use libfuzzer_sys::fuzz_target;
use scanner_audio::rtsp::{audio_track, parse_response};

fuzz_target!(|data: &[u8]| {
    let mut rest = data;
    // Parse a stream of pipelined responses.
    while let Ok(Some((resp, used))) = parse_response(rest) {
        if let Ok(sdp) = std::str::from_utf8(&resp.body) {
            let _ = audio_track(sdp, "rtsp://192.0.2.1/au:scanner.au");
        }
        if used == 0 || used > rest.len() {
            break;
        }
        rest = &rest[used..];
    }
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = audio_track(s, "rtsp://192.0.2.1/au:scanner.au");
    }
});
