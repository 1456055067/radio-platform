//! Transmission detection.
//!
//! An SDS200 is a single receiver, so at most one transmission is active at
//! a time.
//!
//! **Heuristic (requires hardware verification).** Reception is inferred
//! from `Property@Mute`: `Unmute` means the squelch is open and audio is
//! passing. A transmission ends when the scanner has stayed muted for
//! `hang_time`, or immediately when the channel identity (frequency, TGID,
//! channel or system) changes. If the scanner does not report `Mute`, no
//! transmissions are inferred.

use std::time::Duration;

use platform_common::{
    ScannerEvent, ScannerId, SiteId, TelemetrySnapshot, TransmissionId, TransmissionStart,
};
use time::OffsetDateTime;

/// Detector configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectorConfig {
    /// Continuous muted time after which a transmission is ended.
    pub hang_time: Duration,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            hang_time: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    frequency_hz: Option<u64>,
    tgid: Option<String>,
    channel: Option<String>,
    system: Option<String>,
}

impl Identity {
    fn of(s: &TelemetrySnapshot) -> Self {
        Self {
            frequency_hz: s.frequency_hz,
            tgid: s.tgid.clone(),
            channel: s.channel.clone(),
            system: s.system.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct Active {
    id: TransmissionId,
    started_at: OffsetDateTime,
    last_active_at: OffsetDateTime,
    identity: Identity,
}

/// Stateful transmission detector for one scanner.
#[derive(Debug)]
pub struct TransmissionDetector {
    config: DetectorConfig,
    scanner_id: ScannerId,
    site_id: Option<SiteId>,
    active: Option<Active>,
}

fn duration_ms(from: OffsetDateTime, to: OffsetDateTime) -> u64 {
    u64::try_from((to - from).whole_milliseconds()).unwrap_or(0)
}

impl TransmissionDetector {
    /// Creates a detector.
    #[must_use]
    pub fn new(scanner_id: ScannerId, site_id: Option<SiteId>, config: DetectorConfig) -> Self {
        Self {
            config,
            scanner_id,
            site_id,
            active: None,
        }
    }

    /// The active transmission, if any.
    #[must_use]
    pub fn active(&self) -> Option<TransmissionId> {
        self.active.as_ref().map(|a| a.id)
    }

    fn end(&mut self, at: OffsetDateTime) -> Option<ScannerEvent> {
        self.active.take().map(|a| {
            let ended_at = at.max(a.started_at);
            ScannerEvent::TransmissionEnded {
                transmission_id: a.id,
                ended_at,
                duration_ms: duration_ms(a.started_at, ended_at),
            }
        })
    }

    fn start(&mut self, s: &TelemetrySnapshot) -> ScannerEvent {
        let id = TransmissionId::new();
        self.active = Some(Active {
            id,
            started_at: s.observed_at,
            last_active_at: s.observed_at,
            identity: Identity::of(s),
        });
        ScannerEvent::TransmissionStarted(TransmissionStart {
            transmission_id: id,
            scanner_id: self.scanner_id,
            site_id: self.site_id,
            started_at: s.observed_at,
            frequency_hz: s.frequency_hz,
            tgid: s.tgid.clone(),
            unit_id: s.unit_id.clone(),
            system: s.system.clone(),
            department: s.department.clone(),
            channel: s.channel.clone(),
            modulation: s.modulation.clone(),
            signal_bars: s.signal_bars,
            rssi: s.rssi,
        })
    }

    /// Processes a snapshot and returns resulting events, in order.
    pub fn observe(&mut self, s: &TelemetrySnapshot) -> Vec<ScannerEvent> {
        let mut events = Vec::new();
        match s.muted {
            Some(false) => {
                let identity = Identity::of(s);
                match &mut self.active {
                    Some(a) if a.identity == identity => {
                        a.last_active_at = a.last_active_at.max(s.observed_at);
                    }
                    Some(_) => {
                        events.extend(self.end(s.observed_at));
                        events.push(self.start(s));
                    }
                    None => events.push(self.start(s)),
                }
            }
            Some(true) => {
                if let Some(a) = &self.active {
                    let quiet = duration_ms(a.last_active_at, s.observed_at);
                    if u128::from(quiet) >= self.config.hang_time.as_millis() {
                        let end_at = a.last_active_at;
                        events.extend(self.end(end_at));
                    }
                }
            }
            None => {}
        }
        events
    }

    /// Ends any active transmission (scanner went offline, agent stopping)
    /// at its last observed activity.
    pub fn flush(&mut self) -> Option<ScannerEvent> {
        let at = self.active.as_ref()?.last_active_at;
        self.end(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn snap(t_ms: i64, muted: Option<bool>, freq: u64) -> TelemetrySnapshot {
        let mut s = TelemetrySnapshot::empty(
            datetime!(2026-11-24 12:00 UTC) + time::Duration::milliseconds(t_ms),
        );
        s.muted = muted;
        s.frequency_hz = Some(freq);
        s.channel = Some(format!("ch{freq}"));
        s
    }

    fn det() -> TransmissionDetector {
        TransmissionDetector::new(ScannerId::new(), None, DetectorConfig::default())
    }

    #[test]
    fn start_continue_hang_end() {
        let mut d = det();
        assert!(d.observe(&snap(0, Some(true), 1)).is_empty());
        let ev = d.observe(&snap(100, Some(false), 1));
        assert!(matches!(
            ev.as_slice(),
            [ScannerEvent::TransmissionStarted(_)]
        ));
        assert!(d.observe(&snap(600, Some(false), 1)).is_empty());
        assert!(
            d.observe(&snap(1500, Some(true), 1)).is_empty(),
            "within hang time"
        );
        let ev = d.observe(&snap(2700, Some(true), 1));
        let [ScannerEvent::TransmissionEnded { duration_ms, .. }] = ev.as_slice() else {
            panic!("{ev:?}")
        };
        // Ends at the last active observation, not at hang expiry.
        assert_eq!(*duration_ms, 500);
        assert!(d.active().is_none());
    }

    #[test]
    fn channel_change_splits_transmissions() {
        let mut d = det();
        d.observe(&snap(0, Some(false), 1));
        let first = d.active().unwrap();
        let ev = d.observe(&snap(300, Some(false), 2));
        assert!(matches!(
            ev.as_slice(),
            [ScannerEvent::TransmissionEnded { transmission_id, duration_ms: 300, .. }, ScannerEvent::TransmissionStarted(_)]
                if *transmission_id == first
        ));
        assert_ne!(d.active().unwrap(), first);
    }

    #[test]
    fn resumed_activity_within_hang_continues() {
        let mut d = det();
        d.observe(&snap(0, Some(false), 1));
        let id = d.active();
        d.observe(&snap(1000, Some(true), 1));
        assert!(d.observe(&snap(1500, Some(false), 1)).is_empty());
        assert_eq!(d.active(), id);
    }

    #[test]
    fn unknown_mute_state_infers_nothing() {
        let mut d = det();
        assert!(d.observe(&snap(0, None, 1)).is_empty());
        assert!(d.active().is_none());
    }

    #[test]
    fn single_receiver_invariant() {
        let mut d = det();
        for i in 0..50u64 {
            d.observe(&snap(i64::try_from(i).unwrap() * 100, Some(false), i % 3));
            // never more than one active transmission by construction
            assert!(d.active().is_some());
        }
    }

    #[test]
    fn flush_ends_active() {
        let mut d = det();
        d.observe(&snap(0, Some(false), 1));
        d.observe(&snap(400, Some(false), 1));
        let ev = d.flush().unwrap();
        assert!(matches!(
            ev,
            ScannerEvent::TransmissionEnded {
                duration_ms: 400,
                ..
            }
        ));
        assert!(d.flush().is_none());
    }
}
