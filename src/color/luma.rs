//! Encoded and linear luma.
//!
//! Focus measurement and alignment use encoded luma (`docs/io-and-color.md`
//! §6); blending uses linear RGB.

/// Rec. 709 luma coefficients.
pub const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Encoded luma `Y' = 0.2126 R' + 0.7152 G' + 0.0722 B'`.
#[inline]
#[must_use]
pub fn encoded_luma(r: f32, g: f32, b: f32) -> f32 {
    REC709[0] * r + REC709[1] * g + REC709[2] * b
}

/// Linear-light luma, using the same Rec. 709 coefficients on linear values.
#[inline]
#[must_use]
pub fn linear_luma(r: f32, g: f32, b: f32) -> f32 {
    REC709[0] * r + REC709[1] * g + REC709[2] * b
}

/// Compute encoded luma for an interleaved RGB buffer.
///
/// # Panics
///
/// Panics if `rgb.len() != out.len() * 3`.
pub fn encoded_luma_buffer(rgb: &[f32], out: &mut [f32]) {
    assert_eq!(rgb.len(), out.len() * 3, "RGB buffer length mismatch");
    for (pixel, luma) in rgb.as_chunks::<3>().0.iter().zip(out.iter_mut()) {
        *luma = encoded_luma(pixel[0], pixel[1], pixel[2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_luma_matches_reference() {
        assert!((encoded_luma(0.5, 0.25, 0.75) - 0.339_25).abs() < 1e-6);
        assert!((encoded_luma(1.0, 1.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((encoded_luma(0.0, 0.0, 0.0) - 0.0).abs() < 1e-8);
    }

    #[test]
    fn linear_luma_is_not_encoded_luma() {
        let encoded = encoded_luma(0.5, 0.5, 0.5);
        let linear = linear_luma(0.5, 0.5, 0.5);
        assert!((encoded - 0.5).abs() < 1e-6);
        assert!((linear - 0.5).abs() < 1e-6);
    }

    #[test]
    fn buffer_matches_scalar() {
        let rgb = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let mut out = [0.0; 3];
        encoded_luma_buffer(&rgb, &mut out);
        assert!((out[0] - 0.2126).abs() < 1e-6);
        assert!((out[1] - 0.7152).abs() < 1e-6);
        assert!((out[2] - 0.0722).abs() < 1e-6);
    }
}
