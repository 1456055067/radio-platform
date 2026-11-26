//! Transmission recording (Phase 3). **Disabled by default**: recording
//! must be enabled in the gateway configuration and per scanner, and only
//! for communications the operator is authorized to record.
//!
//! - [`segment::Segmenter`] cuts transmissions from the agents' Opus stream
//!   (pre-buffer, post-roll, duration cap).
//! - [`encode_ogg`] writes a segment as Ogg Opus (RFC 7845).
//! - [`store::RecordingStore`] stores it in S3-compatible storage, a local
//!   directory, or memory.

#![warn(missing_docs)]

pub mod segment;
pub mod store;

pub use segment::{Segment, SegmentPolicy, Segmenter};
pub use store::{CONTENT_TYPE, RecordingStore, StorageConfig, StorageError};

/// Opus encoder pre-skip in 48 kHz samples for the agent's 8 kHz VoIP
/// encoder (libopus lookahead, 6.5 ms).
pub const PRE_SKIP: u16 = 312;

/// Encodes a segment as an Ogg Opus file.
#[must_use]
pub fn encode_ogg(seg: &Segment) -> Vec<u8> {
    let serial = u32::from_le_bytes(
        seg.transmission_id.0.as_bytes()[..4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    let tx = seg.transmission_id.to_string();
    let sc = seg.scanner_id.to_string();
    let mut w = scanner_audio::ogg::OggOpusWriter::new(
        serial,
        PRE_SKIP,
        8000,
        "radio-platform",
        &[("TRANSMISSION", tx.as_str()), ("SCANNER", sc.as_str())],
    );
    for f in &seg.frames {
        w.push(&f.opus, scanner_audio::FRAME_48K);
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn encodes_and_stores_round_trip() {
        let seg = Segment {
            transmission_id: platform_common::TransmissionId::new(),
            scanner_id: platform_common::ScannerId::new(),
            frames: (0..50)
                .map(|i| {
                    Arc::new(platform_common::wire::media::MediaFrame {
                        scanner_id: platform_common::ScannerId::new(),
                        captured_ms: i * 20,
                        seq: u32::try_from(i).unwrap_or(0),
                        concealed: false,
                        opus: vec![0xF8, 0xFF, 0xFE],
                    })
                })
                .collect(),
            truncated: false,
        };
        let ogg = encode_ogg(&seg);
        assert_eq!(ogg.get(..4), Some(&b"OggS"[..]));
        let store = RecordingStore::open(&StorageConfig::Memory).unwrap_or_else(|e| panic!("{e}"));
        let key = RecordingStore::key(
            platform_common::TenantId::new(),
            platform_common::SiteId::new(),
            seg.scanner_id,
            time::macros::datetime!(2026-11-25 10:00 UTC),
            seg.transmission_id,
        );
        assert!(key.contains("/2026/11/25/") && key.ends_with(".opus"));
        store
            .put(&key, ogg.clone())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(store.get(&key).await.unwrap_or_else(|e| panic!("{e}")), ogg);
        assert!(store.get("missing").await.is_err());
        store.delete(&key).await.unwrap_or_else(|e| panic!("{e}"));
        assert!(store.get(&key).await.is_err());
        store
            .delete(&key)
            .await
            .unwrap_or_else(|e| panic!("deleting twice: {e}"));
    }
}
