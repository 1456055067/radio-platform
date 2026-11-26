//! Message bus between gateway replicas.
//!
//! Payloads are opaque strings (the gateway sends JSON envelopes). Every
//! subscriber, including the publisher's own, receives every message.
//! Delivery is best-effort and at most once: messages published while a
//! replica's listener is reconnecting are lost. Callers must tolerate that
//! (presence is re-announced periodically; commands time out).
//!
//! - [`MemoryBus`]: in-process, for tests and single-process deployments.
//! - [`PgBus`]: PostgreSQL `LISTEN`/`NOTIFY` on the gateway's own database.
//!   Payloads over the NOTIFY limit are stored in `cluster_bus_spill` and
//!   only the row id is notified.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::store::{StoreError, StoreResult};

/// Buffered messages per subscriber before it starts lagging.
const SUBSCRIBER_BUFFER: usize = 4096;
/// NOTIFY payloads must stay under 8000 bytes.
const INLINE_LIMIT: usize = 7000;
const CHANNEL: &str = "radio_platform_bus";
const SPILL_PREFIX: &str = "spill:";

/// A replica-to-replica message bus.
#[async_trait]
pub trait MessageBus: Send + Sync + std::fmt::Debug {
    /// Publishes to all replicas (including this one).
    async fn publish(&self, payload: &str) -> StoreResult<()>;
    /// Subscribes to all messages.
    fn subscribe(&self) -> broadcast::Receiver<Arc<str>>;
}

/// In-process bus. Clones share one channel, so two gateway states in one
/// process behave like two replicas.
#[derive(Debug, Clone)]
pub struct MemoryBus {
    tx: broadcast::Sender<Arc<str>>,
}

impl Default for MemoryBus {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBus {
    /// Creates a bus.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(SUBSCRIBER_BUFFER);
        Self { tx }
    }
}

#[async_trait]
impl MessageBus for MemoryBus {
    async fn publish(&self, payload: &str) -> StoreResult<()> {
        let _ = self.tx.send(Arc::from(payload));
        Ok(())
    }
    fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }
}

/// PostgreSQL `LISTEN`/`NOTIFY` bus.
#[derive(Debug, Clone)]
pub struct PgBus {
    pool: PgPool,
    tx: broadcast::Sender<Arc<str>>,
}

fn backend(e: &sqlx::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

impl PgBus {
    /// Starts listening. The listener task runs until the pool is closed
    /// and reconnects automatically after connection loss.
    ///
    /// # Errors
    /// The initial `LISTEN` fails.
    pub async fn start(pool: PgPool) -> StoreResult<Self> {
        let mut listener = PgListener::connect_with(&pool)
            .await
            .map_err(|e| backend(&e))?;
        listener.listen(CHANNEL).await.map_err(|e| backend(&e))?;
        let (tx, _) = broadcast::channel(SUBSCRIBER_BUFFER);
        let bus = Self { pool, tx };
        tokio::spawn(bus.clone().listen(listener));
        Ok(bus)
    }

    async fn listen(self, mut listener: PgListener) {
        loop {
            match listener.try_recv().await {
                Ok(Some(n)) => {
                    let p = n.payload();
                    let payload: Option<Arc<str>> = match p.strip_prefix(SPILL_PREFIX) {
                        Some(id) => self.fetch_spill(id).await,
                        None => Some(Arc::from(p)),
                    };
                    if let Some(payload) = payload {
                        let _ = self.tx.send(payload);
                    }
                }
                Ok(None) => {
                    warn!("cluster bus connection lost; reconnecting (messages may be missed)")
                }
                Err(e) => {
                    if self.pool.is_closed() {
                        debug!("cluster bus stopping: pool closed");
                        return;
                    }
                    warn!(error = %e, "cluster bus receive failed; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    async fn fetch_spill(&self, id: &str) -> Option<Arc<str>> {
        let id: i64 = id.parse().ok()?;
        match sqlx::query_as::<_, (String,)>("SELECT payload FROM cluster_bus_spill WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
        {
            Ok(Some((p,))) => Some(Arc::from(p)),
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, "cluster bus spill fetch failed");
                None
            }
        }
    }

    /// Deletes spilled payloads older than `age`.
    ///
    /// # Errors
    /// Database errors.
    pub async fn purge_spill(&self, age: Duration) -> StoreResult<u64> {
        let secs = f64::from(u32::try_from(age.as_secs()).unwrap_or(u32::MAX));
        let r = sqlx::query(
            "DELETE FROM cluster_bus_spill WHERE created_at < now() - make_interval(secs => $1)",
        )
        .bind(secs)
        .execute(&self.pool)
        .await
        .map_err(|e| backend(&e))?;
        Ok(r.rows_affected())
    }
}

#[async_trait]
impl MessageBus for PgBus {
    async fn publish(&self, payload: &str) -> StoreResult<()> {
        let note = if payload.len() <= INLINE_LIMIT {
            payload.to_owned()
        } else {
            let (id,): (i64,) =
                sqlx::query_as("INSERT INTO cluster_bus_spill (payload) VALUES ($1) RETURNING id")
                    .bind(payload)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| backend(&e))?;
            format!("{SPILL_PREFIX}{id}")
        };
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(note)
            .execute(&self.pool)
            .await
            .map_err(|e| backend(&e))?;
        Ok(())
    }
    fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }
}
