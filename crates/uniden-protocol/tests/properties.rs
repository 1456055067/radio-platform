//! Property-based tests for the protocol codec.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use proptest::prelude::*;
use uniden_protocol::xml::{XmlLimits, parse_documents};
use uniden_protocol::{
    Arg, Command, Frame, FrameAssembler, Frequency, RawCommand, Squelch, Volume, decode_reply,
};

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Arbitrary datagrams never panic the assembler or the XML parser.
    #[test]
    fn assembler_never_panics(datagrams in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..512), 0..8)) {
        let mut a = FrameAssembler::default();
        for d in &datagrams {
            if let Ok(frames) = a.push(d) {
                for f in frames {
                    if let Frame::Xml(x) = f {
                        let _ = parse_documents(&x.documents, &XmlLimits::default());
                    }
                }
            }
        }
    }

    /// XML-looking datagrams never panic.
    #[test]
    fn xmlish_never_panics(body in "[<>/=\" a-zA-Z0-9?!\\-\\[\\]&;#]{0,256}") {
        let mut a = FrameAssembler::default();
        let data = format!("GSI,<XML>,\r{body}");
        if let Ok(frames) = a.push(data.as_bytes()) {
            for f in frames {
                let _ = decode_reply(&Command::GetScannerInfo, f, &XmlLimits::default());
            }
        }
    }

    /// Frequencies survive both wire representations at 100 Hz resolution.
    #[test]
    fn frequency_round_trip(units in 1u64..100_000_000) {
        let f = Frequency::from_command_units(&units.to_string()).unwrap();
        prop_assert_eq!(f.as_command_units(), units);
        let back = Frequency::from_mhz_str(&f.to_string()).unwrap();
        prop_assert_eq!(back, f);
    }

    /// Encoded commands contain exactly one CR, at the end, regardless of
    /// argument content accepted by validation.
    #[test]
    fn encoded_commands_are_single_frames(args in prop::collection::vec("[ -~]{0,16}", 0..6)) {
        let args: Vec<Arg> = args.into_iter().filter_map(|a| Arg::new(a).ok()).collect();
        let n = args.len();
        let raw = RawCommand::new("ABC", args).unwrap();
        let wire = Command::Raw(raw).encode().unwrap();
        prop_assert_eq!(wire.iter().filter(|b| **b == b'\r').count(), 1);
        prop_assert_eq!(wire.last(), Some(&b'\r'));
        prop_assert!(!wire.contains(&b'\n'));
        prop_assert_eq!(wire.iter().filter(|b| **b == b',').count(), n);
    }

    /// Level setters encode and decode symmetrically.
    #[test]
    fn levels_round_trip(v in 0u8..=Volume::MAX, s in 0u8..=Squelch::MAX) {
        let vol = Volume::new(v).unwrap();
        prop_assert_eq!(Command::SetVolume(vol).encode().unwrap(), format!("VOL,{v}\r").into_bytes());
        prop_assert_eq!(v.to_string().parse::<Volume>().unwrap(), vol);
        prop_assert_eq!(s.to_string().parse::<Squelch>().unwrap(), Squelch::new(s).unwrap());
    }

    /// A multi-part footer sequence yields the same merged entries no matter
    /// how many entries each part holds.
    #[test]
    fn multipart_merge_is_partition_independent(n in 1usize..60, per_part in 1usize..10) {
        let parts: Vec<Vec<usize>> = (0..n).collect::<Vec<_>>().chunks(per_part).map(<[usize]>::to_vec).collect();
        let total = parts.len();
        let mut a = FrameAssembler::default();
        let mut frames = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let body: String = part.iter().map(|k| format!("<SYS Index=\"{k}\" Name=\"S{k}\"/>")).collect();
            let eot = u8::from(i + 1 == total);
            let dg = format!("GLT,<XML>,\r<?xml version=\"1.0\"?>\r<GLT>{body}<Footer No=\"{}\" EOT=\"{eot}\"/></GLT>\r", i + 1);
            frames.extend(a.push(dg.as_bytes()).unwrap());
        }
        prop_assert_eq!(frames.len(), 1);
        let reply = decode_reply(
            &Command::GetList(uniden_protocol::ListRequest::Systems { favorites_list: 0 }),
            frames.remove(0),
            &XmlLimits::default(),
        ).unwrap();
        let uniden_protocol::Reply::List(list) = reply else { panic!("not a list") };
        let idx: Vec<u32> = list.entries.iter().filter_map(|e| e.index).collect();
        prop_assert_eq!(idx, (0..n as u32).collect::<Vec<_>>());
    }
}
