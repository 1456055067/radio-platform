//! Disk journal for the outage buffer, so unacknowledged events survive an
//! agent restart or power loss.
//!
//! Layout of `state_dir`:
//! - `session`: the agent session UUID. It is reused across restarts, so
//!   the gateway's `(agent, session, seq)` de-duplication still holds for
//!   events replayed after a restart.
//! - `events.jsonl`: append-only JSON lines, either
//!   `{"e":{seq, scanner_id, event}}` or `{"a":seq}` (acknowledged up to
//!   `seq`).
//!
//! Each event append is fsynced before the event is considered buffered.
//! Acknowledgements are not fsynced: losing one only causes a duplicate
//! that the gateway drops. A torn final line (a crash mid-write) is
//! ignored. The journal is compacted (rewritten to a temporary file,
//! fsynced, then renamed) once acknowledged records dominate it.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use platform_common::{ScannerEvent, ScannerId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

/// Journal failures.
#[derive(Debug, Error)]
pub enum JournalError {
    /// File-system error.
    #[error("{path}: {source}")]
    Io {
        /// File.
        path: String,
        /// Cause.
        source: std::io::Error,
    },
    /// The session file is not a UUID.
    #[error("{0}: corrupt session file")]
    Session(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> JournalError + '_ {
    move |source| JournalError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// A journaled event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Sequence number.
    pub seq: u64,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Event.
    pub event: ScannerEvent,
}

#[derive(Debug, Serialize, Deserialize)]
enum Record {
    #[serde(rename = "e")]
    Event(Box<Entry>),
    #[serde(rename = "a")]
    Ack(u64),
}

/// What was recovered when opening a journal.
#[derive(Debug)]
pub struct Recovered {
    /// Session UUID (new if none was stored).
    pub session: uuid::Uuid,
    /// Unacknowledged events in order (at most `max_events`).
    pub events: VecDeque<Entry>,
    /// Next sequence number to assign.
    pub next_seq: u64,
    /// Events discarded because more than `max_events` were pending.
    pub dropped: u64,
    /// Unreadable lines skipped (a torn tail after a crash).
    pub skipped_lines: u64,
}

/// An open journal.
#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    file: File,
    records: u64,
    max_events: usize,
    compact_slack: u64,
}

const EVENTS: &str = "events.jsonl";
const SESSION: &str = "session";
/// Compact when the file holds this many records beyond the pending ones.
const COMPACT_SLACK: u64 = 4096;

#[cfg(unix)]
fn restrict_dir(dir: &Path) -> Result<(), JournalError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io(dir))
}

#[cfg(not(unix))]
fn restrict_dir(_: &Path) -> Result<(), JournalError> {
    Ok(())
}

fn open_append(path: &Path) -> Result<File, JournalError> {
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path).map_err(io(path))
}

fn write_atomically(dir: &Path, name: &str, body: &[u8]) -> Result<(), JournalError> {
    let tmp = dir.join(format!("{name}.tmp"));
    let dst = dir.join(name);
    {
        let mut o = OpenOptions::new();
        o.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp).map_err(io(&tmp))?;
        f.write_all(body).map_err(io(&tmp))?;
        f.sync_all().map_err(io(&tmp))?;
    }
    std::fs::rename(&tmp, &dst).map_err(io(&dst))?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

impl Journal {
    /// Opens (creating if needed) the journal in `dir` and recovers its
    /// pending events.
    ///
    /// # Errors
    /// File-system errors or a corrupt session file.
    pub fn open(dir: &Path, max_events: usize) -> Result<(Self, Recovered), JournalError> {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        restrict_dir(dir)?;
        let max_events = max_events.max(1);

        let session_path = dir.join(SESSION);
        let session = match std::fs::read_to_string(&session_path) {
            Ok(s) => uuid::Uuid::parse_str(s.trim())
                .map_err(|_| JournalError::Session(session_path.display().to_string()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = uuid::Uuid::new_v4();
                write_atomically(dir, SESSION, s.to_string().as_bytes())?;
                s
            }
            Err(e) => return Err(io(&session_path)(e)),
        };

        let path = dir.join(EVENTS);
        let mut events: VecDeque<Entry> = VecDeque::new();
        let mut max_seq = 0u64;
        let mut skipped = 0u64;
        let mut records = 0u64;
        match File::open(&path) {
            Ok(f) => {
                for line in BufReader::new(f).lines() {
                    let Ok(line) = line else {
                        skipped += 1;
                        continue;
                    };
                    records += 1;
                    match serde_json::from_str::<Record>(&line) {
                        Ok(Record::Event(e)) => {
                            max_seq = max_seq.max(e.seq);
                            events.push_back(*e);
                        }
                        Ok(Record::Ack(seq)) => {
                            max_seq = max_seq.max(seq);
                            events.retain(|e| e.seq > seq);
                        }
                        Err(_) => skipped += 1,
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(&path)(e)),
        }
        if skipped > 0 {
            warn!(skipped, path = %path.display(), "ignored unreadable journal lines");
        }
        let mut dropped = 0u64;
        while events.len() > max_events {
            events.pop_front();
            dropped += 1;
        }
        let mut j = Self {
            dir: dir.to_owned(),
            file: open_append(&path)?,
            records,
            max_events,
            compact_slack: COMPACT_SLACK,
        };
        // Start from a clean file holding exactly the recovered state.
        j.compact(events.iter())?;
        Ok((
            j,
            Recovered {
                session,
                events,
                next_seq: max_seq + 1,
                dropped,
                skipped_lines: skipped,
            },
        ))
    }

    fn append(&mut self, r: &Record, sync: bool) -> Result<(), JournalError> {
        let path = self.dir.join(EVENTS);
        let mut line = serde_json::to_vec(r).map_err(|e| io(&path)(std::io::Error::other(e)))?;
        line.push(b'\n');
        self.file.write_all(&line).map_err(io(&path))?;
        if sync {
            self.file.sync_data().map_err(io(&path))?;
        }
        self.records += 1;
        Ok(())
    }

    /// Durably records an event.
    ///
    /// # Errors
    /// File-system errors.
    pub fn record_event(&mut self, e: &Entry) -> Result<(), JournalError> {
        self.append(&Record::Event(Box::new(e.clone())), true)
    }

    /// Records an acknowledgement and compacts when worthwhile.
    ///
    /// # Errors
    /// File-system errors.
    pub fn record_ack<'a>(
        &mut self,
        seq: u64,
        pending: impl Iterator<Item = &'a Entry> + Clone,
    ) -> Result<(), JournalError> {
        self.append(&Record::Ack(seq), false)?;
        let live = pending.clone().count() as u64;
        if self.records > live + self.compact_slack.max(self.max_events as u64) {
            self.compact(pending)?;
        }
        Ok(())
    }

    /// Rewrites the journal to hold exactly `pending`.
    ///
    /// # Errors
    /// File-system errors.
    pub fn compact<'a>(
        &mut self,
        pending: impl Iterator<Item = &'a Entry>,
    ) -> Result<(), JournalError> {
        let mut body = Vec::new();
        let mut n = 0u64;
        for e in pending {
            let path = self.dir.join(EVENTS);
            let mut line = serde_json::to_vec(&Record::Event(Box::new(e.clone())))
                .map_err(|x| io(&path)(std::io::Error::other(x)))?;
            line.push(b'\n');
            body.extend_from_slice(&line);
            n += 1;
        }
        write_atomically(&self.dir, EVENTS, &body)?;
        self.file = open_append(&self.dir.join(EVENTS))?;
        self.records = n;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_common::ScannerHealth;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("rp-journal-{}", uuid::Uuid::new_v4().simple()))
    }

    fn entry(seq: u64, s: ScannerId) -> Entry {
        Entry {
            seq,
            scanner_id: s,
            event: ScannerEvent::HealthChanged {
                health: ScannerHealth::Healthy,
            },
        }
    }

    #[test]
    fn survives_restart_with_same_session_and_sequence() {
        let dir = tmp();
        let s = ScannerId::new();
        let (mut j, r) = Journal::open(&dir, 100).unwrap_or_else(|e| panic!("{e}"));
        assert!(r.events.is_empty());
        assert_eq!(r.next_seq, 1);
        let session = r.session;
        let mut pending: Vec<Entry> = (1..=5).map(|n| entry(n, s)).collect();
        for e in &pending {
            j.record_event(e).unwrap_or_else(|e| panic!("{e}"));
        }
        pending.retain(|e| e.seq > 3);
        j.record_ack(3, pending.iter())
            .unwrap_or_else(|e| panic!("{e}"));
        drop(j);

        let (_, r) = Journal::open(&dir, 100).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.session, session, "session survives restart");
        assert_eq!(
            r.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(r.next_seq, 6, "sequence continues");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn torn_tail_is_ignored_and_bound_applies() {
        let dir = tmp();
        let s = ScannerId::new();
        let (mut j, _) = Journal::open(&dir, 3).unwrap_or_else(|e| panic!("{e}"));
        for n in 1..=5 {
            j.record_event(&entry(n, s))
                .unwrap_or_else(|e| panic!("{e}"));
        }
        drop(j);
        let mut f = open_append(&dir.join(EVENTS)).unwrap_or_else(|e| panic!("{e}"));
        f.write_all(b"{\"e\":{\"seq\":6,\"scann")
            .unwrap_or_else(|e| panic!("{e}"));
        drop(f);
        let (_, r) = Journal::open(&dir, 3).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.skipped_lines, 1);
        assert_eq!(r.dropped, 2, "oldest beyond the bound are dropped");
        assert_eq!(
            r.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert_eq!(r.next_seq, 6);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn compaction_keeps_only_pending() {
        let dir = tmp();
        let s = ScannerId::new();
        let (mut j, _) = Journal::open(&dir, 10).unwrap_or_else(|e| panic!("{e}"));
        j.compact_slack = 16;
        let mut last = Vec::new();
        for n in 1..=200u64 {
            let e = entry(n, s);
            j.record_event(&e).unwrap_or_else(|e| panic!("{e}"));
            last = vec![e];
            if n % 2 == 0 {
                last.clear();
                j.record_ack(n, last.iter())
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        }
        let lines = std::fs::read_to_string(dir.join(EVENTS))
            .unwrap_or_else(|e| panic!("{e}"))
            .lines()
            .count();
        assert!(lines < 60, "journal was compacted ({lines} lines)");
        drop(j);
        let (_, r) = Journal::open(&dir, 10).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            r.events
                .iter()
                .map(|e| e.seq)
                .eq(last.iter().map(|e| e.seq))
        );
        assert_eq!(r.next_seq, 201);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(EVENTS))
                .unwrap_or_else(|e| panic!("{e}"))
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "journal is private to the agent user");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
