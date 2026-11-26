//! Ogg Opus file writer (RFC 7845) for recorded transmissions: one logical
//! stream, mono, one Opus packet per page.

const CRC_POLY: u32 = 0x04C1_1DB7;

#[allow(clippy::indexing_slicing)] // const evaluation: `i < 256` bounds the index
const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ CRC_POLY
            } else {
                r << 1
            };
            j += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
}

static CRC_TABLE: [u32; 256] = crc_table();

/// Ogg's CRC-32 (polynomial 0x04C11DB7, no reflection, zero init).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |crc, &b| {
        let idx = usize::from(((crc >> 24) as u8) ^ b);
        (crc << 8) ^ CRC_TABLE.get(idx).copied().unwrap_or(0)
    })
}

/// Builds an Ogg Opus file in memory.
#[derive(Debug)]
pub struct OggOpusWriter {
    out: Vec<u8>,
    serial: u32,
    seq: u32,
    granule: u64,
    pre_skip: u16,
    packets: u64,
}

impl OggOpusWriter {
    /// Starts a file: writes the `OpusHead` and `OpusTags` pages.
    #[must_use]
    pub fn new(
        serial: u32,
        pre_skip: u16,
        input_rate: u32,
        vendor: &str,
        comments: &[(&str, &str)],
    ) -> Self {
        let mut w = Self {
            out: Vec::new(),
            serial,
            seq: 0,
            granule: 0,
            pre_skip,
            packets: 0,
        };
        let mut head = b"OpusHead".to_vec();
        head.push(1); // version
        head.push(1); // channels
        head.extend_from_slice(&pre_skip.to_le_bytes());
        head.extend_from_slice(&input_rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // mapping family
        w.page(&head, 0, 0x02);
        let mut tags = b"OpusTags".to_vec();
        let len = |s: &str| u32::try_from(s.len()).unwrap_or(0).to_le_bytes();
        tags.extend_from_slice(&len(vendor));
        tags.extend_from_slice(vendor.as_bytes());
        tags.extend_from_slice(&u32::try_from(comments.len()).unwrap_or(0).to_le_bytes());
        for (k, v) in comments {
            let c = format!("{k}={v}");
            tags.extend_from_slice(&len(&c));
            tags.extend_from_slice(c.as_bytes());
        }
        w.page(&tags, 0, 0);
        w
    }

    fn page(&mut self, packet: &[u8], granule: u64, flags: u8) {
        let mut lacing = Vec::new();
        let mut rest = packet.len();
        while rest >= 255 {
            lacing.push(255u8);
            rest -= 255;
        }
        lacing.push(u8::try_from(rest).unwrap_or(0));
        let start = self.out.len();
        self.out.extend_from_slice(b"OggS");
        self.out.push(0);
        self.out.push(flags);
        self.out.extend_from_slice(&granule.to_le_bytes());
        self.out.extend_from_slice(&self.serial.to_le_bytes());
        self.out.extend_from_slice(&self.seq.to_le_bytes());
        let crc_at = self.out.len();
        self.out.extend_from_slice(&[0; 4]);
        self.out.push(u8::try_from(lacing.len()).unwrap_or(255));
        self.out.extend_from_slice(&lacing);
        self.out.extend_from_slice(packet);
        let crc = crc32(self.out.get(start..).unwrap_or_default());
        if let Some(slot) = self.out.get_mut(crc_at..crc_at + 4) {
            slot.copy_from_slice(&crc.to_le_bytes());
        }
        self.seq += 1;
    }

    /// Appends one 20 ms Opus packet (`samples_48k` = 960). Packets larger
    /// than one page's 255 lacing values (≈65 kB) are ignored.
    pub fn push(&mut self, packet: &[u8], samples_48k: u32) {
        if packet.len() >= 255 * 255 {
            return;
        }
        if self.packets == 0 {
            self.granule = u64::from(self.pre_skip);
        }
        self.granule += u64::from(samples_48k);
        self.packets += 1;
        self.page(packet, self.granule, 0);
    }

    /// Duration of the audio written so far, in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.granule.saturating_sub(u64::from(self.pre_skip)) / 48
    }

    /// Finishes the file (an empty end-of-stream page) and returns it.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        let g = self.granule;
        // An EOS page with no packet: one zero lacing value.
        self.page(&[], g, 0x04);
        self.out
    }
}

/// Why an Ogg Opus file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OggError {
    /// Not an Ogg page where one was expected.
    #[error("not an Ogg page at byte {0}")]
    Capture(usize),
    /// A page is cut short.
    #[error("truncated page at byte {0}")]
    Truncated(usize),
    /// A page checksum does not match.
    #[error("bad page checksum at byte {0}")]
    Checksum(usize),
    /// More than one logical stream, or an unsupported layout.
    #[error("unsupported Ogg layout: {0}")]
    Unsupported(&'static str),
    /// Missing or malformed `OpusHead`.
    #[error("not an Opus stream: {0}")]
    NotOpus(&'static str),
    /// Over the size limits.
    #[error("too large")]
    TooLarge,
}

/// The `OpusHead` fields that decoding needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusHead {
    /// Channels (1 or 2).
    pub channels: u8,
    /// Samples (at 48 kHz) to discard from the start.
    pub pre_skip: u16,
    /// Original input sample rate (informational).
    pub input_rate: u32,
}

/// The packets of a single-stream Ogg Opus file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OggOpus {
    /// Stream header.
    pub head: OpusHead,
    /// Audio packets in order (headers excluded).
    pub packets: Vec<Vec<u8>>,
    /// Final granule position (48 kHz samples, including pre-skip).
    pub granule: u64,
}

/// Most audio packets accepted (about 2 hours of 20 ms packets).
pub const MAX_PACKETS: usize = 360_000;

/// Reads a single-stream Ogg Opus file: checks every page checksum,
/// reassembles packets that span pages, and validates `OpusHead`.
/// `OpusTags` is skipped.
///
/// # Errors
/// [`OggError`].
pub fn read_ogg_opus(data: &[u8]) -> Result<OggOpus, OggError> {
    let mut at = 0usize;
    let mut serial = None;
    let mut partial: Vec<u8> = Vec::new();
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut granule = 0u64;
    let mut total = 0usize;
    while at < data.len() {
        let hdr = data.get(at..at + 27).ok_or(OggError::Truncated(at))?;
        if hdr.get(..4) != Some(&b"OggS"[..]) {
            return Err(OggError::Capture(at));
        }
        if hdr.get(4) != Some(&0) {
            return Err(OggError::Unsupported("Ogg version"));
        }
        let flags = hdr.get(5).copied().unwrap_or(0);
        let le64 = |r: std::ops::Range<usize>| {
            hdr.get(r)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map(u64::from_le_bytes)
        };
        let le32 = |r: std::ops::Range<usize>| {
            hdr.get(r)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map(u32::from_le_bytes)
        };
        let page_granule = le64(6..14).ok_or(OggError::Truncated(at))?;
        let page_serial = le32(14..18).ok_or(OggError::Truncated(at))?;
        let stored_crc = le32(22..26).ok_or(OggError::Truncated(at))?;
        let nseg = usize::from(hdr.get(26).copied().unwrap_or(0));
        let lacing = data
            .get(at + 27..at + 27 + nseg)
            .ok_or(OggError::Truncated(at))?;
        let body_len: usize = lacing.iter().map(|&b| usize::from(b)).sum();
        let page_len = 27 + nseg + body_len;
        let page = data.get(at..at + page_len).ok_or(OggError::Truncated(at))?;
        let mut check = page.to_vec();
        if let Some(c) = check.get_mut(22..26) {
            c.copy_from_slice(&[0; 4]);
        }
        if crc32(&check) != stored_crc {
            return Err(OggError::Checksum(at));
        }
        match serial {
            None => serial = Some(page_serial),
            Some(s) if s != page_serial => {
                return Err(OggError::Unsupported("multiple logical streams"));
            }
            Some(_) => {}
        }
        if flags & 0x01 == 0 && !partial.is_empty() {
            return Err(OggError::Unsupported("lost packet continuation"));
        }
        let mut body = page.get(27 + nseg..).unwrap_or_default();
        for &l in lacing {
            let n = usize::from(l);
            let (chunk, rest) = body.split_at(n.min(body.len()));
            partial.extend_from_slice(chunk);
            body = rest;
            total += n;
            if total > 64 * 1024 * 1024 {
                return Err(OggError::TooLarge);
            }
            if l < 255 {
                packets.push(std::mem::take(&mut partial));
                if packets.len() > MAX_PACKETS + 2 {
                    return Err(OggError::TooLarge);
                }
            }
        }
        if page_granule != u64::MAX {
            granule = page_granule;
        }
        at += page_len;
    }
    let mut it = packets.into_iter();
    let head = it.next().ok_or(OggError::NotOpus("empty"))?;
    if head.get(..8) != Some(&b"OpusHead"[..]) || head.len() < 19 {
        return Err(OggError::NotOpus("missing OpusHead"));
    }
    if head.get(8).is_none_or(|&v| v >> 4 != 0) {
        return Err(OggError::NotOpus("unsupported OpusHead version"));
    }
    let channels = head.get(9).copied().unwrap_or(0);
    if !(1..=2).contains(&channels) || head.get(18) != Some(&0) {
        return Err(OggError::Unsupported(
            "only mono or stereo, mapping family 0",
        ));
    }
    let pre_skip = head
        .get(10..12)
        .and_then(|b| <[u8; 2]>::try_from(b).ok())
        .map_or(0, u16::from_le_bytes);
    let input_rate = head
        .get(12..16)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes);
    let tags = it.next().ok_or(OggError::NotOpus("missing OpusTags"))?;
    if tags.get(..8) != Some(&b"OpusTags"[..]) {
        return Err(OggError::NotOpus("missing OpusTags"));
    }
    // A trailing empty packet is the writer's end-of-stream marker.
    let packets: Vec<Vec<u8>> = it.filter(|p| !p.is_empty()).collect();
    Ok(OggOpus {
        head: OpusHead {
            channels,
            pre_skip,
            input_rate,
        },
        packets,
        granule,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vector() {
        // CRC of "OggS" header bytes is deterministic; check a classic vector.
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0x89A1_897F);
    }

    #[test]
    fn writes_valid_pages() {
        let mut w = OggOpusWriter::new(42, 312, 8000, "radio-platform", &[("SCANNER", "x")]);
        w.push(&[0xF8; 40], 960);
        w.push(&vec![1; 300], 960);
        assert_eq!(w.duration_ms(), 40);
        let f = w.finish();
        // Walk pages and verify each CRC.
        let mut at = 0;
        let mut pages = 0;
        while at < f.len() {
            assert_eq!(f.get(at..at + 4), Some(&b"OggS"[..]));
            let nseg = usize::from(f[at + 26]);
            let body: usize = f[at + 27..at + 27 + nseg]
                .iter()
                .map(|&b| usize::from(b))
                .sum();
            let len = 27 + nseg + body;
            let mut page = f[at..at + len].to_vec();
            let stored = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
            page[22..26].copy_from_slice(&[0; 4]);
            assert_eq!(crc32(&page), stored, "page {pages} CRC");
            at += len;
            pages += 1;
        }
        assert_eq!(pages, 5, "head, tags, 2 audio, EOS");
        assert_eq!(&f[28..36], b"OpusHead");
    }

    #[test]
    fn reads_back_what_it_writes() {
        let mut w = OggOpusWriter::new(7, 312, 8000, "radio-platform", &[("A", "b")]);
        w.push(&[0xF8; 40], 960);
        w.push(&vec![2; 600], 960); // spans several lacing values
        let f = w.finish();
        let r = read_ogg_opus(&f).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            r.head,
            OpusHead {
                channels: 1,
                pre_skip: 312,
                input_rate: 8000
            }
        );
        assert_eq!(r.packets, vec![vec![0xF8; 40], vec![2; 600]]);
        assert_eq!(r.granule, 312 + 1920);
        // Corruption is detected.
        let mut bad = f.clone();
        if let Some(b) = bad.get_mut(100) {
            *b ^= 0xFF;
        }
        assert!(read_ogg_opus(&bad).is_err());
        assert!(read_ogg_opus(&f[..f.len() - 3]).is_err());
        assert!(read_ogg_opus(b"not ogg").is_err());
        assert!(read_ogg_opus(&[]).is_err());
    }
}
