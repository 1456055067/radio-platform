#![no_main]
//! Binary media frames arrive from agents over the network.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(f) = platform_common::wire::media::decode(data) {
        assert_eq!(platform_common::wire::media::decode(&platform_common::wire::media::encode(&f)), Some(f));
    }
});
