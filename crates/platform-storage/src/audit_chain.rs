//! Tamper-evident audit chain (ADR 0011).
//!
//! Each tenant's audit records form a hash chain:
//! `hash(n) = SHA-256(hash(n-1) || canonical(record n))`, starting from 32
//! zero bytes. The canonical form is a JSON array with a fixed field order;
//! object keys inside `details` are sorted, so the encoding survives a round
//! trip through PostgreSQL `JSONB`. Changing, removing or reordering a
//! record breaks every later link; dropping the newest records is caught by
//! comparing with the stored chain head (which operators should also copy
//! somewhere the database administrator cannot write, such as the log
//! pipeline; the gateway logs it hourly).

use aws_lc_rs::digest;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::model::AuditRecord;

/// The link before the first record.
pub const GENESIS: [u8; 32] = [0; 32];

/// Lower-case hex.
#[must_use]
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Parses 64 hex characters.
#[must_use]
pub fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Sorts object keys recursively.
#[must_use]
pub fn canonical_json(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                if let Some(x) = m.get(k) {
                    out.insert(k.clone(), canonical_json(x));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

/// The bytes hashed for a record (version 1). `at` is in whole
/// microseconds, the database's precision.
#[must_use]
pub fn canonical(r: &AuditRecord, seq: u64) -> Vec<u8> {
    let v = serde_json::json!([
        1,
        seq,
        r.tenant_id.to_string(),
        r.at.unix_timestamp_nanos() / 1000,
        r.actor,
        r.action,
        r.resource_type,
        r.resource_id,
        r.outcome.as_str(),
        canonical_json(&r.details),
        r.source_ip.map(|ip| ip.to_string()),
    ]);
    // Serializing a `Value` cannot fail.
    serde_json::to_vec(&v).unwrap_or_default()
}

/// The next link.
#[must_use]
pub fn link(prev: &[u8; 32], r: &AuditRecord, seq: u64) -> [u8; 32] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(prev);
    ctx.update(&canonical(r, seq));
    let d = ctx.finish();
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// Truncates a timestamp to whole microseconds.
#[must_use]
pub fn micros(t: time::OffsetDateTime) -> time::OffsetDateTime {
    let ns = t.nanosecond();
    t.replace_nanosecond(ns - ns % 1000).unwrap_or(t)
}

/// Where and why a chain is broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChainBreak {
    /// Sequence number at which verification failed.
    pub seq: u64,
    /// Description.
    pub reason: String,
}

/// Incremental verifier: feed records in sequence order.
#[derive(Debug, Clone)]
pub struct Verifier {
    prev: [u8; 32],
    next_seq: u64,
    /// Records verified.
    pub verified: u64,
}

impl Default for Verifier {
    fn default() -> Self {
        Self {
            prev: GENESIS,
            next_seq: 1,
            verified: 0,
        }
    }
}

impl Verifier {
    /// Checks one record.
    ///
    /// # Errors
    /// [`ChainBreak`] if the record is missing, out of order, altered, or
    /// does not link to its predecessor.
    pub fn push(&mut self, r: &AuditRecord) -> Result<(), ChainBreak> {
        let seq = r.seq.unwrap_or(0);
        let fail = |reason: &str| ChainBreak {
            seq,
            reason: reason.to_owned(),
        };
        if seq != self.next_seq {
            return Err(ChainBreak {
                seq: self.next_seq,
                reason: format!("record {} is missing (found {seq})", self.next_seq),
            });
        }
        if r.prev_hash.as_deref().and_then(unhex) != Some(self.prev) {
            return Err(fail("does not link to the previous record"));
        }
        let h = link(&self.prev, r, seq);
        if r.hash.as_deref().and_then(unhex) != Some(h) {
            return Err(fail("content does not match its hash (altered)"));
        }
        self.prev = h;
        self.next_seq += 1;
        self.verified += 1;
        Ok(())
    }

    /// Checks the end of the chain against the stored head.
    ///
    /// # Errors
    /// [`ChainBreak`] if records after the last verified one are missing.
    pub fn finish(&self, head_seq: u64, head_hash: &str) -> Result<(), ChainBreak> {
        if head_seq != self.next_seq - 1 || unhex(head_hash) != Some(self.prev) {
            return Err(ChainBreak {
                seq: self.next_seq,
                reason: format!(
                    "chain ends at {} but the head says {head_seq} (records removed?)",
                    self.next_seq - 1
                ),
            });
        }
        Ok(())
    }

    /// The last verified hash.
    #[must_use]
    pub fn last(&self) -> [u8; 32] {
        self.prev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AuditOutcome;

    fn rec(n: u64, prev: &[u8; 32]) -> AuditRecord {
        let mut r = AuditRecord {
            id: i64::try_from(n).unwrap_or(0),
            at: micros(time::macros::datetime!(2026-11-25 10:00:00.123456789 UTC)),
            tenant_id: platform_common::TenantId(uuid::Uuid::nil()),
            actor: "alice".into(),
            action: "scanner.update".into(),
            resource_type: "scanner".into(),
            resource_id: Some("x".into()),
            outcome: AuditOutcome::Success,
            details: serde_json::json!({ "z": 1, "a": { "y": [1, 2], "b": null } }),
            source_ip: Some(
                "192.0.2.1"
                    .parse()
                    .unwrap_or(std::net::IpAddr::from([0, 0, 0, 0])),
            ),
            seq: Some(n),
            prev_hash: Some(hex(prev)),
            hash: None,
        };
        r.hash = Some(hex(&link(prev, &r, n)));
        r
    }

    #[test]
    fn canonical_form_ignores_key_order() {
        let a = canonical_json(&serde_json::json!({ "b": 1, "a": { "d": 2, "c": 3 } }));
        assert_eq!(
            serde_json::to_string(&a).unwrap_or_default(),
            r#"{"a":{"c":3,"d":2},"b":1}"#
        );
        let mut r = rec(1, &GENESIS);
        let h = link(&GENESIS, &r, 1);
        r.details = serde_json::json!({ "a": { "b": null, "y": [1, 2] }, "z": 1 });
        assert_eq!(link(&GENESIS, &r, 1), h);
        assert_eq!(r.at.nanosecond(), 123_456_000);
    }

    #[test]
    fn detects_alteration_removal_and_truncation() {
        let r1 = rec(1, &GENESIS);
        let h1 = unhex(r1.hash.as_deref().unwrap_or_default()).unwrap_or_default();
        let r2 = rec(2, &h1);
        let h2 = unhex(r2.hash.as_deref().unwrap_or_default()).unwrap_or_default();
        let r3 = rec(3, &h2);
        let ok = |rs: &[&AuditRecord]| {
            let mut v = Verifier::default();
            rs.iter().try_for_each(|r| v.push(r)).map(|()| v)
        };
        let v = ok(&[&r1, &r2, &r3]).unwrap_or_default();
        assert_eq!(v.verified, 3);
        assert!(v.finish(3, r3.hash.as_deref().unwrap_or_default()).is_ok());
        assert!(v.finish(4, "00").is_err(), "tail removed");
        let mut altered = r2.clone();
        altered.actor = "mallory".into();
        assert_eq!(ok(&[&r1, &altered, &r3]).map(|_| ()).unwrap_err().seq, 2);
        assert_eq!(ok(&[&r1, &r3]).map(|_| ()).unwrap_err().seq, 2, "gap");
        let mut relinked = r3.clone();
        relinked.prev_hash = Some(hex(&GENESIS));
        assert!(ok(&[&r1, &r2, &relinked]).is_err());
        assert_eq!(unhex(&hex(&h2)), Some(h2));
        assert!(unhex("zz").is_none());
    }
}
