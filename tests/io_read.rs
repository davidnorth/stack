//! TIFF reader conformance tests against committed fixtures.

use std::path::PathBuf;

use focus_stack::io::tiff_read::read_tiff;

const W: u32 = 19;
const H: u32 = 13;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/io")
        .join(name)
}

/// 8-bit source value for pixel `(x, y)`, channel `c` in the fixture pattern.
fn sample8(x: u32, y: u32, c: u32) -> u16 {
    (((y * W + x) * 3 + c) % 256) as u16
}

/// Expected 16-bit value after the documented `v * 257` promotion.
fn sample16(x: u32, y: u32, c: u32) -> u16 {
    sample8(x, y, c) * 257
}

fn expected_rgb() -> Vec<u16> {
    let mut rgb = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            for c in 0..3 {
                rgb.push(sample16(x, y, c));
            }
        }
    }
    rgb
}

fn expected_gray() -> Vec<u16> {
    let mut rgb = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            let value = ((y * W + x) % 256) as u16 * 257;
            rgb.extend_from_slice(&[value, value, value]);
        }
    }
    rgb
}

fn expected_oriented(orientation: u16) -> Vec<u16> {
    let (out_w, out_h) = if orientation <= 4 { (W, H) } else { (H, W) };
    let mut rgb = Vec::with_capacity((out_w * out_h * 3) as usize);
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = match orientation {
                1 => (x, y),
                2 => (W - 1 - x, y),
                3 => (W - 1 - x, H - 1 - y),
                4 => (x, H - 1 - y),
                5 => (y, x),
                6 => (y, H - 1 - x),
                7 => (W - 1 - y, H - 1 - x),
                8 => (W - 1 - y, x),
                _ => unreachable!(),
            };
            for c in 0..3 {
                rgb.push(sample16(sx, sy, c));
            }
        }
    }
    rgb
}

fn assert_pixels(actual: &[u16], expected: &[u16], name: &str) {
    assert_eq!(actual.len(), expected.len(), "{name}: sample count");
    if let Some(index) = actual
        .iter()
        .zip(expected.iter())
        .position(|(actual, expected)| actual != expected)
    {
        panic!(
            "{name}: sample {index} is {}, expected {}",
            actual[index], expected[index]
        );
    }
}

fn assert_rgb_fixture(name: &str, bits: u8) {
    let frame = read_tiff(fixture(name)).unwrap_or_else(|err| panic!("{name}: {err}"));
    assert_eq!(frame.meta.width, W, "{name}: width");
    assert_eq!(frame.meta.height, H, "{name}: height");
    assert_eq!(frame.meta.bits, bits, "{name}: bit depth");
    assert_eq!(frame.meta.orientation, 1, "{name}: orientation");
    assert_pixels(&frame.rgb, &expected_rgb(), name);
}

#[test]
fn decodes_16bit_rgb_strips_and_compressions() {
    for name in [
        "rgb16_none.tif",
        "rgb16_lzw.tif",
        "rgb16_lzw_predictor.tif",
        "rgb16_deflate.tif",
        "rgb16_deflate_predictor.tif",
        "rgb16_packbits.tif",
        "rgb16_bigendian.tif",
    ] {
        assert_rgb_fixture(name, 16);
    }
}

#[test]
fn decodes_tiled_16bit_rgb() {
    assert_rgb_fixture("rgb16_tiled.tif", 16);
}

#[test]
fn decodes_bigtiff() {
    assert_rgb_fixture("rgb16_bigtiff.tif", 16);
}

#[test]
fn decodes_8bit_rgb_and_promotes_to_16bit() {
    for name in ["rgb8_none.tif", "rgb8_lzw.tif", "rgb8_lzw_predictor.tif"] {
        assert_rgb_fixture(name, 8);
    }
}

#[test]
fn replicates_grayscale_to_rgb() {
    for (name, bits) in [("gray16_none.tif", 16), ("gray8_lzw.tif", 8)] {
        let frame = read_tiff(fixture(name)).unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(frame.meta.width, W, "{name}: width");
        assert_eq!(frame.meta.height, H, "{name}: height");
        assert_eq!(frame.meta.bits, bits, "{name}: bit depth");
        assert_pixels(&frame.rgb, &expected_gray(), name);
    }
}

#[test]
fn applies_exif_orientation_and_keeps_original_tag() {
    for orientation in [1, 6, 8] {
        let name = format!("orient{orientation}.tif");
        let frame = read_tiff(fixture(&name)).unwrap_or_else(|err| panic!("{name}: {err}"));
        let (out_w, out_h) = if orientation <= 4 { (W, H) } else { (H, W) };
        assert_eq!(frame.meta.orientation, orientation, "{name}: tag");
        assert_eq!(frame.meta.width, out_w, "{name}: width");
        assert_eq!(frame.meta.height, out_h, "{name}: height");
        assert_eq!(frame.meta.bits, 8, "{name}: bit depth");
        assert_pixels(&frame.rgb, &expected_oriented(orientation), &name);
    }
}

#[test]
fn captures_icc_profile_bytes_exactly() {
    let frame = read_tiff(fixture("icc_srgb.tif")).unwrap();
    let expected = std::fs::read(fixture("srgb.icc")).unwrap();
    assert_eq!(frame.meta.icc.as_deref(), Some(expected.as_slice()));
    assert_eq!(frame.meta.bits, 8);
    assert_eq!(frame.meta.orientation, 1);
    assert_pixels(&frame.rgb, &expected_rgb(), "icc_srgb.tif");

    let plain = read_tiff(fixture("rgb16_none.tif")).unwrap();
    assert_eq!(plain.meta.icc, None);
}

#[test]
fn rejects_cmyk() {
    let err = read_tiff(fixture("cmyk.tif")).unwrap_err();
    assert!(err.to_string().contains("CMYK"), "unexpected error: {err}");
    assert_eq!(err.path(), fixture("cmyk.tif"));
}

#[test]
fn rejects_planar_configuration() {
    let err = read_tiff(fixture("rgb8_planar.tif")).unwrap_err();
    assert!(
        err.to_string().contains("planar configuration"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_unsupported_compression() {
    let err = read_tiff(fixture("rgb16_lzma.tif")).unwrap_err();
    assert!(
        err.to_string().contains("compression"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_truncated_file_with_named_error() {
    let err = read_tiff(fixture("truncated.tif")).unwrap_err();
    assert_eq!(err.path(), fixture("truncated.tif"));
    assert!(
        err.to_string().contains("truncated.tif"),
        "error does not name the file: {err}"
    );
}
