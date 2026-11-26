//! Bounded outage buffer.
//!
//! Events are sequenced and retained until the gateway acknowledges them.
//! If the buffer is full, the oldest unacknowledged event is dropped and
//! counted. Telemetry keeps only the latest snapshot per scanner.
//!
//! With a [`Journal`] the events also survive agent restarts (see
//! [`crate::journal`]). If the journal fails at runtime, the buffer keeps
//! working in memory and reports itself as no longer durable.

use std::collections::{HashMap, HashSet, VecDeque};

use platform_common::{ScannerEvent, ScannerId, TelemetrySnapshot};
use tracing::error;

use crate::journal::{Journal, JournalError, Recovered};

/// A sequenced event.
pub use crate::journal::Entry as Sequenced;

/// The buffer.
#[derive(Debug)]
pub struct Buffer {
    max_events: usize,
    next_seq: u64,
    events: VecDeque<Sequenced>,
    dropped: u64,
    telemetry: HashMap<ScannerId, TelemetrySnapshot>,
    dirty: HashSet<ScannerId>,
    journal: Option<Journal>,
    journal_failed: bool,
}

impl Buffer {
    /// Creates a buffer holding at most `max_events` unacknowledged events.
    #[must_use]
    pub fn new(max_events: usize) -> Self {
        Self {
            max_events: max_events.max(1),
            next_seq: 1,
            events: VecDeque::new(),
            dropped: 0,
            telemetry: HashMap::new(),
            dirty: HashSet::new(),
            journal: None,
            journal_failed: false,
        }
    }

    /// Opens a durable buffer backed by a journal in `dir`, recovering
    /// events that were unacknowledged when the agent last stopped. Returns
    /// the buffer and the persisted session id.
    ///
    /// # Errors
    /// The journal cannot be opened.
    pub fn open(
        max_events: usize,
        dir: &std::path::Path,
    ) -> Result<(Self, uuid::Uuid), JournalError> {
        let (
            journal,
            Recovered {
                session,
                events,
                next_seq,
                dropped,
                ..
            },
        ) = Journal::open(dir, max_events)?;
        let mut b = Self::new(max_events);
        b.events = events;
        b.next_seq = next_seq.max(1);
        b.dropped = dropped;
        b.journal = Some(journal);
        Ok((b, session))
    }

    /// True while events are being persisted.
    #[must_use]
    pub fn durable(&self) -> bool {
        self.journal.is_some()
    }

    /// True if a journal was configured but failed.
    #[must_use]
    pub fn journal_failed(&self) -> bool {
        self.journal_failed
    }

    fn journal_error(&mut self, e: &JournalError) {
        error!(error = %e, "event journal failed; continuing in memory only (events will not survive a restart)");
        self.journal = None;
        self.journal_failed = true;
    }

    /// Appends an event, returning its sequence number.
    pub fn push_event(&mut self, scanner_id: ScannerId, event: ScannerEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.events.len() >= self.max_events {
            self.events.pop_front();
            self.dropped += 1;
        }
        let entry = Sequenced {
            seq,
            scanner_id,
            event,
        };
        if let Some(j) = self.journal.as_mut()
            && let Err(e) = j.record_event(&entry)
        {
            self.journal_error(&e);
        }
        self.events.push_back(entry);
        seq
    }

    /// Records the latest telemetry for a scanner.
    pub fn put_telemetry(&mut self, scanner_id: ScannerId, snapshot: TelemetrySnapshot) {
        self.telemetry.insert(scanner_id, snapshot);
        self.dirty.insert(scanner_id);
    }

    /// Drops events up to and including `seq`.
    pub fn ack(&mut self, seq: u64) {
        while self.events.front().is_some_and(|e| e.seq <= seq) {
            self.events.pop_front();
        }
        if let Some(j) = self.journal.as_mut()
            && let Err(e) = j.record_ack(seq, self.events.iter())
        {
            self.journal_error(&e);
        }
    }

    /// Unacknowledged events with `seq > after`, in order.
    #[must_use]
    pub fn events_after(&self, after: u64) -> Vec<Sequenced> {
        self.events
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect()
    }

    /// Takes changed telemetry since the last call.
    pub fn take_dirty_telemetry(&mut self) -> Vec<(ScannerId, TelemetrySnapshot)> {
        let ids: Vec<_> = self.dirty.drain().collect();
        ids.into_iter()
            .filter_map(|id| self.telemetry.get(&id).cloned().map(|t| (id, t)))
            .collect()
    }

    /// Marks all telemetry as changed (after a reconnect).
    pub fn mark_all_dirty(&mut self) {
        self.dirty.extend(self.telemetry.keys().copied());
    }

    /// Number of unacknowledged events.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.events.len()
    }

    /// Events dropped because the buffer was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_common::ScannerHealth;

    fn ev() -> ScannerEvent {
        ScannerEvent::HealthChanged {
            health: ScannerHealth::Healthy,
        }
    }

    #[test]
    fn sequencing_ack_and_overflow() {
        let mut b = Buffer::new(3);
        let s = ScannerId::new();
        for _ in 0..5 {
            b.push_event(s, ev());
        }
        assert_eq!(b.dropped(), 2);
        assert_eq!(
            b.events_after(0).iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        b.ack(4);
        assert_eq!(b.pending(), 1);
        assert_eq!(b.events_after(0)[0].seq, 5);
        assert!(b.events_after(5).is_empty());
    }

    #[test]
    fn durable_buffer_recovers_after_restart() {
        let dir = std::env::temp_dir().join(format!("rp-buf-{}", uuid::Uuid::new_v4().simple()));
        let s = ScannerId::new();
        let (mut b, session) = Buffer::open(10, &dir).unwrap_or_else(|e| panic!("{e}"));
        assert!(b.durable());
        for _ in 0..4 {
            b.push_event(s, ev());
        }
        b.ack(2);
        drop(b);
        let (mut b, again) = Buffer::open(10, &dir).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(session, again);
        assert_eq!(
            b.events_after(0).iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(b.push_event(s, ev()), 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn telemetry_is_latest_only() {
        let mut b = Buffer::new(10);
        let s = ScannerId::new();
        let mut t = TelemetrySnapshot::empty(platform_common::now_utc());
        b.put_telemetry(s, t.clone());
        t.volume = Some(3);
        b.put_telemetry(s, t);
        let d = b.take_dirty_telemetry();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].1.volume, Some(3));
        assert!(b.take_dirty_telemetry().is_empty());
        b.mark_all_dirty();
        assert_eq!(b.take_dirty_telemetry().len(), 1);
    }
}
