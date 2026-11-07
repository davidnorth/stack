//! sRGB <-> linear-light transfer functions.
//!
//! These are the exact IEC 61966-2-1 curves from `docs/io-and-color.md` §6.
//! Values outside `[0, 1]` are extended by the same formulas rather than
//! clamped; clipping is reported by the caller.

/// Decode one encoded sRGB channel to linear light.
#[inline]
#[must_use]
pub fn srgb_decode(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Encode one linear-light sRGB channel.
#[inline]
#[must_use]
pub fn srgb_encode(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_matches_reference_points() {
        assert!((srgb_decode(0.0) - 0.0).abs() < 1e-8);
        assert!((srgb_decode(1.0) - 1.0).abs() < 1e-7);
        assert!((srgb_decode(0.5) - 0.214_041_14).abs() < 1e-7);
        assert!((srgb_encode(0.214_041_14) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn round_trip_is_within_1e_6() {
        for i in 0..=10_000 {
            let encoded = i as f32 / 10_000.0;
            let round_trip = srgb_encode(srgb_decode(encoded));
            assert!(
                (round_trip - encoded).abs() < 1e-6,
                "encoded {encoded}: got {round_trip}"
            );
        }
    }

    #[test]
    fn linear_segment_is_continuous() {
        let below = srgb_decode(0.040_45);
        let above = srgb_decode(0.040_451);
        assert!((above - below).abs() < 1e-7);
    }
}
