//! Recording decode (Ogg Opus → 16 kHz mono PCM) and WAV encoding.

use scanner_audio::ogg::{OggError, read_ogg_opus};
use thiserror::Error;

/// Output sample rate (what Whisper models expect).
pub const SAMPLE_RATE: u32 = 16_000;

/// 48 kHz granule samples per output sample.
const RATIO: u64 = 48_000 / SAMPLE_RATE as u64;

/// Longest output per Opus packet (120 ms at 16 kHz).
const MAX_FRAME: usize = 1920;

/// Why a recording could not be decoded.
#[derive(Debug, Error)]
pub enum AudioError {
    /// Container problem.
    #[error("recording container: {0}")]
    Container(#[from] OggError),
    /// Codec problem.
    #[error("recording codec: {0}")]
    Codec(String),
    /// Longer than allowed.
    #[error("recording longer than {0} s")]
    TooLong(u32),
}

/// Decodes an Ogg Opus recording to 16 kHz mono PCM, trimming the encoder
/// pre-skip and any padding past the final granule position. Stereo is
/// mixed down.
///
/// # Errors
/// [`AudioError`].
pub fn decode_ogg_opus(data: &[u8], max_secs: u32) -> Result<Vec<i16>, AudioError> {
    let ogg = read_ogg_opus(data)?;
    let stereo = ogg.head.channels == 2;
    let channels = if stereo {
        opus::Channels::Stereo
    } else {
        opus::Channels::Mono
    };
    let mut dec =
        opus::Decoder::new(SAMPLE_RATE, channels).map_err(|e| AudioError::Codec(e.to_string()))?;
    let limit = usize::try_from(u64::from(max_secs) * u64::from(SAMPLE_RATE)).unwrap_or(usize::MAX);
    let mut pcm: Vec<i16> = Vec::new();
    let mut buf = vec![0i16; MAX_FRAME * 2];
    for p in &ogg.packets {
        let n = dec
            .decode(p, &mut buf, false)
            .map_err(|e| AudioError::Codec(e.to_string()))?;
        if stereo {
            pcm.extend(
                buf.as_chunks::<2>()
                    .0
                    .iter()
                    .take(n)
                    .map(|[l, r]| i16::try_from((i32::from(*l) + i32::from(*r)) / 2).unwrap_or(0)),
            );
        } else {
            pcm.extend_from_slice(buf.get(..n).unwrap_or_default());
        }
        if pcm.len() > limit + MAX_FRAME {
            return Err(AudioError::TooLong(max_secs));
        }
    }
    let skip = usize::try_from(u64::from(ogg.head.pre_skip) / RATIO).unwrap_or(0);
    let total = ogg
        .granule
        .saturating_sub(u64::from(ogg.head.pre_skip))
        .checked_div(RATIO)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(usize::MAX);
    let end = skip.saturating_add(total).min(pcm.len());
    Ok(pcm.get(skip.min(end)..end).unwrap_or_default().to_vec())
}

/// Writes 16-bit mono PCM as a WAV file.
#[must_use]
pub fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = u32::try_from(pcm.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&data_len.saturating_add(36).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use scanner_audio::ogg::OggOpusWriter;

    /// A tone encoded exactly as the agent does (8 kHz VoIP Opus).
    fn tone_ogg(hz: f64, ms: usize) -> Vec<u8> {
        let mut enc = scanner_audio::opus_enc::Encoder::new().unwrap_or_else(|e| panic!("{e}"));
        let mut w = OggOpusWriter::new(1, enc.pre_skip(), 8000, "test", &[]);
        let n = ms * 8;
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let pcm: Vec<i16> = (0..n)
            .map(|i| ((i as f64 * hz * std::f64::consts::TAU / 8000.0).sin() * 12_000.0) as i16)
            .collect();
        for f in enc.push(&pcm).unwrap_or_else(|e| panic!("{e}")) {
            w.push(&f.data, scanner_audio::FRAME_48K);
        }
        w.finish()
    }

    #[test]
    fn decodes_to_16k_with_the_right_length_and_tone() {
        let pcm = decode_ogg_opus(&tone_ogg(700.0, 1000), 60).unwrap_or_else(|e| panic!("{e}"));
        // 1 s of audio minus the pre-skip trim.
        assert!((15_800..=16_000).contains(&pcm.len()), "{}", pcm.len());
        let hz =
            sds_simulator::whisper::estimate_tone(SAMPLE_RATE, pcm.get(4000..).unwrap_or_default())
                .unwrap_or_default();
        assert!((hz - 700.0).abs() < 25.0, "{hz}");
        // The WAV round-trips through the stub's parser.
        let (rate, back) = sds_simulator::whisper::parse_wav(&wav(&pcm, SAMPLE_RATE))
            .unwrap_or_else(|| panic!("wav"));
        assert_eq!((rate, back.len()), (SAMPLE_RATE, pcm.len()));
        assert_eq!(back, pcm);
    }

    #[test]
    fn enforces_limits_and_rejects_garbage() {
        assert!(matches!(
            decode_ogg_opus(&tone_ogg(500.0, 3000), 1),
            Err(AudioError::TooLong(1))
        ));
        assert!(matches!(
            decode_ogg_opus(b"OggS but not really", 60),
            Err(AudioError::Container(_))
        ));
        let w = wav(&[1, -1], 16_000);
        assert_eq!(w.len(), 48);
        assert_eq!(w.get(..4), Some(&b"RIFF"[..]));
    }
}
