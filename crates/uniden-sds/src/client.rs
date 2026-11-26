//! The public [`Scanner`] handle.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uniden_protocol::xml::{GltList, ScannerInfo};
use uniden_protocol::{
    AvoidStatus, Command, DisplayStatus, FirmwareVersion, Frequency, JumpModeKind, KeyCode,
    KeyMode, ListRequest, Location, Model, QuickKeys, Reply, SCAN_FROM_TOP, ScannerDateTime,
    ScannerError, Squelch, Target, Volume,
};

use crate::config::ScannerConfig;
use crate::error::{Error, Result};
use crate::health::{Health, HealthTracker};
use crate::worker::{Request, Stats, StatsSnapshot, Worker};

/// Result of probing one command during capability detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    /// The scanner returned a well-formed reply.
    Supported,
    /// The scanner rejected the command (`ERR` usually means unsupported;
    /// `NG` means unavailable in the current state).
    Rejected(ScannerError),
    /// No reply was received (unsupported over the network, or lost).
    NoReply,
    /// A reply arrived but could not be decoded.
    Undecodable(String),
}

/// Capabilities detected by [`Scanner::probe_capabilities`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Reported model, if `MDL` succeeded.
    pub model: Option<Model>,
    /// Reported firmware, if `VER` succeeded.
    pub firmware: Option<FirmwareVersion>,
    /// Per-command probe results, keyed by probe name.
    pub commands: BTreeMap<String, Support>,
}

impl Capabilities {
    /// True if the named probe succeeded.
    #[must_use]
    pub fn supports(&self, probe: &str) -> bool {
        matches!(self.commands.get(probe), Some(Support::Supported))
    }
}

/// Read-only probes used for capability detection. No probe changes state.
const PROBES: [(&str, Command); 10] = [
    ("GSI", Command::GetScannerInfo),
    ("STS", Command::GetStatus),
    ("VOL", Command::GetVolume),
    ("SQL", Command::GetSquelch),
    ("FQK", Command::GetFavoritesQuickKeys),
    ("DTM", Command::GetDateTime),
    ("LCR", Command::GetLocation),
    ("URC", Command::GetUserRecord),
    ("GLT,FL", Command::GetList(ListRequest::FavoritesLists)),
    ("VER", Command::GetFirmwareVersion),
];

/// A handle to one scanner. Cheap to clone; all clones share one command
/// queue, so commands to a scanner are always executed one at a time.
///
/// # Example
///
/// ```no_run
/// use uniden_sds::{Scanner, ScannerConfig, DEFAULT_PORT};
///
/// # async fn demo() -> uniden_sds::Result<()> {
/// let addr = format!("192.0.2.10:{DEFAULT_PORT}").parse().expect("valid address");
/// let scanner = Scanner::connect(ScannerConfig::new(addr)).await?;
/// println!("{} {}", scanner.model().await?, scanner.firmware_version().await?);
/// let info = scanner.scanner_info().await?;
/// if let Some(freq) = info.active_frequency() {
///     println!("receiving {freq}");
/// }
/// scanner.shutdown().await;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Scanner {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    address: SocketAddr,
    requests: mpsc::Sender<Request>,
    push: broadcast::Sender<Arc<ScannerInfo>>,
    health: watch::Receiver<Health>,
    stats: Arc<Stats>,
    cancel: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

macro_rules! expect_reply {
    ($cmd:expr, $reply:expr, $pat:pat => $out:expr) => {
        match $reply {
            $pat => Ok($out),
            other => Err(Error::UnexpectedReply {
                command: $cmd.name().to_owned(),
                reply: format!("{other:?}"),
            }),
        }
    };
}

impl Scanner {
    /// Binds a local UDP socket and starts the per-scanner worker. No packet
    /// is sent until the first command.
    ///
    /// # Errors
    /// Returns an error if the socket cannot be bound or connected.
    pub async fn connect(config: ScannerConfig) -> Result<Self> {
        let bind = config.bind.unwrap_or_else(|| {
            if config.address.is_ipv4() {
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
            } else {
                SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
            }
        });
        let socket = UdpSocket::bind(bind).await?;
        // A connected socket only accepts datagrams from the scanner address.
        socket.connect(config.address).await?;
        let (tx, rx) = mpsc::channel(config.queue_depth.max(1));
        let (push, _) = broadcast::channel(config.push_capacity.max(1));
        let (health_tx, health_rx) = watch::channel(Health::Unknown);
        let stats = Arc::new(Stats::default());
        let cancel = CancellationToken::new();
        let address = config.address;
        let buf = vec![0; config.max_datagram.clamp(512, 65_535)];
        let worker = Worker {
            assembler: uniden_protocol::FrameAssembler::new(config.frame_limits),
            config,
            socket,
            requests: rx,
            push: push.clone(),
            health: health_tx,
            stats: Arc::clone(&stats),
            cancel: cancel.clone(),
            buf,
            tracker: HealthTracker::default(),
        };
        let task = tokio::spawn(worker.run());
        Ok(Self {
            inner: Arc::new(Inner {
                address,
                requests: tx,
                push,
                health: health_rx,
                stats,
                cancel,
                task: Mutex::new(Some(task)),
            }),
        })
    }

    /// The scanner address.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.inner.address
    }

    /// Current link health.
    #[must_use]
    pub fn health(&self) -> Health {
        *self.inner.health.borrow()
    }

    /// A receiver that observes health changes.
    #[must_use]
    pub fn watch_health(&self) -> watch::Receiver<Health> {
        self.inner.health.clone()
    }

    /// Protocol counters.
    #[must_use]
    pub fn stats(&self) -> StatsSnapshot {
        self.inner.stats.snapshot()
    }

    /// Subscribes to pushed `ScannerInfo` updates (enable with
    /// [`Scanner::start_push`]).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<ScannerInfo>> {
        self.inner.push.subscribe()
    }

    /// Stops the worker and waits for it to exit. Queued commands fail with
    /// [`Error::Closed`].
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        if let Some(task) = self.inner.task.lock().await.take() {
            let _ = task.await;
        }
    }

    /// Executes any command and returns the decoded reply.
    ///
    /// Read-only commands are retried according to the configuration;
    /// state-changing commands are sent exactly once.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn execute(&self, command: Command) -> Result<Reply> {
        command.encode()?;
        let (tx, rx) = oneshot::channel();
        self.inner
            .requests
            .try_send(Request { command, reply: tx })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::QueueFull,
                mpsc::error::TrySendError::Closed(_) => Error::Closed,
            })?;
        rx.await.map_err(|_| Error::Closed)?
    }

    async fn expect_ok(&self, command: Command) -> Result<()> {
        let name = command.name().to_owned();
        match self.execute(command).await? {
            Reply::Ok => Ok(()),
            other => Err(Error::UnexpectedReply {
                command: name,
                reply: format!("{other:?}"),
            }),
        }
    }

    /// `MDL`: model name.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn model(&self) -> Result<Model> {
        let c = Command::GetModel;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::Model(m) => m)
    }

    /// `VER`: firmware version.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn firmware_version(&self) -> Result<FirmwareVersion> {
        let c = Command::GetFirmwareVersion;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::FirmwareVersion(v) => v)
    }

    /// `GSI`: full scanner information (mode, system, department, channel,
    /// frequency, talkgroup, signal, volume, squelch, display areas).
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn scanner_info(&self) -> Result<ScannerInfo> {
        let c = Command::GetScannerInfo;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::ScannerInfo(i) => *i)
    }

    /// `STS`: legacy display status.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn display_status(&self) -> Result<DisplayStatus> {
        let c = Command::GetStatus;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::Status(s) => s)
    }

    /// Active frequency from `GSI`, if one is displayed.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn active_frequency(&self) -> Result<Option<Frequency>> {
        Ok(self.scanner_info().await?.active_frequency())
    }

    /// `VOL`: current volume (community-reported command).
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn volume(&self) -> Result<Volume> {
        let c = Command::GetVolume;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::Volume(v) => v)
    }

    /// `VOL,<level>`: set volume (community-reported command; not retried).
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn set_volume(&self, level: Volume) -> Result<()> {
        self.expect_ok(Command::SetVolume(level)).await
    }

    /// `SQL`: current squelch (community-reported command).
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn squelch(&self) -> Result<Squelch> {
        let c = Command::GetSquelch;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::Squelch(s) => s)
    }

    /// `SQL,<level>`: set squelch (community-reported command; not retried).
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn set_squelch(&self, level: Squelch) -> Result<()> {
        self.expect_ok(Command::SetSquelch(level)).await
    }

    /// `KEY,<code>,<mode>`: emulate a front-panel key.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn press_key(&self, code: KeyCode, mode: KeyMode) -> Result<()> {
        self.expect_ok(Command::Key { code, mode }).await
    }

    /// `HLD`: hold on a system, department, site or channel. Indexes come
    /// from [`ScannerInfo`] or [`Scanner::list`] and are valid only while
    /// `DB_Counter` is unchanged.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn hold(&self, target: Target) -> Result<()> {
        self.expect_ok(Command::Hold(target)).await
    }

    /// Resumes scanning with `JPM,SCN_MODE,4294967295`, which the
    /// specification documents as "start to scan from top channel". This
    /// restarts the scan rather than releasing a hold in place; SDS200
    /// behaviour requires hardware verification.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn resume_scan(&self) -> Result<()> {
        let index = uniden_protocol::Arg::new(SCAN_FROM_TOP.to_string())?;
        self.expect_ok(Command::JumpMode {
            mode: JumpModeKind::Scan,
            index,
        })
        .await
    }

    /// `AVD`: avoid or unavoid a target.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn avoid(&self, target: Target, status: AvoidStatus) -> Result<()> {
        self.expect_ok(Command::Avoid(target, status)).await
    }

    /// `QSH,<freq>`: quick search hold on a frequency.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn quick_search_hold(&self, frequency: Frequency) -> Result<()> {
        self.expect_ok(Command::QuickSearchHold(frequency)).await
    }

    /// `JNT`: jump to a number tag.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn jump_number_tag(
        &self,
        favorites_list: u8,
        system: u8,
        channel: u16,
    ) -> Result<()> {
        self.expect_ok(Command::JumpNumberTag {
            favorites_list,
            system,
            channel,
        })
        .await
    }

    /// `GLT`: fetch a list.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn list(&self, request: ListRequest) -> Result<GltList> {
        let c = Command::GetList(request);
        expect_reply!(c, self.execute(c.clone()).await?, Reply::List(l) => l)
    }

    /// `FQK`: favorites-list quick key states.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn favorites_quick_keys(&self) -> Result<QuickKeys> {
        let c = Command::GetFavoritesQuickKeys;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::QuickKeys(q) => q)
    }

    /// `DTM`: scanner date and time.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn date_time(&self) -> Result<ScannerDateTime> {
        let c = Command::GetDateTime;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::DateTime(d) => d)
    }

    /// `LCR`: configured location and range.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn location(&self) -> Result<Location> {
        let c = Command::GetLocation;
        expect_reply!(c, self.execute(c.clone()).await?, Reply::Location(l) => l)
    }

    /// `PSI,<ms>`: ask the scanner to push `ScannerInfo` periodically;
    /// receive updates with [`Scanner::subscribe`].
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn start_push(&self, interval_ms: u32) -> Result<()> {
        match self
            .execute(Command::PushScannerInfo {
                interval_ms: Some(interval_ms),
            })
            .await?
        {
            Reply::Ok | Reply::ScannerInfo(_) => Ok(()),
            other => Err(Error::UnexpectedReply {
                command: "PSI".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    /// `PSI,0`: stop pushes (community-reported semantics).
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn stop_push(&self) -> Result<()> {
        self.start_push(0).await
    }

    /// `POF`: power off (community-reported; not retried).
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn power_off(&self) -> Result<()> {
        self.expect_ok(Command::PowerOff).await
    }

    /// Probes model, firmware and a fixed set of **read-only** commands.
    /// No probe changes scanner state.
    pub async fn probe_capabilities(&self) -> Capabilities {
        let model = self.model().await.ok();
        let mut caps = Capabilities {
            model,
            firmware: None,
            commands: BTreeMap::new(),
        };
        for (name, command) in PROBES {
            let result = self.execute(command).await;
            if let Ok(Reply::FirmwareVersion(v)) = &result {
                caps.firmware = Some(v.clone());
            }
            let support = match result {
                Ok(_) => Support::Supported,
                Err(Error::Rejected { error, .. }) => Support::Rejected(error),
                Err(Error::Timeout { .. }) => Support::NoReply,
                Err(e) => Support::Undecodable(e.to_string()),
            };
            caps.commands.insert(name.to_owned(), support);
        }
        caps
    }
}
