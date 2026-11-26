//! RTP (RFC 3550) packet parsing. Every length is bounds-checked; malformed
//! packets are rejected, never trusted.

use thiserror::Error;

/// RTP parse failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RtpError {
    /// Shorter than the fixed header, CSRC list or extension.
    #[error("truncated RTP packet")]
    Truncated,
    /// Version is not 2.
    #[error("unsupported RTP version {0}")]
    Version(u8),
    /// Padding length is zero or exceeds the payload.
    #[error("invalid RTP padding")]
    Padding,
}

/// A parsed RTP packet borrowing its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    /// Marker bit.
    pub marker: bool,
    /// Payload type.
    pub payload_type: u8,
    /// Sequence number.
    pub sequence: u16,
    /// Media timestamp.
    pub timestamp: u32,
    /// Synchronisation source.
    pub ssrc: u32,
    /// Payload (padding removed).
    pub payload: &'a [u8],
}

fn be16(b: &[u8], at: usize) -> Result<u16, RtpError> {
    let s: [u8; 2] = b
        .get(at..at + 2)
        .and_then(|s| s.try_into().ok())
        .ok_or(RtpError::Truncated)?;
    Ok(u16::from_be_bytes(s))
}

fn be32(b: &[u8], at: usize) -> Result<u32, RtpError> {
    let s: [u8; 4] = b
        .get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or(RtpError::Truncated)?;
    Ok(u32::from_be_bytes(s))
}

impl<'a> RtpPacket<'a> {
    /// Parses a datagram.
    ///
    /// # Errors
    /// [`RtpError`] for malformed packets.
    #[allow(clippy::indexing_slicing)] // every index is preceded by a length check
    pub fn parse(b: &'a [u8]) -> Result<Self, RtpError> {
        if b.len() < 12 {
            return Err(RtpError::Truncated);
        }
        let version = b[0] >> 6;
        if version != 2 {
            return Err(RtpError::Version(version));
        }
        let padding = b[0] & 0x20 != 0;
        let extension = b[0] & 0x10 != 0;
        let csrc = usize::from(b[0] & 0x0F);
        let marker = b[1] & 0x80 != 0;
        let payload_type = b[1] & 0x7F;
        let sequence = be16(b, 2)?;
        let timestamp = be32(b, 4)?;
        let ssrc = be32(b, 8)?;
        let mut start = 12 + 4 * csrc;
        if b.len() < start {
            return Err(RtpError::Truncated);
        }
        if extension {
            let words = usize::from(be16(b, start + 2)?);
            start += 4 + 4 * words;
            if b.len() < start {
                return Err(RtpError::Truncated);
            }
        }
        let mut end = b.len();
        if padding {
            let pad = usize::from(b[end - 1]);
            if pad == 0 || pad > end - start {
                return Err(RtpError::Padding);
            }
            end -= pad;
        }
        Ok(Self {
            marker,
            payload_type,
            sequence,
            timestamp,
            ssrc,
            payload: b.get(start..end).ok_or(RtpError::Truncated)?,
        })
    }
}

/// Builds an RTP packet (simulator and tests).
#[must_use]
pub fn build(
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    marker: bool,
    payload: &[u8],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + payload.len());
    v.push(0x80);
    v.push((u8::from(marker) << 7) | (payload_type & 0x7F));
    v.extend_from_slice(&sequence.to_be_bytes());
    v.extend_from_slice(&timestamp.to_be_bytes());
    v.extend_from_slice(&ssrc.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_packet() {
        let p = build(0, 7, 160, 0xdead_beef, true, &[1, 2, 3]);
        let r = RtpPacket::parse(&p).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (r.payload_type, r.sequence, r.timestamp, r.ssrc, r.marker),
            (0, 7, 160, 0xdead_beef, true)
        );
        assert_eq!(r.payload, &[1, 2, 3]);
    }

    #[test]
    fn handles_csrc_extension_and_padding() {
        let mut p = vec![0x80 | 0x20 | 0x10 | 0x01, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        p.extend_from_slice(&[9, 9, 9, 9]); // CSRC
        p.extend_from_slice(&[0xBE, 0xDE, 0, 1, 1, 2, 3, 4]); // 1-word extension
        p.extend_from_slice(&[5, 6]); // payload
        p.extend_from_slice(&[0, 2]); // 2 bytes padding
        let r = RtpPacket::parse(&p).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.payload, &[5, 6]);
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(RtpPacket::parse(&[0x80; 5]), Err(RtpError::Truncated));
        assert_eq!(
            RtpPacket::parse(&[0x40; 12]).map(|_| ()),
            Err(RtpError::Version(1))
        );
        let mut p = build(0, 1, 1, 1, false, &[1]);
        p[0] |= 0x0F; // 15 CSRCs that are not there
        assert_eq!(RtpPacket::parse(&p), Err(RtpError::Truncated));
        let mut p = build(0, 1, 1, 1, false, &[1, 200]);
        p[0] |= 0x20; // padding 200 > payload
        assert_eq!(RtpPacket::parse(&p), Err(RtpError::Padding));
    }
}
