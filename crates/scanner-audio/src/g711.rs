//! G.711 µ-law (PCMU, RTP payload type 0) per ITU-T G.711.

/// Decodes one µ-law byte to 16-bit linear PCM.
#[must_use]
pub fn ulaw_to_linear(u: u8) -> i16 {
    let u = !u;
    let sign = u & 0x80;
    let exponent = (u >> 4) & 0x07;
    let mantissa = u & 0x0F;
    let magnitude = ((i32::from(mantissa) << 3) + 0x84) << exponent;
    let sample = magnitude - 0x84;
    // Range is within ±32124, so the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation)]
    let s = sample as i16;
    if sign != 0 { -s } else { s }
}

/// Encodes 16-bit linear PCM to µ-law (used by the simulator and tests).
#[must_use]
pub fn linear_to_ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32635;
    let mut s = i32::from(sample);
    let sign = if s < 0 {
        s = -s;
        0x80
    } else {
        0
    };
    s = s.min(CLIP) + BIAS;
    let mut exponent = 7u8;
    let mut mask = 0x4000;
    while exponent > 0 && s & mask == 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = ((s >> (i32::from(exponent) + 3)) & 0x0F) as u8;
    !(sign | (exponent << 4) | mantissa)
}

/// Decodes a PCMU payload into `out`.
pub fn decode_into(payload: &[u8], out: &mut Vec<i16>) {
    out.extend(payload.iter().map(|&b| ulaw_to_linear(b)));
}

/// µ-law silence.
pub const ULAW_SILENCE: u8 = 0xFF;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(ulaw_to_linear(0xFF), 0);
        assert_eq!(ulaw_to_linear(0x7F), 0);
        assert_eq!(ulaw_to_linear(0x00), -32124);
        assert_eq!(ulaw_to_linear(0x80), 32124);
        assert_eq!(linear_to_ulaw(0), 0xFF);
    }

    #[test]
    fn round_trip_is_close() {
        for s in (-32000i32..32000).step_by(97) {
            let s = s as i16;
            let back = ulaw_to_linear(linear_to_ulaw(s));
            let err = (i32::from(back) - i32::from(s)).abs();
            // µ-law quantisation error grows with magnitude (≤ ~3 %).
            assert!(err <= 2 + i32::from(s).abs() / 30, "{s} -> {back}");
        }
    }
}
