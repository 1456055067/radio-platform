#![no_main]
//! Parses arbitrary text as ScannerInfo / GLT XML.
use libfuzzer_sys::fuzz_target;
use uniden_protocol::xml::{GltList, ScannerInfo, XmlLimits, parse_documents};

fuzz_target!(|data: &str| {
    let docs: Vec<String> = data.split('\u{1f}').map(str::to_owned).collect();
    if let Ok(root) = parse_documents(&docs, &XmlLimits::default()) {
        let _ = ScannerInfo::from_element(root.clone()).map(|i| (i.active_frequency(), i.is_holding()));
        let _ = GltList::from_element(root);
    }
});
