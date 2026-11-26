//! PCM → Opus. The scanner's audio is 8 kHz mono narrowband speech, so the
//! encoder runs at 8 kHz in VoIP mode with 20 ms frames.

use thiserror::Error;

/// Input sample rate.
pub const SAMPLE_RATE: u32 = 8000;
/// Samples per 20 ms frame at 8 kHz.
pub const FRAME_SAMPLES: usize = 160;
/// Frame duration in milliseconds.
pub const FRAME_MS: u32 = 20;
/// Opus RTP/Ogg clock: always 48 kHz (RFC 7587, RFC 7845).
pub const OPUS_CLOCK: u32 = 48_000;
/// 48 kHz samples per frame.
pub const FRAME_48K: u32 = 960;
/// Encoder bitrate (bits/s).
pub const BITRATE: i32 = 16_000;

/// Opus failures.
#[derive(Debug, Error)]
#[error("opus: {0}")]
pub struct OpusError(String);

/// A 20 ms Opus packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusFrame {
    /// Encoded bytes.
    pub data: Vec<u8>,
    /// True if this frame stands in for lost input (encoded silence).
    pub concealed: bool,
}

/// Buffers PCM and emits Opus frames.
pub struct Encoder {
    enc: opus::Encoder,
    pcm: Vec<i16>,
    lookahead: u16,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("buffered", &self.pcm.len())
            .finish()
    }
}

impl Encoder {
    /// Creates an encoder.
    ///
    /// # Errors
    /// libopus refused the configuration.
    pub fn new() -> Result<Self, OpusError> {
        let mut enc =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
                .map_err(|e| OpusError(e.to_string()))?;
        enc.set_bitrate(opus::Bitrate::Bits(BITRATE))
            .map_err(|e| OpusError(e.to_string()))?;
        let la = enc.get_lookahead().map_err(|e| OpusError(e.to_string()))?;
        // Lookahead is reported at the encoder rate; Ogg pre-skip is 48 kHz.
        let lookahead = u16::try_from(la.max(0).saturating_mul(6)).unwrap_or(312);
        Ok(Self {
            enc,
            pcm: Vec::with_capacity(FRAME_SAMPLES * 4),
            lookahead,
        })
    }

    /// Pre-skip for Ogg Opus (48 kHz samples).
    #[must_use]
    pub fn pre_skip(&self) -> u16 {
        self.lookahead
    }

    fn encode_one(&mut self, pcm: &[i16], concealed: bool) -> Result<OpusFrame, OpusError> {
        let mut out = [0u8; 1500];
        let n = self
            .enc
            .encode(pcm, &mut out)
            .map_err(|e| OpusError(e.to_string()))?;
        Ok(OpusFrame {
            data: out.get(..n).unwrap_or_default().to_vec(),
            concealed,
        })
    }

    /// Adds decoded PCM; returns any complete frames.
    ///
    /// # Errors
    /// Encoding failure.
    pub fn push(&mut self, pcm: &[i16]) -> Result<Vec<OpusFrame>, OpusError> {
        self.pcm.extend_from_slice(pcm);
        let mut out = Vec::new();
        while self.pcm.len() >= FRAME_SAMPLES {
            let frame: Vec<i16> = self.pcm.drain(..FRAME_SAMPLES).collect();
            out.push(self.encode_one(&frame, false)?);
        }
        Ok(out)
    }

    /// Stands in for `samples` of lost input with silence, keeping timing.
    ///
    /// # Errors
    /// Encoding failure.
    pub fn conceal(&mut self, samples: usize) -> Result<Vec<OpusFrame>, OpusError> {
        let mut frames = self.push(&vec![0; samples])?;
        for f in &mut frames {
            f.concealed = true;
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_decodes_back() {
        let mut e = Encoder::new().unwrap_or_else(|err| panic!("{err}"));
        // 1 kHz tone, 100 ms, pushed in uneven pieces.
        let tone: Vec<i16> = (0..800)
            .map(|i| {
                ((f64::from(i) * 2.0 * std::f64::consts::PI * 1000.0 / 8000.0).sin() * 8000.0)
                    as i16
            })
            .collect();
        let mut frames = e
            .push(tone.get(..123).unwrap_or_default())
            .unwrap_or_else(|err| panic!("{err}"));
        frames.extend(
            e.push(tone.get(123..).unwrap_or_default())
                .unwrap_or_else(|err| panic!("{err}")),
        );
        assert_eq!(frames.len(), 5);
        assert!(
            frames
                .iter()
                .all(|f| !f.data.is_empty() && f.data.len() < 200)
        );
        let mut d =
            opus::Decoder::new(48_000, opus::Channels::Mono).unwrap_or_else(|err| panic!("{err}"));
        let mut pcm = vec![0i16; 960];
        for f in &frames {
            let n = d
                .decode(&f.data, &mut pcm, false)
                .unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(n, 960, "20 ms at 48 kHz");
        }
        let energy: f64 = pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / 960.0;
        assert!(energy.sqrt() > 1000.0, "tone survives encoding");
        let c = e
            .conceal(FRAME_SAMPLES)
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(c.len() == 1 && c[0].concealed);
        assert!(e.pre_skip() > 0);
    }
}
