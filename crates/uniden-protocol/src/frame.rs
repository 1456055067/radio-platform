//! Reassembly of received datagrams into protocol frames.
//!
//! The scanner replies with either a single carriage-return terminated line
//! (`MDL,SDS200\r`) or an XML reply introduced by `CMD,<XML>,\r` followed by an
//! XML document (BCDx36HP specification v1.03, `GLT`/`MSI` examples). Over UDP
//! a large XML document may be split across several datagrams, so the
//! [`FrameAssembler`] accumulates bytes until the XML root element is closed.
//!
//! Long lists are sent as a *sequence* of self-contained XML documents, each
//! carrying a `<Footer No="n" EOT="0|1"/>` element (some firmware spells it
//! `<Foot .../>`). `No` starts at 1 and increments by one; `EOT="1"` marks the
//! final part. A document without a footer is complete on its own. This
//! behaviour is community-reported from SDS200 hardware on firmware 1.23.15
//! (`sds200ha`, `sdsctl`, `noctule-scan`) and is not in the BCDx36HP
//! specification; see `docs/protocol-compliance.md`.
//!
//! Some replies arrive as a bare `CMD,<XML>,` header datagram followed by a
//! datagram that holds only the document; both shapes are handled.
//!
//! All limits are enforced before memory is committed.

use crate::error::DecodeError;

/// Resource limits for frame reassembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLimits {
    /// Maximum length of a single line frame.
    pub max_line_len: usize,
    /// Maximum number of comma-separated fields in a line frame.
    pub max_fields: usize,
    /// Maximum cumulative size of an XML reply (all parts).
    pub max_xml_len: usize,
    /// Maximum number of XML documents (footer parts) in one reply.
    pub max_xml_parts: usize,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            max_line_len: 4096,
            max_fields: 256,
            max_xml_len: 1024 * 1024,
            max_xml_parts: 256,
        }
    }
}

/// A single-line reply: `COMMAND[,field...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineFrame {
    /// Leading token (command name or bare error token such as `ERR`).
    pub command: String,
    /// Remaining comma-separated fields.
    pub fields: Vec<String>,
}

/// An XML reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlFrame {
    /// Command that introduced the reply (`GSI`, `GLT`, `PSI`, ...).
    pub command: String,
    /// One or more complete XML documents.
    pub documents: Vec<String>,
}

/// A complete reply frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Line reply.
    Line(LineFrame),
    /// XML reply.
    Xml(XmlFrame),
}

/// Bare tokens that the scanner may send instead of a command echo.
pub const BARE_ERROR_TOKENS: [&str; 4] = ["ERR", "NG", "FER", "ORER"];

impl Frame {
    /// The leading command token.
    #[must_use]
    pub fn command(&self) -> &str {
        match self {
            Self::Line(l) => &l.command,
            Self::Xml(x) => &x.command,
        }
    }

    /// True if this frame is a bare error token without a command echo
    /// (and therefore cannot be correlated by name).
    #[must_use]
    pub fn is_bare_error(&self) -> bool {
        matches!(self, Self::Line(l) if l.fields.is_empty()
            && BARE_ERROR_TOKENS.contains(&l.command.as_str()))
    }
}

#[derive(Debug)]
enum State {
    Idle,
    Xml {
        command: String,
        buf: Vec<u8>,
        documents: Vec<String>,
        total: usize,
        next_part: u32,
    },
}

/// Incremental frame reassembler. One instance per scanner connection.
#[derive(Debug)]
pub struct FrameAssembler {
    limits: FrameLimits,
    state: State,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self::new(FrameLimits::default())
    }
}

impl FrameAssembler {
    /// Creates an assembler with the given limits.
    #[must_use]
    pub fn new(limits: FrameLimits) -> Self {
        Self {
            limits,
            state: State::Idle,
        }
    }

    /// Discards any partially assembled reply.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    /// True if no partial XML reply is pending.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Feeds one received datagram and returns any frames it completed.
    ///
    /// In line mode a datagram boundary also terminates a line, because UDP
    /// preserves message boundaries.
    ///
    /// # Errors
    /// Returns an error (and resets the assembler) if the data is malformed
    /// or exceeds a limit. Frames completed earlier in the same datagram are
    /// discarded in that case.
    pub fn push(&mut self, datagram: &[u8]) -> Result<Vec<Frame>, DecodeError> {
        let result = self.push_inner(datagram);
        if result.is_err() {
            self.reset();
        }
        result
    }

    fn push_inner(&mut self, datagram: &[u8]) -> Result<Vec<Frame>, DecodeError> {
        let mut out = Vec::new();
        let mut rest: Vec<u8> = datagram.to_vec();
        loop {
            match &mut self.state {
                State::Idle => {
                    let start = rest
                        .iter()
                        .position(|b| !matches!(b, b'\r' | b'\n' | 0))
                        .unwrap_or(rest.len());
                    rest.drain(..start);
                    if rest.is_empty() {
                        return Ok(out);
                    }
                    if let Some((command, header_len)) = xml_header(&rest) {
                        rest.drain(..header_len);
                        self.state = State::Xml {
                            command,
                            buf: Vec::new(),
                            documents: Vec::new(),
                            total: 0,
                            next_part: 1,
                        };
                        continue;
                    }
                    let end = rest
                        .iter()
                        .position(|b| matches!(b, b'\r' | b'\n'))
                        .unwrap_or(rest.len());
                    if end > self.limits.max_line_len {
                        return Err(DecodeError::LimitExceeded {
                            what: "line frame",
                            limit: self.limits.max_line_len,
                        });
                    }
                    let line: Vec<u8> = rest.drain(..end).collect();
                    out.push(Frame::Line(parse_line(&line, self.limits.max_fields)?));
                }
                State::Xml {
                    command,
                    buf,
                    documents,
                    total,
                    next_part,
                } => {
                    // A continuation part may repeat the `CMD,<XML>,` header.
                    if buf.is_empty() && !documents.is_empty() {
                        let lead = rest
                            .iter()
                            .position(|b| !b.is_ascii_whitespace())
                            .unwrap_or(rest.len());
                        if let Some((cmd, len)) = xml_header(rest.get(lead..).unwrap_or_default()) {
                            if cmd != *command {
                                return Err(DecodeError::Malformed(format!(
                                    "XML continuation for `{cmd}` while assembling `{command}`"
                                )));
                            }
                            rest.drain(..lead + len);
                        }
                    }
                    *total += rest.len();
                    if *total > self.limits.max_xml_len {
                        return Err(DecodeError::LimitExceeded {
                            what: "XML reply",
                            limit: self.limits.max_xml_len,
                        });
                    }
                    buf.append(&mut rest);
                    let Some(end) = document_end(buf)? else {
                        return Ok(out);
                    };
                    let doc: Vec<u8> = buf.drain(..end).collect();
                    let doc = String::from_utf8_lossy(&doc).trim().to_owned();
                    let more = match footer(&doc) {
                        None => false,
                        Some(f) => {
                            if let Some(no) = f.number
                                && no != *next_part
                            {
                                return Err(DecodeError::Malformed(format!(
                                    "`{command}` XML part {no} received, expected {next_part}"
                                )));
                            }
                            *next_part = next_part.saturating_add(1);
                            !f.end_of_transmission
                        }
                    };
                    documents.push(doc);
                    if documents.len() > self.limits.max_xml_parts {
                        return Err(DecodeError::LimitExceeded {
                            what: "XML reply parts",
                            limit: self.limits.max_xml_parts,
                        });
                    }
                    rest = std::mem::take(buf);
                    if more {
                        // Keep assembling; leftover bytes are the next part.
                        let trimmed = rest.iter().all(u8::is_ascii_whitespace);
                        if trimmed {
                            return Ok(out);
                        }
                        continue;
                    }
                    let command = std::mem::take(command);
                    let documents = std::mem::take(documents);
                    out.push(Frame::Xml(XmlFrame { command, documents }));
                    self.state = State::Idle;
                }
            }
        }
    }
}

/// Recognises `CMD,<XML>,` at the start of `data`, returning the command and
/// header length (including one optional trailing CR/LF pair).
fn xml_header(data: &[u8]) -> Option<(String, usize)> {
    const MARK: &[u8] = b",<XML>,";
    let comma = data.iter().take(9).position(|b| *b == b',')?;
    let name = data.get(..comma)?;
    if name.is_empty() || !name.iter().all(u8::is_ascii_uppercase) {
        return None;
    }
    if data.get(comma..comma + MARK.len())? != MARK {
        return None;
    }
    let mut len = comma + MARK.len();
    while matches!(data.get(len), Some(b'\r' | b'\n')) {
        len += 1;
    }
    Some((String::from_utf8_lossy(name).into_owned(), len))
}

fn parse_line(line: &[u8], max_fields: usize) -> Result<LineFrame, DecodeError> {
    // Latin-1 mapping is lossless, preserving the scanner's non-ASCII font
    // codes in `STS` display text.
    let text: String = line.iter().map(|&b| char::from(b)).collect();
    let mut parts = text.split(',');
    let command = parts.next().unwrap_or_default().trim().to_owned();
    if command.is_empty()
        || command.len() > 8
        || !command
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(DecodeError::Malformed(format!(
            "invalid command token in `{}`",
            text.chars().take(32).collect::<String>()
        )));
    }
    let mut fields = Vec::new();
    for p in parts {
        if fields.len() >= max_fields {
            return Err(DecodeError::LimitExceeded {
                what: "line fields",
                limit: max_fields,
            });
        }
        fields.push(p.to_owned());
    }
    Ok(LineFrame { command, fields })
}

/// Finds the end offset of the first complete XML document in `buf`.
///
/// This is a quote-aware tag scanner, not a parser: it only tracks element
/// depth so that reassembly does not depend on a particular root name. DTDs
/// are rejected outright.
fn document_end(buf: &[u8]) -> Result<Option<usize>, DecodeError> {
    let mut i = 0usize;
    let mut depth: usize = 0;
    let mut seen_root = false;
    while i < buf.len() {
        let Some(rel) = buf.get(i..).and_then(|s| s.iter().position(|b| *b == b'<')) else {
            return Ok(None);
        };
        i += rel;
        let tail = buf.get(i..).unwrap_or_default();
        if tail.starts_with(b"<?") {
            match find(tail, b"?>") {
                Some(e) => i += e + 2,
                None => return Ok(None),
            }
            continue;
        }
        if tail.starts_with(b"<!--") {
            match find(tail, b"-->") {
                Some(e) => i += e + 3,
                None => return Ok(None),
            }
            continue;
        }
        if tail.starts_with(b"<![CDATA[") {
            match find(tail, b"]]>") {
                Some(e) => i += e + 3,
                None => return Ok(None),
            }
            continue;
        }
        if tail.starts_with(b"<!") {
            return Err(DecodeError::Xml(
                "DTD declarations are not permitted".into(),
            ));
        }
        // Element tag: find closing '>' outside quotes.
        let mut quote: Option<u8> = None;
        let mut end = None;
        for (k, &b) in tail.iter().enumerate().skip(1) {
            match quote {
                Some(q) if b == q => quote = None,
                Some(_) => {}
                None if b == b'"' || b == b'\'' => quote = Some(b),
                None if b == b'>' => {
                    end = Some(k);
                    break;
                }
                None => {}
            }
        }
        let Some(end) = end else {
            return Ok(None);
        };
        let closing = tail.get(1) == Some(&b'/');
        let self_closing = end > 0 && tail.get(end - 1) == Some(&b'/');
        if closing {
            depth = depth
                .checked_sub(1)
                .ok_or_else(|| DecodeError::Xml("closing tag without matching start tag".into()))?;
        } else if !self_closing {
            depth += 1;
            seen_root = true;
        } else {
            seen_root = true;
        }
        i += end + 1;
        if seen_root && depth == 0 {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Footer {
    number: Option<u32>,
    end_of_transmission: bool,
}

/// Extracts the last `<Footer .../>` or `<Foot .../>` element of a document.
fn footer(doc: &str) -> Option<Footer> {
    let pos = ["<Footer", "<Foot "]
        .iter()
        .filter_map(|t| doc.rfind(t))
        .max()?;
    let tag = doc.get(pos..).unwrap_or_default();
    let tag = tag.split_once('>').map_or(tag, |(t, _)| t);
    let number = attr_value(tag, "No").and_then(|v| v.trim().parse().ok());
    let end_of_transmission = attr_value(tag, "EOT").is_none_or(|v| v.trim() != "0");
    Some(Footer {
        number,
        end_of_transmission,
    })
}

fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let needle = format!(" {name}={quote}");
        if let Some(start) = tag.find(&needle) {
            let rest = tag.get(start + needle.len()..)?;
            return rest.split(quote).next();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(cmd: &str, fields: &[&str]) -> Frame {
        Frame::Line(LineFrame {
            command: cmd.into(),
            fields: fields.iter().map(|s| (*s).to_owned()).collect(),
        })
    }

    #[test]
    fn parses_single_line() {
        let mut a = FrameAssembler::default();
        assert_eq!(
            a.push(b"MDL,SDS200\r").unwrap(),
            vec![line("MDL", &["SDS200"])]
        );
        assert_eq!(
            a.push(b"VER,Version 1.23.04").unwrap(),
            vec![line("VER", &["Version 1.23.04"])]
        );
        assert_eq!(a.push(b"ERR\r").unwrap(), vec![line("ERR", &[])]);
        assert!(a.push(b"ERR\r").unwrap()[0].is_bare_error());
    }

    #[test]
    fn parses_multiple_lines_in_one_datagram() {
        let mut a = FrameAssembler::default();
        let frames = a.push(b"VOL,OK\rSQL,3\r").unwrap();
        assert_eq!(frames, vec![line("VOL", &["OK"]), line("SQL", &["3"])]);
    }

    const GLT: &str = "GLT,<XML>,\r<?xml version=\"1.0\" encoding=\"utf-8\"?>\r<GLT>\r\
        <FL Index=\"0\" Name=\"Favorites List 1\" Monitor=\"On\" Q_Key=\"1\" N_Tag=\"None\" />\r\
        </GLT>\r";

    #[test]
    fn assembles_xml_in_one_datagram() {
        let mut a = FrameAssembler::default();
        let frames = a.push(GLT.as_bytes()).unwrap();
        let [Frame::Xml(x)] = frames.as_slice() else {
            panic!("{frames:?}")
        };
        assert_eq!(x.command, "GLT");
        assert_eq!(x.documents.len(), 1);
        assert!(x.documents[0].ends_with("</GLT>"));
        assert!(a.is_idle());
    }

    #[test]
    fn assembles_xml_split_at_every_byte_after_header() {
        // UDP preserves datagram boundaries, so the `CMD,<XML>,` header is
        // never split; the document body may be split anywhere.
        let header = "GLT,<XML>,".len();
        for split in header..GLT.len() {
            let mut a = FrameAssembler::default();
            let (l, r) = GLT.as_bytes().split_at(split);
            let mut frames = a.push(l).unwrap();
            frames.extend(a.push(r).unwrap());
            assert_eq!(frames.len(), 1, "split at {split}: {frames:?}");
            assert!(matches!(&frames[0], Frame::Xml(_)));
        }
    }

    #[test]
    fn line_after_xml_in_same_datagram() {
        let mut a = FrameAssembler::default();
        let data = format!("{GLT}VOL,5\r");
        let frames = a.push(data.as_bytes()).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1], line("VOL", &["5"]));
    }

    #[test]
    fn footer_multipart() {
        let mut a = FrameAssembler::default();
        let p1 = "GSI,<XML>,\r<?xml version=\"1.0\"?><ScannerInfo Mode=\"a\"><Footer No=\"1\" EOT=\"0\"/></ScannerInfo>";
        let p2 = "GSI,<XML>,\r<?xml version=\"1.0\"?><ScannerInfo><Property VOL=\"3\"/><Footer No=\"2\" EOT=\"1\"/></ScannerInfo>";
        assert!(a.push(p1.as_bytes()).unwrap().is_empty());
        assert!(!a.is_idle());
        let frames = a.push(p2.as_bytes()).unwrap();
        let [Frame::Xml(x)] = frames.as_slice() else {
            panic!("{frames:?}")
        };
        assert_eq!(x.documents.len(), 2);
    }

    #[test]
    fn footer_sequence_gap_is_rejected() {
        let mut a = FrameAssembler::default();
        let p1 = "GLT,<XML>,\r<GLT><SYS Index=\"1\"/><Footer No=\"1\" EOT=\"0\"/></GLT>";
        let p3 = "GLT,<XML>,\r<GLT><SYS Index=\"2\"/><Foot No=\"3\" EOT=\"1\"/></GLT>";
        assert!(a.push(p1.as_bytes()).unwrap().is_empty());
        assert!(a.push(p3.as_bytes()).is_err());
        assert!(a.is_idle());
    }

    #[test]
    fn header_only_datagram_then_document() {
        let mut a = FrameAssembler::default();
        assert!(a.push(b"GSI,<XML>,\r\n").unwrap().is_empty());
        let frames = a
            .push(b"<?xml version=\"1.0\"?>\r<ScannerInfo Mode=\"x\"/>\r")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].command(), "GSI");
    }

    #[test]
    fn rejects_dtd_and_enforces_limits() {
        let mut a = FrameAssembler::default();
        let evil = "GSI,<XML>,\r<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY a \"b\">]><x/>";
        assert!(a.push(evil.as_bytes()).is_err());
        assert!(a.is_idle());

        let mut a = FrameAssembler::new(FrameLimits {
            max_xml_len: 64,
            ..FrameLimits::default()
        });
        assert!(a.push(b"GSI,<XML>,\r<ScannerInfo>").unwrap().is_empty());
        assert!(a.push(&[b' '; 100]).is_err());
        assert!(a.is_idle());

        let mut a = FrameAssembler::new(FrameLimits {
            max_line_len: 8,
            ..FrameLimits::default()
        });
        assert!(a.push(b"MDL,SDS200-LONG\r").is_err());
    }

    #[test]
    fn rejects_garbage_tokens() {
        let mut a = FrameAssembler::default();
        assert!(a.push(b"\xff\xfe,1\r").is_err());
        assert!(a.push(b"mdl,x\r").is_err());
        assert!(a.push(b"\r\n\0").unwrap().is_empty());
    }

    #[test]
    fn quoted_gt_does_not_end_tag() {
        let mut a = FrameAssembler::default();
        let frames = a
            .push(b"GSI,<XML>,\r<ScannerInfo Mode=\"a>b\"><X T='</ScannerInfo>'/></ScannerInfo>")
            .unwrap();
        assert_eq!(frames.len(), 1);
    }
}
