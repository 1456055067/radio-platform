//! Reordering jitter buffer with loss accounting.
//!
//! Packets are released in sequence order once `depth` newer packets have
//! arrived after them, or immediately when contiguous. A missing sequence
//! number is reported as a gap once it can no longer arrive in time; late
//! and duplicate packets are dropped and counted. Interarrival jitter
//! follows RFC 3550 §6.4.1.

use std::collections::BTreeMap;

/// Reception statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RtpStats {
    /// Packets accepted.
    pub received: u64,
    /// Sequence numbers never received in time.
    pub lost: u64,
    /// Packets that arrived after their slot was released.
    pub late: u64,
    /// Duplicate packets.
    pub duplicates: u64,
    /// Packets that arrived out of order but in time.
    pub reordered: u64,
    /// Interarrival jitter estimate in timestamp units.
    pub jitter: u32,
    /// Times the stream restarted (SSRC change or a large sequence jump).
    pub resyncs: u64,
}

/// Output of the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Played {
    /// A packet's payload, in order.
    Packet {
        /// RTP timestamp.
        timestamp: u32,
        /// Payload.
        payload: Vec<u8>,
    },
    /// One missing packet.
    Lost,
}

/// Sequence jumps larger than this restart the stream instead of being
/// reported as loss.
const MAX_JUMP: u64 = 3000;

/// The jitter buffer.
#[derive(Debug)]
pub struct JitterBuffer {
    depth: usize,
    ssrc: Option<u32>,
    /// Extended sequence number of the next packet to release.
    next: Option<u64>,
    highest: Option<u64>,
    pending: BTreeMap<u64, (u32, Vec<u8>)>,
    stats: RtpStats,
    last_transit: Option<i64>,
    jitter_q4: i64,
    cycles: u64,
    max_pending: usize,
}

impl JitterBuffer {
    /// Holds up to `depth` packets for reordering (2–3 = 40–60 ms at
    /// 20 ms packets is typical).
    #[must_use]
    pub fn new(depth: usize) -> Self {
        Self {
            depth: depth.max(1),
            ssrc: None,
            next: None,
            highest: None,
            pending: BTreeMap::new(),
            stats: RtpStats::default(),
            last_transit: None,
            jitter_q4: 0,
            cycles: 0,
            max_pending: depth.max(1) * 4 + 8,
        }
    }

    /// Statistics so far.
    #[must_use]
    pub fn stats(&self) -> RtpStats {
        RtpStats {
            jitter: u32::try_from(self.jitter_q4 >> 4).unwrap_or(u32::MAX),
            ..self.stats
        }
    }

    fn extend(&mut self, seq: u16) -> u64 {
        let Some(h) = self.highest else {
            return u64::from(seq);
        };
        let base = self.cycles << 16;
        let cand = base | u64::from(seq);
        // Choose the candidate nearest the highest seen (handles wrap).
        let options = [cand.wrapping_sub(1 << 16), cand, cand + (1 << 16)];
        let best = options
            .into_iter()
            .min_by_key(|c| c.abs_diff(h))
            .unwrap_or(cand);
        if best >> 16 > self.cycles {
            self.cycles = best >> 16;
        }
        best
    }

    fn reset(&mut self, ssrc: u32) {
        if self.ssrc.is_some() {
            self.stats.resyncs += 1;
        }
        self.ssrc = Some(ssrc);
        self.next = None;
        self.highest = None;
        self.pending.clear();
        self.cycles = 0;
        self.last_transit = None;
    }

    /// Adds a packet. `arrival` is the arrival time in timestamp units
    /// (for jitter only).
    pub fn push(&mut self, ssrc: u32, seq: u16, timestamp: u32, payload: &[u8], arrival: u32) {
        if self.ssrc != Some(ssrc) {
            self.reset(ssrc);
        }
        let ext = self.extend(seq);
        if let Some(h) = self.highest
            && ext.abs_diff(h) > MAX_JUMP
        {
            self.reset(ssrc);
            return self.push(ssrc, seq, timestamp, payload, arrival);
        }
        if self.next.is_some_and(|n| ext < n) {
            self.stats.late += 1;
            return;
        }
        if self.pending.contains_key(&ext) {
            self.stats.duplicates += 1;
            return;
        }
        if self.highest.is_some_and(|h| ext < h) {
            self.stats.reordered += 1;
        }
        self.highest = Some(self.highest.map_or(ext, |h| h.max(ext)));
        self.next.get_or_insert(ext);
        self.stats.received += 1;
        // RFC 3550 jitter: J += (|D| - J) / 16, kept scaled by 16.
        let transit = i64::from(arrival) - i64::from(timestamp);
        if let Some(prev) = self.last_transit {
            let d = (transit - prev).abs();
            self.jitter_q4 += d - (self.jitter_q4 + 8) / 16;
        }
        self.last_transit = Some(transit);
        self.pending.insert(ext, (timestamp, payload.to_vec()));
    }

    /// Releases everything that is ready.
    pub fn pop_ready(&mut self) -> Vec<Played> {
        let mut out = Vec::new();
        while let (Some(next), Some(high)) = (self.next, self.highest) {
            if let Some((ts, payload)) = self.pending.remove(&next) {
                out.push(Played::Packet {
                    timestamp: ts,
                    payload,
                });
                self.next = Some(next + 1);
            } else if high >= next + self.depth as u64 || self.pending.len() > self.max_pending {
                out.push(Played::Lost);
                self.stats.lost += 1;
                self.next = Some(next + 1);
            } else {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(j: &mut JitterBuffer, seqs: &[u16]) -> Vec<Played> {
        let mut out = Vec::new();
        for &s in seqs {
            j.push(1, s, u32::from(s) * 160, &[s as u8], u32::from(s) * 160);
            out.extend(j.pop_ready());
        }
        out
    }

    fn seqs(p: &[Played]) -> Vec<Option<u8>> {
        p.iter()
            .map(|x| match x {
                Played::Packet { payload, .. } => payload.first().copied(),
                Played::Lost => None,
            })
            .collect()
    }

    #[test]
    fn in_order_passes_straight_through() {
        let mut j = JitterBuffer::new(3);
        assert_eq!(
            seqs(&feed(&mut j, &[1, 2, 3])),
            vec![Some(1), Some(2), Some(3)]
        );
    }

    #[test]
    fn reorders_within_depth_and_reports_loss() {
        let mut j = JitterBuffer::new(2);
        let out = feed(&mut j, &[1, 3, 2, 5, 6, 7]);
        assert_eq!(
            seqs(&out),
            vec![Some(1), Some(2), Some(3), None, Some(5), Some(6), Some(7)]
        );
        let s = j.stats();
        assert_eq!((s.received, s.lost, s.reordered), (6, 1, 1));
        // 4 arrives after its slot was declared lost.
        j.push(1, 4, 640, &[4], 640);
        assert_eq!(j.stats().late, 1);
        j.push(1, 7, 7 * 160, &[7], 0);
        assert_eq!(j.stats().late, 2);
    }

    #[test]
    fn wraps_sequence_numbers() {
        let mut j = JitterBuffer::new(2);
        let out = feed(&mut j, &[65534, 65535, 0, 1]);
        assert_eq!(out.len(), 4);
        assert_eq!(j.stats().lost, 0);
    }

    #[test]
    fn resyncs_on_new_ssrc_or_big_jump() {
        let mut j = JitterBuffer::new(2);
        feed(&mut j, &[10, 11]);
        j.push(2, 500, 0, &[0], 0);
        assert_eq!(j.stats().resyncs, 1);
        assert_eq!(j.pop_ready().len(), 1);
        j.push(2, 40_000, 0, &[0], 0);
        assert_eq!(
            j.stats().resyncs,
            2,
            "a huge jump restarts instead of mass loss"
        );
        assert_eq!(j.stats().lost, 0);
    }

    #[test]
    fn bounded_under_garbage() {
        let mut j = JitterBuffer::new(3);
        for s in (0..60_000u32).step_by(7) {
            j.push(9, (s * 13 % 65_536) as u16, s, &[0], s);
            let _ = j.pop_ready();
            assert!(j.pending.len() <= j.max_pending + 1);
        }
    }
}
