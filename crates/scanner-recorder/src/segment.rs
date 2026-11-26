//! Cuts transmissions out of a scanner's continuous Opus stream.
//!
//! The agent streams 20 ms frames stamped with its own clock; transmission
//! start and end events carry the same clock, so a segment is the frames
//! with `started - pre_buffer <= captured < ended + post_roll`. Frames that
//! arrive before the start event are kept in a short ring so the pre-buffer
//! works even when the event is a little late. Segments are capped at
//! `max_duration`; one with no frames at all produces nothing.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use platform_common::wire::media::MediaFrame;
use platform_common::{ScannerId, TransmissionId};

/// Segmentation settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentPolicy {
    /// Audio kept from before the start event, in ms.
    pub pre_buffer_ms: i64,
    /// Audio kept after the end event, in ms.
    pub post_roll_ms: i64,
    /// Longest recording, in ms.
    pub max_duration_ms: i64,
    /// Finish a segment whose end event never arrives after this long
    /// without frames, in ms.
    pub stale_ms: i64,
}

impl Default for SegmentPolicy {
    fn default() -> Self {
        Self {
            pre_buffer_ms: 1000,
            post_roll_ms: 500,
            max_duration_ms: 300_000,
            stale_ms: 30_000,
        }
    }
}

/// A finished recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Transmission.
    pub transmission_id: TransmissionId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Frames in capture order.
    pub frames: Vec<Arc<MediaFrame>>,
    /// Hit the duration cap.
    pub truncated: bool,
}

#[derive(Debug)]
struct Open {
    scanner_id: ScannerId,
    from_ms: i64,
    until_ms: Option<i64>,
    frames: Vec<Arc<MediaFrame>>,
    last_activity_ms: i64,
}

/// The segmenter (one per gateway replica, all scanners).
#[derive(Debug)]
pub struct Segmenter {
    policy: SegmentPolicy,
    ring: HashMap<ScannerId, VecDeque<Arc<MediaFrame>>>,
    open: HashMap<TransmissionId, Open>,
}

impl Segmenter {
    /// Creates a segmenter.
    #[must_use]
    pub fn new(policy: SegmentPolicy) -> Self {
        Self {
            policy,
            ring: HashMap::new(),
            open: HashMap::new(),
        }
    }

    /// Open segments.
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// A transmission started (only call for scanners that record).
    pub fn start(&mut self, id: TransmissionId, scanner_id: ScannerId, started_ms: i64) {
        let from_ms = started_ms - self.policy.pre_buffer_ms;
        let frames = self
            .ring
            .get(&scanner_id)
            .map(|r| {
                r.iter()
                    .filter(|f| f.captured_ms >= from_ms)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.open.insert(
            id,
            Open {
                scanner_id,
                from_ms,
                until_ms: None,
                frames,
                last_activity_ms: started_ms,
            },
        );
    }

    /// A transmission ended.
    pub fn end(&mut self, id: TransmissionId, ended_ms: i64) {
        if let Some(o) = self.open.get_mut(&id) {
            o.until_ms = Some(ended_ms + self.policy.post_roll_ms);
        }
    }

    /// Adds a frame; returns segments it completed.
    pub fn frame(&mut self, f: Arc<MediaFrame>) -> Vec<Segment> {
        let ring = self.ring.entry(f.scanner_id).or_default();
        ring.push_back(Arc::clone(&f));
        let keep_from = f.captured_ms - self.policy.pre_buffer_ms - 1000;
        while ring.front().is_some_and(|x| x.captured_ms < keep_from) {
            ring.pop_front();
        }
        let mut done = Vec::new();
        for (id, o) in &mut self.open {
            if o.scanner_id != f.scanner_id {
                continue;
            }
            o.last_activity_ms = f.captured_ms;
            let past_end = o.until_ms.is_some_and(|u| f.captured_ms >= u);
            let too_long = f.captured_ms - o.from_ms >= self.policy.max_duration_ms;
            if !past_end && f.captured_ms >= o.from_ms {
                o.frames.push(Arc::clone(&f));
            }
            if past_end || too_long {
                done.push((*id, too_long && !past_end));
            }
        }
        done.into_iter()
            .filter_map(|(id, truncated)| self.finish(id, truncated))
            .collect()
    }

    /// Finishes segments whose stream stopped (`now_ms` on the agent's
    /// clock as best known).
    pub fn flush_stale(&mut self, now_ms: i64) -> Vec<Segment> {
        let stale: Vec<TransmissionId> = self
            .open
            .iter()
            .filter(|(_, o)| {
                o.until_ms.is_some_and(|u| now_ms >= u + 2000)
                    || now_ms - o.last_activity_ms >= self.policy.stale_ms
            })
            .map(|(id, _)| *id)
            .collect();
        stale
            .into_iter()
            .filter_map(|id| self.finish(id, false))
            .collect()
    }

    fn finish(&mut self, id: TransmissionId, truncated: bool) -> Option<Segment> {
        let mut o = self.open.remove(&id)?;
        if o.frames.is_empty() {
            return None;
        }
        o.frames.sort_by_key(|f| (f.captured_ms, f.seq));
        o.frames.dedup_by_key(|f| f.seq);
        Some(Segment {
            transmission_id: id,
            scanner_id: o.scanner_id,
            frames: o.frames,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: ScannerId, ms: i64) -> Arc<MediaFrame> {
        Arc::new(MediaFrame {
            scanner_id: s,
            captured_ms: ms,
            seq: u32::try_from(ms / 20).unwrap_or(0),
            concealed: false,
            opus: vec![1],
        })
    }

    #[test]
    fn pre_buffer_post_roll_and_other_scanners() {
        let mut seg = Segmenter::new(SegmentPolicy::default());
        let (a, b) = (ScannerId::new(), ScannerId::new());
        for ms in (0..2000).step_by(20) {
            assert!(seg.frame(f(a, ms)).is_empty());
            seg.frame(f(b, ms));
        }
        let tx = TransmissionId::new();
        seg.start(tx, a, 2000); // event slightly late: ring supplies 1 s
        for ms in (2000..5000).step_by(20) {
            seg.frame(f(b, ms));
            assert!(seg.frame(f(a, ms)).is_empty());
        }
        seg.end(tx, 5000);
        let mut out = Vec::new();
        for ms in (5000..6000).step_by(20) {
            out.extend(seg.frame(f(a, ms)));
        }
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(s.scanner_id, a);
        assert_eq!(
            s.frames.first().map(|x| x.captured_ms),
            Some(1000),
            "1 s pre-buffer"
        );
        assert_eq!(
            s.frames.last().map(|x| x.captured_ms),
            Some(5480),
            "0.5 s post-roll"
        );
        assert!(s.frames.iter().all(|x| x.scanner_id == a));
        assert_eq!(seg.open_count(), 0);
    }

    #[test]
    fn caps_duration_and_flushes_stale() {
        let mut seg = Segmenter::new(SegmentPolicy {
            max_duration_ms: 1000,
            ..SegmentPolicy::default()
        });
        let s = ScannerId::new();
        let tx = TransmissionId::new();
        seg.start(tx, s, 0);
        let mut out = Vec::new();
        for ms in (0..3000).step_by(20) {
            out.extend(seg.frame(f(s, ms)));
        }
        assert_eq!(out.len(), 1);
        assert!(out[0].truncated);
        // A segment whose stream stops is flushed after the end event.
        let mut seg = Segmenter::new(SegmentPolicy::default());
        let tx2 = TransmissionId::new();
        seg.start(tx2, s, 3000);
        seg.frame(f(s, 3000));
        seg.end(tx2, 3100);
        assert!(seg.flush_stale(4000).is_empty());
        assert_eq!(seg.flush_stale(6000).len(), 1);
        // No frames at all: nothing is written.
        let tx3 = TransmissionId::new();
        seg.start(tx3, ScannerId::new(), 0);
        assert!(seg.flush_stale(60_000).is_empty());
        assert_eq!(seg.open_count(), 0);
    }
}
