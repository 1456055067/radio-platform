//! Per-scanner actor that owns the UDP socket.
//!
//! Exactly one command is in flight per scanner. The protocol has no request
//! identifiers, so replies are correlated by command name; stale datagrams
//! are drained before each send, and frames that do not answer the current
//! command are discarded (counted in [`Stats::stale_frames`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::{Instant, sleep, timeout_at};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};
use uniden_protocol::xml::ScannerInfo;
use uniden_protocol::{
    Command, Effect, Frame, FrameAssembler, Reply, ReplyFormat, ResponseError, answers,
    decode_reply,
};

use crate::config::ScannerConfig;
use crate::error::Error;
use crate::health::{Health, HealthTracker};

/// Monotonic counters for observability.
#[derive(Debug, Default)]
pub struct Stats {
    /// Datagrams sent.
    pub datagrams_sent: AtomicU64,
    /// Datagrams received from the scanner.
    pub datagrams_received: AtomicU64,
    /// Commands completed with a decoded reply.
    pub replies: AtomicU64,
    /// Exchanges that timed out (per attempt).
    pub timeouts: AtomicU64,
    /// Retries performed.
    pub retries: AtomicU64,
    /// Frames discarded as stale, duplicate or uncorrelated.
    pub stale_frames: AtomicU64,
    /// Datagrams or frames that failed to decode.
    pub decode_errors: AtomicU64,
    /// Push (`PSI`) frames delivered to subscribers.
    pub push_frames: AtomicU64,
}

/// Point-in-time copy of [`Stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(missing_docs)]
pub struct StatsSnapshot {
    pub datagrams_sent: u64,
    pub datagrams_received: u64,
    pub replies: u64,
    pub timeouts: u64,
    pub retries: u64,
    pub stale_frames: u64,
    pub decode_errors: u64,
    pub push_frames: u64,
}

impl Stats {
    /// Takes a snapshot.
    #[must_use]
    pub fn snapshot(&self) -> StatsSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        StatsSnapshot {
            datagrams_sent: l(&self.datagrams_sent),
            datagrams_received: l(&self.datagrams_received),
            replies: l(&self.replies),
            timeouts: l(&self.timeouts),
            retries: l(&self.retries),
            stale_frames: l(&self.stale_frames),
            decode_errors: l(&self.decode_errors),
            push_frames: l(&self.push_frames),
        }
    }
}

fn inc(a: &AtomicU64) {
    a.fetch_add(1, Ordering::Relaxed);
}

pub(crate) struct Request {
    pub(crate) command: Command,
    pub(crate) reply: oneshot::Sender<Result<Reply, Error>>,
}

pub(crate) struct Worker {
    pub(crate) config: ScannerConfig,
    pub(crate) socket: UdpSocket,
    pub(crate) requests: mpsc::Receiver<Request>,
    pub(crate) push: broadcast::Sender<Arc<ScannerInfo>>,
    pub(crate) health: watch::Sender<Health>,
    pub(crate) stats: Arc<Stats>,
    pub(crate) cancel: CancellationToken,
    pub(crate) assembler: FrameAssembler,
    pub(crate) buf: Vec<u8>,
    pub(crate) tracker: HealthTracker,
}

impl Worker {
    pub(crate) async fn run(mut self) {
        let mut last_activity = Instant::now();
        loop {
            let keepalive_at = self.config.keepalive_interval.map(|i| last_activity + i);
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => break,
                req = self.requests.recv() => {
                    let Some(req) = req else { break };
                    let cancel = self.cancel.clone();
                    let result = tokio::select! {
                        () = cancel.cancelled() => Err(Error::Closed),
                        r = self.execute(&req.command) => r,
                    };
                    // The caller may have gone away; that is not an error.
                    let _ = req.reply.send(result);
                    last_activity = Instant::now();
                }
                recv = self.socket.recv(&mut self.buf) => {
                    match recv {
                        Ok(n) => self.handle_idle_datagram(n),
                        Err(e) => {
                            warn!(error = %e, "receive error while idle");
                            sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
                () = sleep_until_opt(keepalive_at) => {
                    if let Err(e) = self.send(&Command::KeepAlive).await {
                        debug!(error = %e, "keepalive send failed");
                    }
                    last_activity = Instant::now();
                }
            }
        }
        // Fail any queued requests promptly.
        self.requests.close();
        while let Ok(req) = self.requests.try_recv() {
            let _ = req.reply.send(Err(Error::Closed));
        }
        debug!(scanner = %self.config.address, "scanner worker stopped");
    }

    fn handle_idle_datagram(&mut self, n: usize) {
        inc(&self.stats.datagrams_received);
        let frames = match self.assembler.push(self.buf.get(..n).unwrap_or_default()) {
            Ok(f) => f,
            Err(e) => {
                inc(&self.stats.decode_errors);
                debug!(error = %e, "discarding undecodable datagram");
                return;
            }
        };
        for frame in frames {
            if !self.try_publish_push(&frame) {
                inc(&self.stats.stale_frames);
                trace!(command = frame.command(), "discarding uncorrelated frame");
            }
        }
    }

    /// Publishes XML `ScannerInfo` frames that are not replies to an
    /// in-flight request. Returns true if the frame was consumed.
    fn try_publish_push(&self, frame: &Frame) -> bool {
        let Frame::Xml(x) = frame else { return false };
        if x.command != "PSI" && x.command != "GSI" {
            return false;
        }
        match decode_reply(
            &Command::GetScannerInfo,
            frame.clone(),
            &self.config.xml_limits,
        ) {
            Ok(Reply::ScannerInfo(info)) => {
                inc(&self.stats.push_frames);
                // No subscribers is not an error.
                let _ = self.push.send(Arc::from(info));
                true
            }
            _ => {
                inc(&self.stats.decode_errors);
                true
            }
        }
    }

    async fn send(&self, command: &Command) -> Result<(), Error> {
        let wire = command.encode()?;
        self.socket.send(&wire).await?;
        inc(&self.stats.datagrams_sent);
        Ok(())
    }

    /// Drains datagrams that arrived before this command was sent.
    fn drain_stale(&mut self) {
        while let Ok(n) = self.socket.try_recv(&mut self.buf) {
            self.handle_idle_datagram(n);
        }
        if !self.assembler.is_idle() {
            self.assembler.reset();
        }
    }

    async fn execute(&mut self, command: &Command) -> Result<Reply, Error> {
        let result = self.execute_inner(command).await;
        let reached = match &result {
            Ok(_) | Err(Error::Rejected { .. } | Error::Decode { .. }) => true,
            Err(Error::Timeout { .. } | Error::Io(_)) => false,
            Err(_) => return result,
        };
        let health = self.tracker.record(reached, self.config.unreachable_after);
        self.health.send_if_modified(|h| {
            let changed = *h != health;
            *h = health;
            changed
        });
        result
    }

    async fn execute_inner(&mut self, command: &Command) -> Result<Reply, Error> {
        let retryable = command.effect() == Effect::ReadOnly;
        let max_attempts = if retryable {
            self.config.retries.saturating_add(1)
        } else {
            1
        };
        let timeout = match command.reply_format() {
            ReplyFormat::Xml | ReplyFormat::Any => self.config.xml_timeout,
            ReplyFormat::Line | ReplyFormat::None => self.config.line_timeout,
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            self.drain_stale();
            self.send(command).await?;
            if command.reply_format() == ReplyFormat::None {
                return Ok(Reply::Ok);
            }
            match self.await_reply(command, Instant::now() + timeout).await {
                Ok(reply) => {
                    inc(&self.stats.replies);
                    return Ok(reply);
                }
                Err(e @ (Error::Rejected { .. } | Error::Io(_))) => return Err(e),
                Err(Error::Timeout {
                    command: name,
                    state_change_unknown,
                    ..
                }) if attempt >= max_attempts => {
                    return Err(Error::Timeout {
                        command: name,
                        attempts: attempt,
                        state_change_unknown,
                    });
                }
                Err(e) if attempt >= max_attempts => return Err(e),
                Err(e) => {
                    inc(&self.stats.retries);
                    let delay = self.config.backoff.delay(attempt);
                    debug!(command = command.name(), attempt, ?delay, error = %e, "retrying");
                    sleep(delay).await;
                }
            }
        }
    }

    /// Waits for a frame answering `command`. Frames that do not answer it,
    /// and answering frames that fail to decode (for example a delayed
    /// duplicate of an earlier reply), are discarded until the deadline.
    async fn await_reply(&mut self, command: &Command, deadline: Instant) -> Result<Reply, Error> {
        let mut last_decode: Option<uniden_protocol::DecodeError> = None;
        loop {
            let n = match timeout_at(deadline, self.socket.recv(&mut self.buf)).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(Error::Io(e)),
                Err(_) => {
                    inc(&self.stats.timeouts);
                    self.assembler.reset();
                    if let Some(error) = last_decode {
                        return Err(Error::Decode {
                            command: command.name().to_owned(),
                            error,
                        });
                    }
                    return Err(Error::Timeout {
                        command: command.name().to_owned(),
                        attempts: 1,
                        state_change_unknown: command.effect() == Effect::StateChanging,
                    });
                }
            };
            inc(&self.stats.datagrams_received);
            let frames = match self.assembler.push(self.buf.get(..n).unwrap_or_default()) {
                Ok(f) => f,
                Err(e) => {
                    inc(&self.stats.decode_errors);
                    debug!(error = %e, "undecodable datagram while awaiting reply");
                    last_decode = Some(e);
                    continue;
                }
            };
            for frame in frames {
                let is_answer = answers(&frame, command)
                    && !(matches!(command, Command::PushScannerInfo { .. })
                        && matches!(frame, Frame::Xml(_)));
                if !is_answer {
                    if !self.try_publish_push(&frame) {
                        inc(&self.stats.stale_frames);
                    }
                    continue;
                }
                match decode_reply(command, frame, &self.config.xml_limits) {
                    Ok(reply) => return Ok(reply),
                    Err(ResponseError::Rejected(error)) => {
                        return Err(Error::Rejected {
                            command: command.name().to_owned(),
                            error,
                        });
                    }
                    Err(ResponseError::Decode(e)) => {
                        inc(&self.stats.decode_errors);
                        debug!(error = %e, "discarding undecodable reply frame");
                        last_decode = Some(e);
                    }
                }
            }
        }
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
