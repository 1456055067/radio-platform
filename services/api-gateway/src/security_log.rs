//! Security event log: authentication failures and rate-limit refusals as
//! structured `tracing` events on the dedicated target [`TARGET`],
//! throttled per source.
//!
//! The security log is separate from the per-tenant audit chain (ADR 0011):
//! a rejected request has no verified tenant, and unauthenticated traffic
//! must never drive database writes or choose a tenant's audit log.
//!
//! Each rejection site builds a borrowed [`SecurityEvent`] and calls
//! [`SecurityLog::record`]. The first [`SecurityLogLimits::per_source`]
//! events per source per window are written individually (within a
//! replica-wide budget); the rest are counted and reported as one
//! `event=suppressed` summary per source, either inline when the source
//! sends again after its window or from [`SecurityLog::flush`] (the gateway's
//! maintenance tick) when it went quiet.
//!
//! Events that carry a verified identity (`subject` or `agent`) draw on their
//! own small replica-wide budget ([`SecurityLogLimits::verified_global`]), so a
//! flood of unauthenticated rejections from many sources cannot hide them.
//! They still count against their source's per-source limit.
//!
//! [`subscriber`] composes the gateway's process-wide subscriber: the general
//! log under `RUST_LOG` and the security log, which `RUST_LOG` cannot silence.
//!
//! Events never carry the token or any part of it, claims of an unverified
//! token, headers, the raw path or the query string: [`SecurityEvent`] has no
//! field that could hold them, and [`layer`] writes no span context (the HTTP
//! request span carries the raw URI).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::http::Method;
use platform_auth::AuthError;
use platform_common::{AgentId, TenantId};
use tracing::field::display;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{FilterExt as _, filter_fn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;

/// The `tracing` target of every security event.
pub const TARGET: &str = "security";

/// Fixed window per source.
const WINDOW: Duration = Duration::from_secs(60);
/// Individual events per source per window.
const PER_SOURCE: u32 = 10;
/// Individual events per replica per window, all sources together.
const GLOBAL: u64 = 300;
/// Individual events with a verified identity per replica per window (a
/// separate budget that unauthenticated rejections cannot consume).
const VERIFIED_GLOBAL: u64 = 60;
/// Number of lock shards.
const SHARDS: usize = 16;
/// Tracked sources per shard (`SHARDS * SHARD_CAPACITY` in total).
const SHARD_CAPACITY: usize = 256;
/// `scope=source` summaries per flush; the rest roll into one `scope=global`.
const SUMMARY_CAP: usize = 128;

/// The listener a rejection happened on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    /// Public REST/WebSocket API.
    Public,
    /// mTLS agent listener.
    Agent,
}

impl Listener {
    /// The stable `listener` field value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Agent => "agent",
        }
    }
}

/// Why a request was rejected (the stable `reason` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No bearer token.
    Missing,
    /// Token could not be parsed.
    Malformed,
    /// Token failed verification (signature, algorithm, key, issuer, audience).
    Invalid,
    /// Token or session expired.
    Expired,
    /// A required claim is missing or invalid (see the `claim` field).
    Claim,
    /// Per-client-IP rate limit (before authentication).
    RateLimitedIp,
    /// Per-subject rate limit (after authentication).
    RateLimitedPrincipal,
    /// CA-verified agent certificate without a usable identity.
    AgentIdentity,
    /// CA-verified agent certificate for an agent that is not registered.
    AgentUnregistered,
}

impl Reason {
    /// Every reason, in metric rendering order.
    pub const ALL: [Self; 9] = [
        Self::Missing,
        Self::Malformed,
        Self::Invalid,
        Self::Expired,
        Self::Claim,
        Self::RateLimitedIp,
        Self::RateLimitedPrincipal,
        Self::AgentIdentity,
        Self::AgentUnregistered,
    ];

    /// The stable `reason` field value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Claim => "claim",
            Self::RateLimitedIp => "rate_limited_ip",
            Self::RateLimitedPrincipal => "rate_limited_principal",
            Self::AgentIdentity => "agent_identity",
            Self::AgentUnregistered => "agent_unregistered",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

    const fn is_rate_limit(self) -> bool {
        matches!(self, Self::RateLimitedIp | Self::RateLimitedPrincipal)
    }

    /// Maps a token verification error to a reason and, for
    /// [`AuthError::Claim`], the claim name. `Provider` and `Config` are
    /// identity-provider or configuration failures, not client failures, and
    /// map to `None` (no security event).
    ///
    /// The claim name is a standard name or a configured claim name; it never
    /// contains token content.
    #[must_use]
    pub fn from_auth_error(e: &AuthError) -> Option<(Self, Option<&str>)> {
        match e {
            AuthError::Malformed => Some((Self::Malformed, None)),
            AuthError::Invalid => Some((Self::Invalid, None)),
            AuthError::Expired => Some((Self::Expired, None)),
            AuthError::Claim(name) => Some((Self::Claim, Some(name.as_str()))),
            AuthError::Provider | AuthError::Config(_) => None,
        }
    }
}

/// One rejected request. Every field is either an enumerated value, an
/// already-parsed address, a route template or a **verified** identity.
#[derive(Debug, Clone, Copy)]
pub struct SecurityEvent<'a> {
    /// Why.
    pub reason: Reason,
    /// Claim name, only with [`Reason::Claim`].
    pub claim: Option<&'a str>,
    /// HTTP status returned (401, 403 or 429).
    pub status: u16,
    /// Client address (see `state::client_ip`) or the TCP peer.
    pub ip: Option<IpAddr>,
    /// HTTP method (written as a standard name or `OTHER`).
    pub method: &'a Method,
    /// Route template, `unmatched`, or the agent path. Never the raw path.
    pub route: &'a str,
    /// Listener.
    pub listener: Listener,
    /// Subject of a **verified** principal only.
    pub subject: Option<&'a str>,
    /// Tenant of a **verified** principal only.
    pub tenant: Option<TenantId>,
    /// Agent id from a CA-verified client certificate only.
    pub agent: Option<&'a AgentId>,
}

/// Throttle limits. [`Default`] holds the production constants.
#[derive(Debug, Clone, Copy)]
pub struct SecurityLogLimits {
    /// Fixed window per source and for the replica-wide budget.
    pub window: Duration,
    /// Individual events per source per window.
    pub per_source: u32,
    /// Individual lines (events without a verified identity and inline
    /// summaries) per replica per window.
    pub global: u64,
    /// Individual events with a verified identity (`subject` or `agent`) per
    /// replica per window.
    pub verified_global: u64,
    /// Number of lock shards (at least 1).
    pub shards: usize,
    /// Tracked sources per shard (at least 1).
    pub shard_capacity: usize,
    /// `scope=source` summaries per flush.
    pub summary_cap: usize,
}

impl Default for SecurityLogLimits {
    fn default() -> Self {
        Self {
            window: WINDOW,
            per_source: PER_SOURCE,
            global: GLOBAL,
            verified_global: VERIFIED_GLOBAL,
            shards: SHARDS,
            shard_capacity: SHARD_CAPACITY,
            summary_cap: SUMMARY_CAP,
        }
    }
}

/// Throttle key: IPv4 address, IPv6 `/64` prefix, or unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    V4(Ipv4Addr),
    V6Prefix(u64),
    Unknown,
}

impl Key {
    fn of(ip: Option<IpAddr>) -> Self {
        match ip.map(|ip| ip.to_canonical()) {
            Some(IpAddr::V4(a)) => Self::V4(a),
            Some(IpAddr::V6(a)) => {
                let s = a.segments();
                Self::V6Prefix(
                    s.iter()
                        .take(4)
                        .fold(0u64, |acc, seg| (acc << 16) | u64::from(*seg)),
                )
            }
            None => Self::Unknown,
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V4(a) => write!(f, "{a}"),
            Self::V6Prefix(p) => write!(f, "{}/64", Ipv6Addr::from(u128::from(*p) << 64)),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    window_start: Instant,
    emitted: u32,
    suppressed: u64,
}

/// One lock shard of the source table.
#[derive(Debug)]
struct Shard {
    map: HashMap<Key, Slot>,
    /// Lower bound on the first instant at which a slot can be evicted (its
    /// window ended and it owes no summary). `None`: no slot can become
    /// evictable until a slot is inserted or its window restarts. A full
    /// shard skips the eviction scan until then.
    next_evict: Option<Instant>,
}

/// Lowers `next` to `at`; `None` (overflow: never expires) leaves it as is.
fn lower(next: &mut Option<Instant>, at: Option<Instant>) {
    if let Some(at) = at {
        *next = Some(next.map_or(at, |n| n.min(at)));
    }
}

/// One replica-wide line budget per window (lock-free; approximate at the
/// window edge).
#[derive(Debug, Default)]
struct Budget {
    window: AtomicU64,
    count: AtomicU64,
}

impl Budget {
    fn admit(&self, idx: u64, limit: u64) -> bool {
        let cur = self.window.load(Ordering::Acquire);
        if idx > cur
            && self
                .window
                .compare_exchange(cur, idx, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.count.store(0, Ordering::Release);
        }
        self.count.fetch_add(1, Ordering::AcqRel) < limit
    }
}

/// What [`SecurityLog::decide`] concluded for one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Decision {
    /// Write a `scope=source` summary with this count first.
    summary: Option<u64>,
    /// Write the event itself.
    emit: bool,
}

/// Summaries due at a flush.
#[derive(Debug, Default)]
struct Due {
    sources: Vec<(Key, u64)>,
    /// Suppressed count of sources beyond the summary cap plus untracked ones.
    global: u64,
}

/// Counters behind the security metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityLogStats {
    events: [u64; Reason::ALL.len()],
    /// Events not written individually (per source and untracked).
    pub suppressed: u64,
    /// Events whose source could not be tracked because the table was full.
    pub untracked: u64,
    /// Sources currently tracked.
    pub tracked_sources: usize,
}

impl SecurityLogStats {
    /// Rejections with `reason`, counted before throttling.
    #[must_use]
    pub fn events(&self, reason: Reason) -> u64 {
        self.events.get(reason.index()).copied().unwrap_or(0)
    }
}

/// The throttled security event emitter. Shared by both listeners through
/// [`crate::AppState`].
// NIST 800-53 Rev5: SC-5 (fixed-size per-source table and replica-wide budgets bound log volume and memory under a rejection flood; the bounded volume lets the operator size log storage)
// POAM: the throttle is per replica; with R replicas a source can produce up to R x per_source lines per window (SIEM aggregates across hosts).
// POAM: verified-identity events share their source's per-source slot, so unauthenticated failures from the same address (e.g. behind one NAT) can suppress them; they are still counted and summarised.
// POAM: the verified-identity budget is replica-wide, not per principal; one verified principal failing from several addresses can spend it, so other verified events that window are only counted and summarised (SEC-005).
#[derive(Debug)]
pub struct SecurityLog {
    limits: SecurityLogLimits,
    hasher: RandomState,
    shards: Vec<Mutex<Shard>>,
    tracked: AtomicUsize,
    epoch: Instant,
    global: Budget,
    verified: Budget,
    untracked_pending: AtomicU64,
    untracked_total: AtomicU64,
    suppressed_total: AtomicU64,
    events: [AtomicU64; Reason::ALL.len()],
    /// Eviction scans run (tests check that a full shard skips them).
    #[cfg(test)]
    evict_scans: AtomicU64,
}

impl Default for SecurityLog {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityLog {
    /// A security log with the production limits.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(SecurityLogLimits::default())
    }

    /// A security log with custom limits (tests).
    #[must_use]
    pub fn with_limits(limits: SecurityLogLimits) -> Self {
        let limits = SecurityLogLimits {
            shards: limits.shards.max(1),
            shard_capacity: limits.shard_capacity.max(1),
            window: limits.window.max(Duration::from_millis(1)),
            ..limits
        };
        Self {
            hasher: RandomState::new(),
            shards: (0..limits.shards)
                .map(|_| {
                    Mutex::new(Shard {
                        map: HashMap::with_capacity(limits.shard_capacity),
                        next_evict: None,
                    })
                })
                .collect(),
            limits,
            tracked: AtomicUsize::new(0),
            epoch: Instant::now(),
            global: Budget::default(),
            verified: Budget::default(),
            untracked_pending: AtomicU64::new(0),
            untracked_total: AtomicU64::new(0),
            suppressed_total: AtomicU64::new(0),
            events: std::array::from_fn(|_| AtomicU64::new(0)),
            #[cfg(test)]
            evict_scans: AtomicU64::new(0),
        }
    }

    /// Records one rejection now.
    pub fn record(&self, ev: &SecurityEvent<'_>) {
        self.record_at(ev, Instant::now());
    }

    /// Records one rejection at `now`: counts it, decides under a short shard
    /// lock whether to write it, and writes after the lock is released.
    pub fn record_at(&self, ev: &SecurityEvent<'_>, now: Instant) {
        if let Some(c) = self.events.get(ev.reason.index()) {
            c.fetch_add(1, Ordering::Relaxed);
        }
        let key = Key::of(ev.ip);
        let verified = ev.subject.is_some() || ev.agent.is_some();
        let d = self.decide(key, now, verified);
        if let Some(n) = d.summary {
            self.write_source_summary(key, n);
        }
        if d.emit {
            write_event(ev);
        }
    }

    /// Writes the summaries of sources whose window ended and removes their
    /// slots, so the table drains within one window after a flood stops.
    pub fn flush(&self) {
        self.flush_at(Instant::now());
    }

    /// [`Self::flush`] at `now`.
    pub fn flush_at(&self, now: Instant) {
        let due = self.collect_due(now);
        for (key, n) in &due.sources {
            self.write_source_summary(*key, *n);
        }
        if due.global > 0 {
            tracing::warn!(
                target: TARGET,
                event = "suppressed",
                scope = "global",
                suppressed = due.global,
                window_secs = self.limits.window.as_secs(),
                "security events suppressed"
            );
        }
    }

    /// Sources currently tracked (at most `shards * shard_capacity`).
    #[must_use]
    pub fn tracked_sources(&self) -> usize {
        self.tracked.load(Ordering::Relaxed)
    }

    /// Counters for the metrics endpoint (atomics only, no lock).
    #[must_use]
    pub fn stats(&self) -> SecurityLogStats {
        SecurityLogStats {
            events: std::array::from_fn(|i| {
                self.events.get(i).map_or(0, |c| c.load(Ordering::Relaxed))
            }),
            suppressed: self.suppressed_total.load(Ordering::Relaxed),
            untracked: self.untracked_total.load(Ordering::Relaxed),
            tracked_sources: self.tracked_sources(),
        }
    }

    fn shard(&self, key: Key) -> Option<&Mutex<Shard>> {
        // The hasher is seeded randomly per process, so a client cannot aim
        // all its sources at one shard.
        let h = usize::try_from(self.hasher.hash_one(key)).unwrap_or_default();
        self.shards.get(h % self.shards.len().max(1))
    }

    fn window_index(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.epoch).as_millis();
        let window = self.limits.window.as_millis().max(1);
        u64::try_from(elapsed / window).unwrap_or(u64::MAX)
    }

    /// Takes one unit of the replica-wide budget for verified-identity events
    /// (`verified`) or for everything else, including summaries.
    fn admit(&self, verified: bool, now: Instant) -> bool {
        let idx = self.window_index(now);
        if verified {
            self.verified.admit(idx, self.limits.verified_global)
        } else {
            self.global.admit(idx, self.limits.global)
        }
    }

    fn suppress(&self) {
        self.suppressed_total.fetch_add(1, Ordering::Relaxed);
    }

    fn expired(&self, slot: &Slot, now: Instant) -> bool {
        now.saturating_duration_since(slot.window_start) >= self.limits.window
    }

    fn window_end(&self, slot: &Slot) -> Option<Instant> {
        slot.window_start.checked_add(self.limits.window)
    }

    /// Removes slots whose window ended and that owe no summary, and
    /// recomputes the shard's next eviction bound from the remaining slots.
    fn evict(&self, shard: &mut Shard, now: Instant) {
        #[cfg(test)]
        self.evict_scans.fetch_add(1, Ordering::Relaxed);
        let before = shard.map.len();
        let mut next = None;
        shard.map.retain(|_, s| {
            if s.suppressed > 0 {
                // Evictable only after its window restarts, which lowers the
                // bound again.
                return true;
            }
            if self.expired(s, now) {
                return false;
            }
            lower(&mut next, self.window_end(s));
            true
        });
        shard.next_evict = next;
        self.tracked
            .fetch_sub(before.saturating_sub(shard.map.len()), Ordering::Relaxed);
    }

    fn decide(&self, key: Key, now: Instant, verified: bool) -> Decision {
        let Some(shard) = self.shard(key) else {
            return Decision {
                summary: None,
                emit: false,
            };
        };
        let mut guard = shard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cap = self.limits.shard_capacity;
        // Scan only when a slot can actually be evicted (`next_evict` is a
        // lower bound), so a shard full of active sources costs no scan.
        if guard.map.len() >= cap
            && guard.next_evict.is_some_and(|t| t <= now)
            && !guard.map.contains_key(&key)
        {
            self.evict(&mut guard, now);
        }
        let len = guard.map.len();
        let fresh = Slot {
            window_start: now,
            emitted: 0,
            suppressed: 0,
        };
        let fresh_end = self.window_end(&fresh);
        let Shard { map, next_evict } = &mut *guard;
        let slot = match map.entry(key) {
            Entry::Occupied(o) => Some(o.into_mut()),
            Entry::Vacant(v) if len < cap => {
                self.tracked.fetch_add(1, Ordering::Relaxed);
                lower(next_evict, fresh_end);
                Some(v.insert(fresh))
            }
            Entry::Vacant(_) => None,
        };
        let Some(slot) = slot else {
            drop(guard);
            // Untracked: the table is full of active sources.
            self.untracked_total.fetch_add(1, Ordering::Relaxed);
            if self.admit(verified, now) {
                return Decision {
                    summary: None,
                    emit: true,
                };
            }
            self.suppress();
            self.untracked_pending.fetch_add(1, Ordering::Relaxed);
            return Decision {
                summary: None,
                emit: false,
            };
        };
        let mut summary = None;
        if self.expired(slot, now) {
            let pending = slot.suppressed;
            *slot = fresh;
            lower(next_evict, fresh_end);
            if pending > 0 {
                // Summaries always use the general budget, so they cannot
                // consume the verified-identity budget.
                if self.admit(false, now) {
                    summary = Some(pending);
                } else {
                    // Budget exhausted: keep the count for a later summary.
                    slot.suppressed = pending;
                }
            }
        }
        let emit = slot.emitted < self.limits.per_source && self.admit(verified, now);
        if emit {
            slot.emitted += 1;
        } else {
            slot.suppressed = slot.suppressed.saturating_add(1);
            self.suppress();
        }
        Decision { summary, emit }
    }

    fn collect_due(&self, now: Instant) -> Due {
        let mut due = Due::default();
        for shard in &self.shards {
            let mut guard = shard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Shard { map, next_evict } = &mut *guard;
            let before = map.len();
            let mut next = None;
            map.retain(|key, s| {
                if !self.expired(s, now) {
                    if s.suppressed == 0 {
                        lower(&mut next, self.window_end(s));
                    }
                    return true;
                }
                if s.suppressed > 0 {
                    if due.sources.len() < self.limits.summary_cap {
                        due.sources.push((*key, s.suppressed));
                    } else {
                        due.global = due.global.saturating_add(s.suppressed);
                    }
                }
                false
            });
            *next_evict = next;
            self.tracked
                .fetch_sub(before.saturating_sub(map.len()), Ordering::Relaxed);
        }
        due.global = due
            .global
            .saturating_add(self.untracked_pending.swap(0, Ordering::Relaxed));
        due
    }

    fn write_source_summary(&self, key: Key, n: u64) {
        tracing::warn!(
            target: TARGET,
            event = "suppressed",
            scope = "source",
            source = %key,
            suppressed = n,
            window_secs = self.limits.window.as_secs(),
            "security events suppressed"
        );
    }
}

/// Writes one event. Every field is enumerated, parsed or verified.
// NIST 800-53 Rev5: AU-3 (what: event/reason, where: listener/method/route, source: src_ip, outcome: status, identity only when verified; the timestamp comes from the layer)
fn write_event(ev: &SecurityEvent<'_>) {
    let method = method_label(ev.method);
    let src_ip = ev.ip.map(display);
    let tenant = ev.tenant.map(display);
    let agent = ev.agent.map(display);
    if ev.reason.is_rate_limit() {
        tracing::warn!(
            target: TARGET,
            event = "rate_limited",
            reason = ev.reason.as_str(),
            status = ev.status,
            listener = ev.listener.as_str(),
            src_ip,
            method,
            route = ev.route,
            subject = ev.subject,
            tenant,
            "request rate limited"
        );
    } else {
        tracing::warn!(
            target: TARGET,
            event = "auth_failure",
            reason = ev.reason.as_str(),
            claim = ev.claim,
            status = ev.status,
            listener = ev.listener.as_str(),
            src_ip,
            method,
            route = ev.route,
            subject = ev.subject,
            tenant,
            agent,
            "authentication failed"
        );
    }
}

/// The method as a standard name, or `OTHER` for extension methods (which
/// are arbitrary client-chosen tokens).
pub(crate) fn method_label(m: &Method) -> &'static str {
    match m.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "CONNECT" => "CONNECT",
        "TRACE" => "TRACE",
        _ => "OTHER",
    }
}

/// Whether `meta` is on exactly the [`TARGET`] target (not a prefix match:
/// `security_x` is not a security event).
fn is_security_target(meta: &tracing::Metadata<'_>) -> bool {
    meta.target() == TARGET
}

/// The production security layer: JSON, one flattened object per line, UTC
/// RFC 3339 timestamps, no span context, enabled for exactly the [`TARGET`]
/// target at `WARN` and above regardless of `RUST_LOG`.
// NIST 800-53 Rev5: AU-12 (security events are always generated; log-level tuning cannot silence them)
pub fn layer<S, W>(writer: W) -> impl Layer<S> + Send + Sync + 'static
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .with_target(true)
        .with_ansi(false)
        // NIST 800-53 Rev5: AU-8 (timestamps in UTC, RFC 3339)
        .with_timer(UtcTime::rfc_3339())
        .with_writer(writer)
        .with_filter(
            filter_fn(|m| is_security_target(m) && *m.level() <= tracing::Level::WARN)
                .with_max_level_hint(LevelFilter::WARN),
        )
}

/// The gateway's process-wide subscriber: the general JSON log on `general`,
/// filtered by `env` (`RUST_LOG`) and never carrying the [`TARGET`] target,
/// plus the security [`layer`] on `security`, which `env` cannot silence.
///
/// The general filter keeps `env`'s static max-level hint, so disabled
/// `debug!`/`trace!` callsites (and `log` records via `LogTracer`) keep their
/// fast path; the subscriber's hint is the more verbose of `env`'s and `WARN`.
// NIST 800-53 Rev5: AU-12 (security events are generated whatever RUST_LOG says)
pub fn subscriber<G, W>(
    env: EnvFilter,
    general: G,
    security: W,
) -> impl tracing::Subscriber + for<'a> LookupSpan<'a> + Send + Sync + 'static
where
    G: for<'w> MakeWriter<'w> + Send + Sync + 'static,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let not_security =
        filter_fn(|m| !is_security_target(m)).with_max_level_hint(LevelFilter::TRACE);
    let general = tracing_subscriber::fmt::layer()
        .json()
        // NIST 800-53 Rev5: AU-8 (timestamps in UTC, RFC 3339)
        .with_timer(UtcTime::rfc_3339())
        .with_writer(general)
        .with_filter(env.and(not_security));
    tracing_subscriber::registry()
        .with(general)
        .with(layer(security))
}

/// Test support: capture the security log through the production [`layer`].
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt as _;

    /// An in-memory log sink. Clones share the buffer.
    #[derive(Debug, Clone, Default)]
    pub struct LogCapture(Arc<Mutex<Vec<u8>>>);

    /// Writer handed out by [`LogCapture`].
    #[derive(Debug)]
    pub struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for LogCapture {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CaptureWriter(Arc::clone(&self.0))
        }
    }

    impl LogCapture {
        /// An empty capture.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Installs the production security layer writing into this capture
        /// as the current thread's default subscriber, until the guard drops.
        /// Use it in `#[tokio::test]` (current-thread runtime).
        #[must_use]
        pub fn install(&self) -> tracing::subscriber::DefaultGuard {
            let sub = tracing_subscriber::registry().with(super::layer(self.clone()));
            let guard = tracing::subscriber::set_default(sub);
            // Interest cached while another subscriber was current would
            // otherwise hide callsites from this one.
            tracing::callsite::rebuild_interest_cache();
            guard
        }

        /// Everything written so far.
        #[must_use]
        pub fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner))
                .into_owned()
        }

        /// Each line parsed as JSON (unparseable lines are skipped).
        #[must_use]
        pub fn events(&self) -> Vec<serde_json::Value> {
            self.contents()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        }

        /// Discards everything captured so far.
        pub fn clear(&self) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn v4(n: u32) -> Option<IpAddr> {
        Some(IpAddr::V4(Ipv4Addr::from(n)))
    }

    fn ev(ip: Option<IpAddr>) -> SecurityEvent<'static> {
        SecurityEvent {
            reason: Reason::Missing,
            claim: None,
            status: 401,
            ip,
            method: &Method::GET,
            route: "/api/v1/me",
            listener: Listener::Public,
            subject: None,
            tenant: None,
            agent: None,
        }
    }

    const fn emitted() -> Decision {
        Decision {
            summary: None,
            emit: true,
        }
    }

    #[test]
    fn admits_per_source_then_suppresses() {
        let log = SecurityLog::new();
        let t0 = Instant::now();
        let key = Key::of(v4(1));
        let admitted = (0..15).filter(|_| log.decide(key, t0, false).emit).count();
        assert_eq!(admitted, 10);
        assert_eq!(log.stats().suppressed, 5);
        // Next window: the pending summary comes first, then the event.
        let d = log.decide(key, t0 + WINDOW, false);
        assert_eq!(
            d,
            Decision {
                summary: Some(5),
                emit: true
            }
        );
    }

    #[test]
    fn flush_reports_quiet_sources_and_evicts() {
        let log = SecurityLog::new();
        let t0 = Instant::now();
        let key = Key::of(v4(7));
        for _ in 0..15 {
            log.decide(key, t0, false);
        }
        log.decide(Key::of(v4(8)), t0, false);
        assert_eq!(log.tracked_sources(), 2);
        // Not due yet.
        assert!(
            log.collect_due(t0 + Duration::from_secs(30))
                .sources
                .is_empty()
        );
        let due = log.collect_due(t0 + Duration::from_secs(61));
        assert_eq!(due.sources, vec![(key, 5)]);
        assert_eq!(due.global, 0);
        assert_eq!(log.tracked_sources(), 0);
    }

    #[test]
    fn global_budget_caps_all_sources() {
        let log = SecurityLog::new();
        let t0 = Instant::now();
        let admitted = (0..400)
            .filter(|n| log.decide(Key::of(v4(*n)), t0, false).emit)
            .count();
        assert_eq!(admitted, 300);
        assert_eq!(log.stats().suppressed, 100);
        // The budget renews in the next global window.
        assert!(log.decide(Key::of(v4(10_000)), t0 + WINDOW, false).emit);
    }

    #[test]
    fn table_is_bounded() {
        let log = SecurityLog::new();
        let t0 = Instant::now();
        for n in 0..10_000 {
            log.decide(Key::of(v4(n)), t0, false);
        }
        assert!(log.tracked_sources() <= SHARDS * SHARD_CAPACITY);
        let s = log.stats();
        assert!(s.untracked >= 10_000 - (SHARDS * SHARD_CAPACITY) as u64);
        assert_eq!(s.tracked_sources, log.tracked_sources());

        let small = SecurityLog::with_limits(SecurityLogLimits {
            shards: 1,
            shard_capacity: 4,
            ..SecurityLogLimits::default()
        });
        for n in 0..100 {
            small.decide(Key::of(v4(n)), t0, false);
        }
        assert_eq!(small.tracked_sources(), 4);
        assert_eq!(small.stats().untracked, 96);
        small.collect_due(t0 + WINDOW);
        assert_eq!(small.tracked_sources(), 0);
        assert!(small.shards[0].lock().unwrap().map.is_empty());
    }

    #[test]
    fn eviction_keeps_counts() {
        let log = SecurityLog::with_limits(SecurityLogLimits {
            shards: 1,
            shard_capacity: 2,
            per_source: 1,
            ..SecurityLogLimits::default()
        });
        let t0 = Instant::now();
        let (a, b, c) = (Key::of(v4(1)), Key::of(v4(2)), Key::of(v4(3)));
        log.decide(a, t0, false);
        log.decide(a, t0, false); // a owes a summary of 1.
        log.decide(b, t0, false); // b owes nothing.
        let later = t0 + WINDOW;
        // Table full; b's window ended with nothing pending, so b is evicted
        // to make room for c, while a is kept with its count.
        assert_eq!(log.decide(c, later, false), emitted());
        let guard = log.shards[0].lock().unwrap();
        let map = &guard.map;
        assert!(map.contains_key(&a) && map.contains_key(&c) && !map.contains_key(&b));
        assert_eq!(map[&a].suppressed, 1);
        drop(guard);
        assert_eq!(log.tracked_sources(), 2);
        let due = log.collect_due(later + WINDOW);
        assert_eq!(due.sources, vec![(a, 1)]);
    }

    #[test]
    fn exhausted_budget_keeps_pending_summary() {
        let log = SecurityLog::with_limits(SecurityLogLimits {
            global: 2,
            per_source: 1,
            ..SecurityLogLimits::default()
        });
        let t0 = Instant::now();
        let a = Key::of(v4(1));
        log.decide(a, t0, false);
        log.decide(a, t0, false);
        // Next window: two other sources use up the budget first.
        let t1 = t0 + WINDOW;
        log.decide(Key::of(v4(2)), t1, false);
        log.decide(Key::of(v4(3)), t1, false);
        let d = log.decide(a, t1, false);
        assert_eq!(
            d,
            Decision {
                summary: None,
                emit: false
            }
        );
        // The old count is carried, plus this suppressed event.
        let due = log.collect_due(t1 + WINDOW);
        assert!(due.sources.contains(&(a, 2)));
    }

    #[test]
    fn summary_cap_rolls_into_global() {
        let log = SecurityLog::with_limits(SecurityLogLimits {
            per_source: 0,
            summary_cap: 2,
            ..SecurityLogLimits::default()
        });
        let t0 = Instant::now();
        for n in 0..5 {
            log.decide(Key::of(v4(n)), t0, false);
        }
        let due = log.collect_due(t0 + WINDOW);
        assert_eq!(due.sources.len(), 2);
        assert_eq!(due.global, 3);
    }

    #[test]
    fn ipv6_groups_by_64_and_mapped_v4_is_canonical() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff:1:2:3".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(Key::of(Some(a)), Key::of(Some(b)));
        assert_ne!(Key::of(Some(a)), Key::of(Some(c)));
        assert_eq!(Key::of(Some(a)).to_string(), "2001:db8:1:2::/64");
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        let plain: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(Key::of(Some(mapped)), Key::of(Some(plain)));
        assert_eq!(Key::of(Some(mapped)).to_string(), "192.0.2.1");
        assert_eq!(Key::of(None).to_string(), "unknown");
    }

    #[test]
    fn reason_mapping_covers_every_auth_error() {
        assert_eq!(
            Reason::from_auth_error(&AuthError::Malformed),
            Some((Reason::Malformed, None))
        );
        assert_eq!(
            Reason::from_auth_error(&AuthError::Invalid),
            Some((Reason::Invalid, None))
        );
        assert_eq!(
            Reason::from_auth_error(&AuthError::Expired),
            Some((Reason::Expired, None))
        );
        let claim = AuthError::Claim("tenant_id".into());
        assert_eq!(
            Reason::from_auth_error(&claim),
            Some((Reason::Claim, Some("tenant_id")))
        );
        assert_eq!(Reason::from_auth_error(&AuthError::Provider), None);
        assert_eq!(
            Reason::from_auth_error(&AuthError::Config("x".into())),
            None
        );
        // Indexes are dense and in `ALL` order.
        for (i, r) in Reason::ALL.iter().enumerate() {
            assert_eq!(r.index(), i);
        }
    }

    #[test]
    fn method_label_normalises_extension_methods() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::DELETE), "DELETE");
        let ext = Method::from_bytes(b"CANARY-METHOD").unwrap();
        assert_eq!(method_label(&ext), "OTHER");
    }

    #[test]
    fn writes_the_schema_without_span_context() {
        let cap = testing::LogCapture::new();
        let _g = cap.install();
        let log = SecurityLog::new();
        let span = tracing::warn_span!("request", uri = "/canary-span?q=canary-query");
        let _e = span.enter();
        let tenant = TenantId::new();
        let agent = AgentId::new("agent-1").unwrap();
        log.record(&SecurityEvent {
            reason: Reason::Claim,
            claim: Some("tenant_id"),
            ip: v4(0xC000_0201),
            ..ev(None)
        });
        log.record(&SecurityEvent {
            reason: Reason::RateLimitedPrincipal,
            status: 429,
            subject: Some("verified-sub"),
            tenant: Some(tenant),
            ..ev(v4(0xC000_0201))
        });
        log.record(&SecurityEvent {
            reason: Reason::AgentUnregistered,
            status: 403,
            listener: Listener::Agent,
            route: "/agent/v1",
            agent: Some(&agent),
            ..ev(None)
        });
        tracing::warn!("not a security event");
        let out = cap.contents();
        assert!(!out.contains("canary"), "{out}");
        assert!(!out.contains("not a security event"));
        let evs = cap.events();
        assert_eq!(evs.len(), 3, "{out}");
        let e = &evs[0];
        assert_eq!(e["target"], "security");
        assert_eq!(e["level"], "WARN");
        assert_eq!(e["event"], "auth_failure");
        assert_eq!(e["reason"], "claim");
        assert_eq!(e["claim"], "tenant_id");
        assert_eq!(e["status"], 401);
        assert_eq!(e["listener"], "public");
        assert_eq!(e["src_ip"], "192.0.2.1");
        assert_eq!(e["method"], "GET");
        assert_eq!(e["route"], "/api/v1/me");
        assert_eq!(e["message"], "authentication failed");
        let ts = e["timestamp"].as_str().unwrap();
        let parsed =
            time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339)
                .unwrap();
        assert!(parsed.offset().is_utc() && ts.ends_with('Z'), "{ts}");
        assert!(e.get("subject").is_none() && e.get("tenant").is_none());
        assert!(e.get("span").is_none() && e.get("spans").is_none());
        let r = &evs[1];
        assert_eq!(r["event"], "rate_limited");
        assert_eq!(r["reason"], "rate_limited_principal");
        assert_eq!(r["subject"], "verified-sub");
        assert_eq!(r["tenant"], tenant.to_string());
        assert_eq!(r["message"], "request rate limited");
        let a = &evs[2];
        assert_eq!(a["reason"], "agent_unregistered");
        assert_eq!(a["agent"], "agent-1");
        assert_eq!(a["listener"], "agent");
        assert!(a.get("src_ip").is_none());
    }

    #[test]
    fn flush_writes_source_and_global_summaries() {
        let cap = testing::LogCapture::new();
        let _g = cap.install();
        let log = SecurityLog::with_limits(SecurityLogLimits {
            shards: 1,
            shard_capacity: 1,
            global: 10,
            ..SecurityLogLimits::default()
        });
        let t0 = Instant::now();
        for _ in 0..12 {
            log.record_at(&ev(v4(1)), t0);
        }
        // Untracked (table full) and the global budget is spent: counted.
        log.record_at(&ev(v4(2)), t0);
        log.flush_at(t0 + WINDOW);
        let evs = cap.events();
        assert_eq!(evs.len(), 12, "{}", cap.contents());
        let s = &evs[10];
        assert_eq!(s["event"], "suppressed");
        assert_eq!(s["scope"], "source");
        assert_eq!(s["source"], "0.0.0.1");
        assert_eq!(s["suppressed"], 2);
        assert_eq!(s["window_secs"], 60);
        let g = &evs[11];
        assert_eq!(g["scope"], "global");
        assert_eq!(g["suppressed"], 1);
        assert!(g.get("source").is_none());
        let stats = log.stats();
        assert_eq!(stats.events(Reason::Missing), 13);
        assert_eq!(stats.suppressed, 3);
        assert_eq!(stats.untracked, 1);
    }

    #[test]
    fn full_shard_of_active_sources_skips_the_eviction_scan() {
        let log = SecurityLog::with_limits(SecurityLogLimits {
            shards: 1,
            shard_capacity: 4,
            ..SecurityLogLimits::default()
        });
        let t0 = Instant::now();
        for n in 0..4 {
            log.decide(Key::of(v4(n)), t0, false);
        }
        assert_eq!(log.shards[0].lock().unwrap().next_evict, Some(t0 + WINDOW));
        // Nothing can expire inside the window: untracked sources cost no scan.
        for n in 100..1_100 {
            log.decide(Key::of(v4(n)), t0 + Duration::from_secs(30), false);
        }
        assert_eq!(log.evict_scans.load(Ordering::Relaxed), 0);
        assert_eq!(log.stats().untracked, 1_000);
        // Sources that owe a summary are not evictable: one scan finds none
        // and clears the bound, so later untracked events skip the scan too.
        let busy = SecurityLog::with_limits(SecurityLogLimits {
            shards: 1,
            shard_capacity: 2,
            per_source: 0,
            ..SecurityLogLimits::default()
        });
        busy.decide(Key::of(v4(1)), t0, false);
        busy.decide(Key::of(v4(2)), t0, false);
        let later = t0 + WINDOW;
        for n in 10..20 {
            busy.decide(Key::of(v4(n)), later, false);
        }
        assert_eq!(busy.evict_scans.load(Ordering::Relaxed), 1);
        assert_eq!(busy.shards[0].lock().unwrap().next_evict, None);
        // Once a window ends with nothing owed, the scan runs and evicts.
        let quiet = SecurityLog::with_limits(SecurityLogLimits {
            shards: 1,
            shard_capacity: 1,
            ..SecurityLogLimits::default()
        });
        quiet.decide(Key::of(v4(1)), t0, false);
        assert!(quiet.decide(Key::of(v4(2)), later, false).emit);
        assert_eq!(quiet.evict_scans.load(Ordering::Relaxed), 1);
        let sh = quiet.shards[0].lock().unwrap();
        assert!(sh.map.contains_key(&Key::of(v4(2))) && sh.map.len() == 1);
        assert_eq!(sh.next_evict, Some(later + WINDOW));
        drop(sh);
        // Counts stay exact across the skipped and the real scans.
        let due = busy.collect_due(later + WINDOW);
        assert_eq!(
            due.sources.iter().map(|(_, n)| n).sum::<u64>() + due.global,
            busy.stats().suppressed
        );
        // Each busy source owes one; the untracked events fit the budget.
        assert_eq!(busy.stats().suppressed, 2);
    }

    #[test]
    fn verified_events_have_their_own_budget() {
        let log = SecurityLog::with_limits(SecurityLogLimits {
            global: 5,
            verified_global: 2,
            ..SecurityLogLimits::default()
        });
        let t0 = Instant::now();
        // Unauthenticated events from many sources spend the general budget.
        let admitted = (0..50)
            .filter(|n| log.decide(Key::of(v4(*n)), t0, false).emit)
            .count();
        assert_eq!(admitted, 5);
        // Verified events still get through, up to their own budget.
        assert!(log.decide(Key::of(v4(1_000)), t0, true).emit);
        assert!(log.decide(Key::of(v4(1_001)), t0, true).emit);
        assert!(!log.decide(Key::of(v4(1_002)), t0, true).emit);
        // They never take from the general budget.
        assert!(!log.decide(Key::of(v4(2_000)), t0, false).emit);
        // The per-source limit still applies to them.
        let per = SecurityLog::with_limits(SecurityLogLimits {
            per_source: 3,
            ..SecurityLogLimits::default()
        });
        let key = Key::of(v4(7));
        let admitted = (0..10).filter(|_| per.decide(key, t0, true).emit).count();
        assert_eq!(admitted, 3);
    }

    #[test]
    fn only_the_exact_security_target_is_written() {
        let cap = testing::LogCapture::new();
        let _g = cap.install();
        tracing::warn!(target: "security_x", "lookalike target");
        tracing::error!(target: "securityx", "lookalike target");
        tracing::warn!(target: "security::inner", "lookalike target");
        tracing::info!(target: TARGET, "below warn");
        tracing::warn!(target: TARGET, "real");
        tracing::error!(target: TARGET, "real");
        let evs = cap.events();
        assert_eq!(evs.len(), 2, "{}", cap.contents());
        assert!(
            evs.iter()
                .all(|e| e["target"] == TARGET && e["message"] == "real")
        );
    }

    #[derive(Debug, Clone)]
    struct Step {
        source: u8,
        advance_ms: u32,
        verified: bool,
    }

    fn step() -> impl Strategy<Value = Step> {
        (0u8..12, 0u32..20_000, any::<bool>()).prop_map(|(source, advance_ms, verified)| Step {
            source,
            advance_ms,
            verified,
        })
    }

    proptest! {
        #[test]
        fn no_count_is_lost_and_state_is_bounded(
            steps in proptest::collection::vec(step(), 1..400),
            per_source in 0u32..5,
            global in 0u64..40,
            verified_global in 0u64..10,
            capacity in 1usize..6,
        ) {
            let window = Duration::from_secs(60);
            let log = SecurityLog::with_limits(SecurityLogLimits {
                window,
                per_source,
                global,
                verified_global,
                shards: 2,
                shard_capacity: capacity,
                summary_cap: 3,
            });
            let t0 = Instant::now();
            let mut now = t0;
            let mut emitted = 0u64;
            let mut summarised = 0u64;
            // (key, window start) -> events admitted individually in that window.
            let mut per_window: HashMap<(Key, Instant), u32> = HashMap::new();
            for s in &steps {
                now += Duration::from_millis(u64::from(s.advance_ms));
                let key = Key::of(v4(u32::from(s.source)));
                let d = log.decide(key, now, s.verified);
                summarised += d.summary.unwrap_or(0);
                if d.emit {
                    emitted += 1;
                    let start = log.shard(key).unwrap().lock().unwrap().map.get(&key).map(|sl| sl.window_start);
                    if let Some(start) = start {
                        let n = per_window.entry((key, start)).or_insert(0);
                        *n += 1;
                        prop_assert!(*n <= per_source);
                    }
                }
                let mut held = 0;
                for m in &log.shards {
                    let sh = m.lock().unwrap();
                    held += sh.map.len();
                    // The skip is safe: whenever a slot is evictable, the
                    // shard's bound says a scan is due.
                    if sh.map.values().any(|sl| log.expired(sl, now) && sl.suppressed == 0) {
                        prop_assert!(sh.next_evict.is_some_and(|t| t <= now));
                    }
                }
                prop_assert_eq!(held, log.tracked_sources());
                prop_assert!(held <= 2 * capacity);
            }
            let due = log.collect_due(now + window);
            summarised += due.sources.iter().map(|(_, n)| n).sum::<u64>() + due.global;
            let total = steps.len() as u64;
            prop_assert_eq!(emitted + summarised, total);
            prop_assert_eq!(log.stats().suppressed, summarised);
            prop_assert_eq!(log.tracked_sources(), 0);
        }
    }
}
