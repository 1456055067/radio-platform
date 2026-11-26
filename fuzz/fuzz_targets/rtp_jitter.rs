#![no_main]
//! RTP packets are untrusted; sequence numbers may be adversarial.
use libfuzzer_sys::fuzz_target;
use scanner_audio::jitter::JitterBuffer;
use scanner_audio::rtp::RtpPacket;

fuzz_target!(|data: &[u8]| {
    let mut jb = JitterBuffer::new(3);
    for chunk in data.chunks(40) {
        if let Ok(p) = RtpPacket::parse(chunk) {
            jb.push(p.ssrc, p.sequence, p.timestamp, p.payload, p.timestamp);
            let _ = jb.pop_ready();
        }
    }
    let _ = jb.stats();
});
