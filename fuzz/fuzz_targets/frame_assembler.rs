#![no_main]
//! Feeds arbitrary datagram sequences to the frame assembler.
use libfuzzer_sys::fuzz_target;
use uniden_protocol::{FrameAssembler, FrameLimits};

fuzz_target!(|data: &[u8]| {
    let mut a = FrameAssembler::new(FrameLimits { max_xml_len: 64 * 1024, ..FrameLimits::default() });
    // Use 0x1F as a datagram separator so one input exercises many pushes.
    for dg in data.split(|b| *b == 0x1f) {
        let _ = a.push(dg);
    }
});
