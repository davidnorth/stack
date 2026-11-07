//! EXIF orientation and metadata extraction.

use std::fmt;

/// Image after the EXIF orientation has been baked into the pixel data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrientedImage {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u16>,
}

/// Error returned for an orientation tag outside the EXIF range `1..=8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidOrientation(pub u16);

impl fmt::Display for InvalidOrientation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid EXIF orientation {} (expected 1..=8)", self.0)
    }
}

impl std::error::Error for InvalidOrientation {}

/// Returns the upright dimensions for `orientation`.
pub fn oriented_dimensions(
    width: u32,
    height: u32,
    orientation: u16,
) -> Result<(u32, u32), InvalidOrientation> {
    match orientation {
        1..=4 => Ok((width, height)),
        5..=8 => Ok((height, width)),
        other => Err(InvalidOrientation(other)),
    }
}

/// Applies the EXIF orientation transform, returning an upright image.
///
/// The input buffer is interleaved RGB with `width * height * 3` samples.
/// Orientations 5–8 swap the output width and height.
pub fn apply_orientation(
    rgb: &[u16],
    width: u32,
    height: u32,
    orientation: u16,
) -> Result<OrientedImage, InvalidOrientation> {
    let (out_width, out_height) = oriented_dimensions(width, height, orientation)?;

    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or(InvalidOrientation(orientation))?;
    assert_eq!(
        rgb.len(),
        expected,
        "orientation input buffer does not match dimensions"
    );

    if orientation == 1 {
        return Ok(OrientedImage {
            width,
            height,
            rgb: rgb.to_vec(),
        });
    }

    let mut out = vec![0u16; expected];
    let w = width as i64;
    let h = height as i64;
    let out_w = out_width as usize;

    for y in 0..out_height as usize {
        for x in 0..out_width as usize {
            let (sx, sy) = match orientation {
                // Mirror horizontal.
                2 => (w - 1 - x as i64, y as i64),
                // Rotate 180°.
                3 => (w - 1 - x as i64, h - 1 - y as i64),
                // Mirror vertical.
                4 => (x as i64, h - 1 - y as i64),
                // Transpose (mirror along the main diagonal).
                5 => (y as i64, x as i64),
                // Rotate 90° clockwise.
                6 => (y as i64, h - 1 - x as i64),
                // Transverse (mirror along the anti-diagonal).
                7 => (w - 1 - y as i64, h - 1 - x as i64),
                // Rotate 90° counter-clockwise.
                8 => (w - 1 - y as i64, x as i64),
                _ => unreachable!("orientation validated above"),
            };

            let src = (sy as usize * width as usize + sx as usize) * 3;
            let dst = (y * out_w + x) * 3;
            out[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
        }
    }

    Ok(OrientedImage {
        width: out_width,
        height: out_height,
        rgb: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3×2 image with pixel `(x, y)` encoded as `(x + 1, y + 1, 7)`.
    fn sample() -> Vec<u16> {
        let mut rgb = Vec::new();
        for y in 0..2 {
            for x in 0..3 {
                rgb.extend_from_slice(&[x + 1, y + 1, 7]);
            }
        }
        rgb
    }

    fn pixel(image: &OrientedImage, x: u32, y: u32) -> [u16; 3] {
        let i = ((y * image.width + x) * 3) as usize;
        [image.rgb[i], image.rgb[i + 1], image.rgb[i + 2]]
    }

    #[test]
    fn orientation_1_is_identity() {
        let out = apply_orientation(&sample(), 3, 2, 1).unwrap();
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(pixel(&out, 0, 0), [1, 1, 7]);
        assert_eq!(pixel(&out, 2, 1), [3, 2, 7]);
    }

    #[test]
    fn orientation_2_mirrors_horizontally() {
        let out = apply_orientation(&sample(), 3, 2, 2).unwrap();
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(pixel(&out, 0, 0), [3, 1, 7]);
        assert_eq!(pixel(&out, 2, 1), [1, 2, 7]);
    }

    #[test]
    fn orientation_3_rotates_180() {
        let out = apply_orientation(&sample(), 3, 2, 3).unwrap();
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(pixel(&out, 0, 0), [3, 2, 7]);
        assert_eq!(pixel(&out, 2, 1), [1, 1, 7]);
    }

    #[test]
    fn orientation_4_mirrors_vertically() {
        let out = apply_orientation(&sample(), 3, 2, 4).unwrap();
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(pixel(&out, 0, 0), [1, 2, 7]);
        assert_eq!(pixel(&out, 2, 1), [3, 1, 7]);
    }

    #[test]
    fn orientation_5_transposes() {
        let out = apply_orientation(&sample(), 3, 2, 5).unwrap();
        assert_eq!((out.width, out.height), (2, 3));
        assert_eq!(pixel(&out, 0, 0), [1, 1, 7]);
        assert_eq!(pixel(&out, 1, 0), [1, 2, 7]);
        assert_eq!(pixel(&out, 0, 1), [2, 1, 7]);
        assert_eq!(pixel(&out, 1, 2), [3, 2, 7]);
    }

    #[test]
    fn orientation_6_rotates_90_clockwise() {
        let out = apply_orientation(&sample(), 3, 2, 6).unwrap();
        assert_eq!((out.width, out.height), (2, 3));
        assert_eq!(pixel(&out, 0, 0), [1, 2, 7]);
        assert_eq!(pixel(&out, 1, 0), [1, 1, 7]);
        assert_eq!(pixel(&out, 0, 2), [3, 2, 7]);
        assert_eq!(pixel(&out, 1, 2), [3, 1, 7]);
    }

    #[test]
    fn orientation_7_is_transverse() {
        let out = apply_orientation(&sample(), 3, 2, 7).unwrap();
        assert_eq!((out.width, out.height), (2, 3));
        assert_eq!(pixel(&out, 0, 0), [3, 2, 7]);
        assert_eq!(pixel(&out, 1, 0), [3, 1, 7]);
        assert_eq!(pixel(&out, 0, 2), [1, 2, 7]);
        assert_eq!(pixel(&out, 1, 2), [1, 1, 7]);
    }

    #[test]
    fn orientation_8_rotates_90_counter_clockwise() {
        let out = apply_orientation(&sample(), 3, 2, 8).unwrap();
        assert_eq!((out.width, out.height), (2, 3));
        assert_eq!(pixel(&out, 0, 0), [3, 1, 7]);
        assert_eq!(pixel(&out, 1, 0), [3, 2, 7]);
        assert_eq!(pixel(&out, 0, 2), [1, 1, 7]);
        assert_eq!(pixel(&out, 1, 2), [1, 2, 7]);
    }

    #[test]
    fn invalid_orientation_is_rejected() {
        assert_eq!(oriented_dimensions(4, 3, 0), Err(InvalidOrientation(0)));
        assert_eq!(oriented_dimensions(4, 3, 9), Err(InvalidOrientation(9)));
        assert_eq!(
            apply_orientation(&sample(), 3, 2, 9),
            Err(InvalidOrientation(9))
        );
    }
}
