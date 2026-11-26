//! Per-scanner audio fan-out and listener demand.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use platform_common::wire::AudioState;
use platform_common::wire::media::MediaFrame;
use platform_common::{AgentId, ScannerId};
use tokio::sync::{Notify, broadcast, mpsc};

/// Frames buffered per scanner for slow listeners (about 1.3 s).
const STREAM_BUFFER: usize = 64;

/// A change in live-listener demand for a scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demand {
    /// Agent that streams the scanner.
    pub agent: AgentId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// At least one listener.
    pub listen: bool,
}

#[derive(Debug)]
struct Stream {
    tx: broadcast::Sender<Arc<MediaFrame>>,
    listeners: usize,
    agent: Option<AgentId>,
    state: Option<AudioState>,
    last_frame: Option<Instant>,
}

impl Stream {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(STREAM_BUFFER);
        Self {
            tx,
            listeners: 0,
            agent: None,
            state: None,
            last_frame: None,
        }
    }
}

#[derive(Debug)]
struct Inner {
    streams: Mutex<HashMap<ScannerId, Stream>>,
    demand: mpsc::UnboundedSender<Demand>,
    tap: Mutex<Option<mpsc::Sender<Arc<MediaFrame>>>>,
    notify: Notify,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Audio streams on this gateway replica.
#[derive(Debug, Clone)]
pub struct MediaHub {
    inner: Arc<Inner>,
}

/// A listener's subscription. Dropping it releases the demand.
#[derive(Debug)]
pub struct Subscription {
    hub: MediaHub,
    scanner_id: ScannerId,
    /// Frames for this listener.
    pub rx: broadcast::Receiver<Arc<MediaFrame>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.release(self.scanner_id);
    }
}

/// Per-scanner status for dashboards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStatus {
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Live listeners on this replica.
    pub listeners: usize,
    /// Agent-reported pipeline state.
    pub state: Option<AudioState>,
    /// Frames arrived within the last 2 s.
    pub receiving: bool,
}

impl MediaHub {
    /// Creates a hub; demand changes are delivered on the returned channel
    /// (the gateway forwards them to the agent).
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Demand>) {
        let (demand, rx) = mpsc::unbounded_channel();
        (
            Self {
                inner: Arc::new(Inner {
                    streams: Mutex::new(HashMap::new()),
                    demand,
                    tap: Mutex::new(None),
                    notify: Notify::new(),
                }),
            },
            rx,
        )
    }

    /// Sends every frame also to `tap` (the recorder). Frames are dropped,
    /// never blocked on, if it lags.
    pub fn set_tap(&self, tap: mpsc::Sender<Arc<MediaFrame>>) {
        *lock(&self.inner.tap) = Some(tap);
    }

    /// Publishes a frame from an agent.
    pub fn publish(&self, frame: MediaFrame) {
        let frame = Arc::new(frame);
        {
            let mut s = lock(&self.inner.streams);
            let st = s.entry(frame.scanner_id).or_insert_with(Stream::new);
            st.last_frame = Some(Instant::now());
            let _ = st.tx.send(Arc::clone(&frame));
        }
        if let Some(t) = lock(&self.inner.tap).as_ref() {
            let _ = t.try_send(frame);
        }
        self.inner.notify.notify_waiters();
    }

    /// Wakes when any frame is published.
    pub async fn published(&self) {
        self.inner.notify.notified().await;
    }

    /// Adds a listener. The first listener raises demand for `agent`.
    #[must_use]
    pub fn subscribe(&self, scanner_id: ScannerId, agent: &AgentId) -> Subscription {
        let (rx, first) = {
            let mut s = lock(&self.inner.streams);
            let st = s.entry(scanner_id).or_insert_with(Stream::new);
            st.listeners += 1;
            st.agent = Some(agent.clone());
            (st.tx.subscribe(), st.listeners == 1)
        };
        if first {
            let _ = self.inner.demand.send(Demand {
                agent: agent.clone(),
                scanner_id,
                listen: true,
            });
        }
        Subscription {
            hub: self.clone(),
            scanner_id,
            rx,
        }
    }

    fn release(&self, scanner_id: ScannerId) {
        let last = {
            let mut s = lock(&self.inner.streams);
            s.get_mut(&scanner_id).and_then(|st| {
                st.listeners = st.listeners.saturating_sub(1);
                (st.listeners == 0).then(|| st.agent.clone()).flatten()
            })
        };
        if let Some(agent) = last {
            let _ = self.inner.demand.send(Demand {
                agent,
                scanner_id,
                listen: false,
            });
        }
    }

    /// Scanners of `agent` that currently have listeners (re-sent to an
    /// agent after it reconnects).
    #[must_use]
    pub fn listened(&self, agent: &AgentId) -> Vec<ScannerId> {
        lock(&self.inner.streams)
            .iter()
            .filter(|(_, s)| s.listeners > 0 && s.agent.as_ref() == Some(agent))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Records the agent-reported pipeline state.
    pub fn set_state(&self, scanner_id: ScannerId, state: AudioState) {
        lock(&self.inner.streams)
            .entry(scanner_id)
            .or_insert_with(Stream::new)
            .state = Some(state);
    }

    /// Status of one scanner's stream.
    #[must_use]
    pub fn status(&self, scanner_id: ScannerId) -> StreamStatus {
        let s = lock(&self.inner.streams);
        let st = s.get(&scanner_id);
        StreamStatus {
            scanner_id,
            listeners: st.map_or(0, |s| s.listeners),
            state: st.and_then(|s| s.state.clone()),
            receiving: st
                .and_then(|s| s.last_frame)
                .is_some_and(|t| t.elapsed().as_secs() < 2),
        }
    }

    /// Totals: (scanners receiving audio, live listeners).
    #[must_use]
    pub fn totals(&self) -> (usize, usize) {
        let s = lock(&self.inner.streams);
        (
            s.values()
                .filter(|st| st.last_frame.is_some_and(|t| t.elapsed().as_secs() < 2))
                .count(),
            s.values().map(|st| st.listeners).sum(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn demand_follows_first_and_last_listener() {
        let (hub, mut demand) = MediaHub::new();
        let agent = AgentId::new("a").unwrap_or_else(|e| panic!("{e}"));
        let id = ScannerId::new();
        let a = hub.subscribe(id, &agent);
        let mut b = hub.subscribe(id, &agent);
        assert_eq!(demand.recv().await.map(|d| d.listen), Some(true));
        assert!(
            demand.try_recv().is_err(),
            "second listener raises no new demand"
        );
        assert_eq!(hub.listened(&agent), vec![id]);
        hub.publish(MediaFrame {
            scanner_id: id,
            captured_ms: 1,
            seq: 1,
            concealed: false,
            opus: vec![1],
        });
        assert_eq!(b.rx.recv().await.map(|f| f.seq).ok(), Some(1));
        drop(a);
        assert!(demand.try_recv().is_err());
        drop(b);
        assert_eq!(demand.recv().await.map(|d| d.listen), Some(false));
        assert!(hub.listened(&agent).is_empty());
        assert_eq!(hub.status(id).listeners, 0);
        assert!(hub.status(id).receiving);
    }
}
