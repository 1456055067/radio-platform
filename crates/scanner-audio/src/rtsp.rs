//! Sans-I/O RTSP/1.0 (RFC 2326) client messages for the SDS200 audio server.
//!
//! The scanner's audio server is reported to be fragile (see
//! `docs/protocol-compliance.md`): one client at a time, and a missing
//! `TEARDOWN` or an out-of-order request can lock it until a power cycle.
//! [`RtspSession`] therefore only produces requests in the order
//! DESCRIBE → SETUP → PLAY → (GET_PARAMETER keepalives) → TEARDOWN, and
//! refuses anything else. Response parsing is bounded.

use thiserror::Error;

/// Maximum header block size accepted from the server.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Maximum body (SDP) size accepted from the server.
pub const MAX_BODY_BYTES: usize = 16 * 1024;
/// Maximum number of response headers.
pub const MAX_HEADERS: usize = 64;

/// RTSP failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RtspError {
    /// The response is malformed.
    #[error("malformed RTSP response: {0}")]
    Malformed(&'static str),
    /// The response exceeds the size limits.
    #[error("RTSP response too large")]
    TooLarge,
    /// The server answered with a non-2xx status.
    #[error("RTSP {method} failed: {status} {reason}")]
    Status {
        /// Request method.
        method: &'static str,
        /// Status code.
        status: u16,
        /// Reason phrase.
        reason: String,
    },
    /// The response's CSeq does not match the request.
    #[error("RTSP CSeq mismatch")]
    CSeq,
    /// The SDP does not offer PCMU audio.
    #[error("unsupported SDP: {0}")]
    Sdp(String),
    /// A request was attempted out of order.
    #[error("RTSP request out of order: {0}")]
    Order(&'static str),
}

/// A parsed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Reason phrase.
    pub reason: String,
    /// Headers in order (names as received).
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
}

impl Response {
    /// First header named `name` (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Parses one response from the start of `buf`.
///
/// Returns `Ok(None)` if more bytes are needed, or the response and the
/// number of bytes consumed.
///
/// # Errors
/// [`RtspError::Malformed`] or [`RtspError::TooLarge`].
pub fn parse_response(buf: &[u8]) -> Result<Option<(Response, usize)>, RtspError> {
    let Some(end) = find(buf, b"\r\n\r\n") else {
        return if buf.len() > MAX_HEADER_BYTES {
            Err(RtspError::TooLarge)
        } else {
            Ok(None)
        };
    };
    if end > MAX_HEADER_BYTES {
        return Err(RtspError::TooLarge);
    }
    let head = std::str::from_utf8(buf.get(..end).ok_or(RtspError::Malformed("head"))?)
        .map_err(|_| RtspError::Malformed("non-UTF-8 header"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(RtspError::Malformed("empty"))?;
    let mut parts = status_line.splitn(3, ' ');
    if parts.next() != Some("RTSP/1.0") {
        return Err(RtspError::Malformed("status line"));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|s| (100..600).contains(s))
        .ok_or(RtspError::Malformed("status code"))?;
    let reason = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    for line in lines {
        if headers.len() >= MAX_HEADERS {
            return Err(RtspError::TooLarge);
        }
        let (k, v) = line.split_once(':').ok_or(RtspError::Malformed("header"))?;
        if k.is_empty() || k.contains(' ') {
            return Err(RtspError::Malformed("header name"));
        }
        headers.push((k.to_owned(), v.trim().to_owned()));
    }
    let len: usize = match headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
    {
        Some((_, v)) => v
            .parse()
            .map_err(|_| RtspError::Malformed("Content-Length"))?,
        None => 0,
    };
    if len > MAX_BODY_BYTES {
        return Err(RtspError::TooLarge);
    }
    let body_start = end + 4;
    let Some(body) = buf.get(body_start..body_start + len) else {
        return Ok(None);
    };
    Ok(Some((
        Response {
            status,
            reason,
            headers,
            body: body.to_vec(),
        },
        body_start + len,
    )))
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// What the SDP offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    /// Control URL for SETUP (absolute).
    pub control: String,
    /// RTP payload type (0 = PCMU).
    pub payload_type: u8,
}

/// Extracts the PCMU audio track from an SDP body.
///
/// The SDS200 is reported to send `m=audio 0 RTP/AVP 0` with
/// `a=control:trackID=1` and no `a=rtpmap`, so static payload type 0 is
/// accepted without an rtpmap.
///
/// # Errors
/// [`RtspError::Sdp`] if there is no PCMU audio track.
pub fn audio_track(sdp: &str, base_url: &str) -> Result<AudioTrack, RtspError> {
    let mut in_audio = false;
    let mut pt = None;
    let mut control = None;
    for line in sdp.lines().map(str::trim) {
        if let Some(m) = line.strip_prefix("m=") {
            if pt.is_some() {
                break;
            }
            let f: Vec<&str> = m.split_whitespace().collect();
            in_audio = f.first() == Some(&"audio");
            if in_audio {
                pt = f
                    .get(3..)
                    .unwrap_or(&[])
                    .iter()
                    .find_map(|p| p.parse::<u8>().ok().filter(|&p| p == 0));
                if pt.is_none() {
                    return Err(RtspError::Sdp(format!(
                        "audio formats {:?} lack PCMU (0)",
                        f.get(3..)
                    )));
                }
            }
        } else if in_audio && let Some(c) = line.strip_prefix("a=control:") {
            control = Some(c.trim().to_owned());
        }
    }
    let payload_type = pt.ok_or_else(|| RtspError::Sdp("no audio track".into()))?;
    let control = match control.as_deref() {
        None | Some("*") => base_url.to_owned(),
        Some(c) if c.starts_with("rtsp://") => {
            // Only allow a control URL on the same host as the base URL.
            let host = |u: &str| {
                u.trim_start_matches("rtsp://")
                    .split('/')
                    .next()
                    .map(str::to_owned)
            };
            if host(c) != host(base_url) {
                return Err(RtspError::Sdp("control URL points to another host".into()));
            }
            c.to_owned()
        }
        Some(c) => format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            c.trim_start_matches('/')
        ),
    };
    if control.contains(['\r', '\n', ' ']) {
        return Err(RtspError::Sdp("invalid control URL".into()));
    }
    Ok(AudioTrack {
        control,
        payload_type,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Describe,
    Setup,
    Play,
    Playing,
    TornDown,
}

/// The session's request sequence.
#[derive(Debug)]
pub struct RtspSession {
    url: String,
    user_agent: String,
    cseq: u32,
    step: Step,
    track: Option<AudioTrack>,
    session: Option<String>,
    timeout_secs: u32,
    client_port: u16,
    server_port: Option<u16>,
}

/// A request to send, with its CSeq for matching the response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Method.
    pub method: &'static str,
    /// CSeq.
    pub cseq: u32,
    /// Wire bytes.
    pub bytes: Vec<u8>,
}

impl RtspSession {
    /// Starts a session for `url`, receiving RTP on `client_port`.
    #[must_use]
    pub fn new(url: &str, client_port: u16) -> Self {
        Self {
            url: url.to_owned(),
            user_agent: concat!("radio-platform/", env!("CARGO_PKG_VERSION")).to_owned(),
            cseq: 0,
            step: Step::Describe,
            track: None,
            session: None,
            timeout_secs: 60,
            client_port,
            server_port: None,
        }
    }

    fn request(&mut self, method: &'static str, url: &str, extra: &[(&str, String)]) -> Request {
        self.cseq += 1;
        let mut s = format!(
            "{method} {url} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: {}\r\n",
            self.cseq, self.user_agent
        );
        if let Some(id) = &self.session {
            s.push_str(&format!("Session: {id}\r\n"));
        }
        for (k, v) in extra {
            s.push_str(&format!("{k}: {v}\r\n"));
        }
        s.push_str("\r\n");
        Request {
            method,
            cseq: self.cseq,
            bytes: s.into_bytes(),
        }
    }

    fn check(&self, req: &Request, resp: &Response) -> Result<(), RtspError> {
        if resp.header("CSeq").and_then(|c| c.parse::<u32>().ok()) != Some(req.cseq) {
            return Err(RtspError::CSeq);
        }
        if !(200..300).contains(&resp.status) {
            return Err(RtspError::Status {
                method: req.method,
                status: resp.status,
                reason: resp.reason.clone(),
            });
        }
        Ok(())
    }

    /// The DESCRIBE request.
    ///
    /// # Errors
    /// [`RtspError::Order`] if not first.
    pub fn describe(&mut self) -> Result<Request, RtspError> {
        if self.step != Step::Describe {
            return Err(RtspError::Order("DESCRIBE"));
        }
        let url = self.url.clone();
        Ok(self.request("DESCRIBE", &url, &[("Accept", "application/sdp".into())]))
    }

    /// Handles the DESCRIBE response.
    ///
    /// # Errors
    /// Status, CSeq or SDP problems.
    pub fn on_describe(
        &mut self,
        req: &Request,
        resp: &Response,
    ) -> Result<&AudioTrack, RtspError> {
        self.check(req, resp)?;
        let base = resp
            .header("Content-Base")
            .map_or_else(|| self.url.clone(), |b| b.trim_end_matches('/').to_owned());
        let sdp =
            std::str::from_utf8(&resp.body).map_err(|_| RtspError::Sdp("non-UTF-8 SDP".into()))?;
        let track = audio_track(sdp, &base)?;
        self.step = Step::Setup;
        Ok(self.track.insert(track))
    }

    /// The SETUP request. A single `client_port` is sent, as reported to
    /// work with the SDS200 (RTCP is not used).
    ///
    /// # Errors
    /// [`RtspError::Order`].
    pub fn setup(&mut self) -> Result<Request, RtspError> {
        let (Step::Setup, Some(track)) = (self.step, self.track.clone()) else {
            return Err(RtspError::Order("SETUP"));
        };
        let transport = format!("RTP/AVP;unicast;client_port={}", self.client_port);
        Ok(self.request("SETUP", &track.control, &[("Transport", transport)]))
    }

    /// Handles the SETUP response (session id, timeout, server port).
    ///
    /// # Errors
    /// Status, CSeq or a missing session.
    pub fn on_setup(&mut self, req: &Request, resp: &Response) -> Result<(), RtspError> {
        self.check(req, resp)?;
        let s = resp
            .header("Session")
            .ok_or(RtspError::Malformed("no Session header"))?;
        let mut parts = s.split(';');
        let id = parts.next().unwrap_or("").trim();
        if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(RtspError::Malformed("session id"));
        }
        for p in parts {
            if let Some(t) = p.trim().strip_prefix("timeout=")
                && let Ok(t) = t.parse::<u32>()
            {
                self.timeout_secs = t.clamp(10, 3600);
            }
        }
        if let Some(t) = resp.header("Transport") {
            self.server_port = t
                .split(';')
                .find_map(|p| p.trim().strip_prefix("server_port="))
                .and_then(|p| p.split('-').next())
                .and_then(|p| p.parse().ok());
        }
        self.session = Some(id.to_owned());
        self.step = Step::Play;
        Ok(())
    }

    /// The PLAY request.
    ///
    /// # Errors
    /// [`RtspError::Order`].
    pub fn play(&mut self) -> Result<Request, RtspError> {
        if self.step != Step::Play {
            return Err(RtspError::Order("PLAY"));
        }
        let url = self.url.clone();
        Ok(self.request("PLAY", &url, &[("Range", "npt=0.000-".into())]))
    }

    /// Handles the PLAY response.
    ///
    /// # Errors
    /// Status or CSeq.
    pub fn on_play(&mut self, req: &Request, resp: &Response) -> Result<(), RtspError> {
        self.check(req, resp)?;
        self.step = Step::Playing;
        Ok(())
    }

    /// A `GET_PARAMETER` keepalive (only while playing).
    ///
    /// # Errors
    /// [`RtspError::Order`].
    pub fn keepalive(&mut self) -> Result<Request, RtspError> {
        if self.step != Step::Playing {
            return Err(RtspError::Order("GET_PARAMETER"));
        }
        let url = self.url.clone();
        Ok(self.request("GET_PARAMETER", &url, &[]))
    }

    /// Checks a keepalive response.
    ///
    /// # Errors
    /// Status or CSeq.
    pub fn on_keepalive(&mut self, req: &Request, resp: &Response) -> Result<(), RtspError> {
        self.check(req, resp)
    }

    /// The TEARDOWN request, if a session was set up and not yet torn down.
    pub fn teardown(&mut self) -> Option<Request> {
        if self.session.is_none() || self.step == Step::TornDown {
            return None;
        }
        self.step = Step::TornDown;
        let url = self.url.clone();
        Some(self.request("TEARDOWN", &url, &[]))
    }

    /// Keepalive interval: under half the server's session timeout.
    #[must_use]
    pub fn keepalive_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from((self.timeout_secs / 2).clamp(5, 25)))
    }

    /// Server RTP port from SETUP, if given.
    #[must_use]
    pub fn server_port(&self) -> Option<u16> {
        self.server_port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(s: &str) -> Response {
        parse_response(s.as_bytes())
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|| panic!("incomplete"))
            .0
    }

    const SDP: &str = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Scanner Audio\r\nm=audio 0 RTP/AVP 0\r\na=control:trackID=1\r\n";

    #[test]
    fn full_session_in_order() {
        let url = "rtsp://192.168.50.10/au:scanner.au";
        let mut s = RtspSession::new(url, 40000);
        assert!(s.setup().is_err() && s.play().is_err() && s.keepalive().is_err());
        assert!(s.teardown().is_none(), "nothing to tear down before SETUP");
        let d = s.describe().unwrap_or_else(|e| panic!("{e}"));
        assert!(
            String::from_utf8_lossy(&d.bytes)
                .starts_with("DESCRIBE rtsp://192.168.50.10/au:scanner.au RTSP/1.0\r\nCSeq: 1\r\n")
        );
        let r = resp(&format!(
            "RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{SDP}",
            SDP.len()
        ));
        let t = s.on_describe(&d, &r).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(t.control, "rtsp://192.168.50.10/au:scanner.au/trackID=1");
        let su = s.setup().unwrap_or_else(|e| panic!("{e}"));
        assert!(
            String::from_utf8_lossy(&su.bytes)
                .contains("Transport: RTP/AVP;unicast;client_port=40000\r\n")
        );
        s.on_setup(&su, &resp("RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: 12345678;timeout=60\r\nTransport: RTP/AVP;unicast;client_port=40000;server_port=6970\r\n\r\n"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.server_port(), Some(6970));
        assert_eq!(s.keepalive_interval(), std::time::Duration::from_secs(25));
        let p = s.play().unwrap_or_else(|e| panic!("{e}"));
        assert!(String::from_utf8_lossy(&p.bytes).contains("Session: 12345678\r\n"));
        s.on_play(&p, &resp("RTSP/1.0 200 OK\r\nCSeq: 3\r\n\r\n"))
            .unwrap_or_else(|e| panic!("{e}"));
        let k = s.keepalive().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(k.method, "GET_PARAMETER");
        let t = s.teardown().unwrap_or_else(|| panic!("teardown"));
        assert_eq!(t.cseq, 5);
        assert!(s.teardown().is_none(), "TEARDOWN once");
        assert!(s.keepalive().is_err());
    }

    #[test]
    fn rejects_bad_responses() {
        let mut s = RtspSession::new("rtsp://h/au:scanner.au", 1);
        let d = s.describe().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            s.on_describe(&d, &resp("RTSP/1.0 200 OK\r\nCSeq: 9\r\n\r\n"))
                .err(),
            Some(RtspError::CSeq)
        );
        assert!(matches!(
            s.on_describe(
                &d,
                &resp("RTSP/1.0 453 Not Enough Bandwidth\r\nCSeq: 1\r\n\r\n")
            ),
            Err(RtspError::Status { status: 453, .. })
        ));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n\r\n").is_err());
        assert!(
            parse_response(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 999999\r\n\r\n")
                .is_err()
        );
        assert_eq!(
            parse_response(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n"),
            Ok(None),
            "incomplete"
        );
        assert_eq!(
            parse_response(b"RTSP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nab"),
            Ok(None)
        );
        assert_eq!(
            parse_response(&vec![b'a'; MAX_HEADER_BYTES + 1]),
            Err(RtspError::TooLarge)
        );
    }

    #[test]
    fn sdp_handling() {
        assert!(
            audio_track("m=audio 0 RTP/AVP 8\r\n", "rtsp://h/x").is_err(),
            "PCMA only"
        );
        assert!(audio_track("m=video 0 RTP/AVP 96\r\n", "rtsp://h/x").is_err());
        assert_eq!(
            audio_track("m=audio 0 RTP/AVP 0\r\n", "rtsp://h/x").map(|t| t.control),
            Ok("rtsp://h/x".into())
        );
        assert!(
            audio_track(
                "m=audio 0 RTP/AVP 0\r\na=control:rtsp://evil/x\r\n",
                "rtsp://h/x"
            )
            .is_err(),
            "foreign host"
        );
        assert!(audio_track("m=audio 0 RTP/AVP 0\r\na=control:a b\r\n", "rtsp://h/x").is_err());
    }
}
