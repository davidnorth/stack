//! Retouch masks and recompositing (`docs/outputs.md` §6, `docs/cli.md` §4).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::io::tiff_read::read_tiff;
use focus_stack::io::tiff_write::{WriteOptions, write_tiff};
use focus_stack::output::report::sha256_file;
use focus_stack::retouch::{RetouchRequest, retouch};
use focus_stack::{Frame, FrameMeta, Similarity};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-retouch-{}-{id}-{label}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn frame(path: &Path, width: u32, height: u32, colour: [u16; 3]) -> Frame {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for _ in 0..(width * height) {
        rgb.extend_from_slice(&colour);
    }
    Frame {
        meta: FrameMeta {
            path: path.to_path_buf(),
            width,
            height,
            bits: 16,
            icc: None,
            orientation: 1,
            exif: None,
            xmp: None,
            iptc: None,
            resolution: None,
        },
        rgb,
    }
}

fn write_colour(path: &Path, width: u32, height: u32, colour: [u16; 3]) {
    let frame = frame(path, width, height, colour);
    write_tiff(path, &frame, &WriteOptions::default()).unwrap();
}

fn identity() -> Similarity {
    Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    }
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn write_report(
    path: &Path,
    base: &Path,
    sources: &[PathBuf],
    transforms: &[Similarity],
) -> String {
    let inputs = sources
        .iter()
        .map(|source| {
            let hash = sha256_file(source).unwrap();
            format!(
                "{{\"path\": {}, \"sha256\": \"{hash}\"}}",
                json_string(source.to_str().unwrap())
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let transforms = transforms
        .iter()
        .map(|transform| {
            format!(
                "{{\"a\": {}, \"b\": {}, \"tx\": {}, \"ty\": {}}}",
                transform.a, transform.b, transform.tx, transform.ty
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let base_hash = sha256_file(base).unwrap();
    let report = format!(
        "{{\"schema_version\": 1, \"inputs\": [{inputs}], \
         \"alignment\": {{\"reference_index\": 0, \"transforms\": [{transforms}]}}, \
         \"output\": {{\"path\": {}, \"sha256\": \"{base_hash}\"}}}}",
        json_string(base.to_str().unwrap())
    );
    fs::write(path, report.clone()).unwrap();
    report
}

fn full_polygon_mask(width: u32, height: u32, source: usize) -> String {
    format!(
        "{{\"version\": 1, \"polygons\": [{{\"points\": \
         [[0, 0], [{width}, 0], [{width}, {height}], [0, {height}]], \
         \"feather\": 0, \"source\": {source}}}]}}"
    )
}

fn assert_colour(actual: &[u16], expected: [u16; 3], tolerance: i32) {
    for (index, pixel) in actual.as_chunks::<3>().0.iter().enumerate() {
        for (channel, &expected) in expected.iter().enumerate() {
            let difference = i32::from(pixel[channel]) - i32::from(expected);
            assert!(
                difference.abs() <= tolerance,
                "pixel {index} channel {channel}: got {}, expected {expected}",
                pixel[channel]
            );
        }
    }
}

fn write_png_gray(path: &Path, width: u32, height: u32, pixels: &[u8]) {
    assert_eq!(pixels.len(), (width * height) as usize);
    let mut raw = Vec::new();
    for row in pixels.chunks(width as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&raw).unwrap();
    let compressed = encoder.finish().unwrap();

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 0, 0, 0, 0]);
    write_chunk(&mut png, b"IHDR", &header);
    write_chunk(&mut png, b"IDAT", &compressed);
    write_chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).unwrap();
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn retouch_from_json_reproduces_the_patch_without_seams() {
    let dir = TempDir::new("patch");
    let base = dir.join("base.tif");
    let source0 = dir.join("source0.tif");
    let source1 = dir.join("source1.tif");
    let report = dir.join("run.json");
    let mask = dir.join("mask.json");
    let output = dir.join("retouched.tif");
    let second_output = dir.join("retouched-2.tif");

    write_colour(&base, 6, 5, [1_000, 2_000, 3_000]);
    write_colour(&source0, 6, 5, [1_000, 2_000, 3_000]);
    write_colour(&source1, 6, 5, [30_000, 40_000, 50_000]);
    write_report(
        &report,
        &base,
        &[source0.clone(), source1.clone()],
        &[identity(), identity()],
    );
    fs::write(&mask, full_polygon_mask(6, 5, 1)).unwrap();

    let base_bytes = fs::read(&base).unwrap();
    let request = RetouchRequest {
        base: base.clone(),
        sources: vec![source0, source1],
        report,
        mask,
        output: Some(output.clone()),
    };
    let outcome = retouch(&request).expect("retouch");
    assert_eq!(outcome.output, output);
    assert_eq!(outcome.masks_applied, 1);

    let written = read_tiff(&output).unwrap();
    assert_eq!(written.meta.width, 6);
    assert_eq!(written.meta.height, 5);
    assert_colour(&written.rgb, [30_000, 40_000, 50_000], 1);

    // The same request is byte-identical.
    let second = RetouchRequest {
        output: Some(second_output.clone()),
        ..request
    };
    retouch(&second).expect("second retouch");
    assert_eq!(
        fs::read(&output).unwrap(),
        fs::read(&second_output).unwrap()
    );

    // The base is never modified.
    assert_eq!(fs::read(&base).unwrap(), base_bytes);
}

#[test]
fn report_transforms_are_applied_deterministically() {
    let dir = TempDir::new("transform");
    let base = dir.join("base.tif");
    let source = dir.join("source.tif");
    let mask = dir.join("mask.json");
    let identity_report = dir.join("identity.json");
    let shifted_report = dir.join("shifted.json");

    write_colour(&base, 8, 4, [1_000, 1_000, 1_000]);
    let mut patterned = frame(&source, 8, 4, [0, 0, 0]);
    for y in 0..4 {
        for x in 0..8 {
            let colour = if x < 4 {
                [10_000, 10_000, 10_000]
            } else {
                [55_000, 55_000, 55_000]
            };
            let index = (y * 8 + x) as usize * 3;
            patterned.rgb[index..index + 3].copy_from_slice(&colour);
        }
    }
    write_tiff(&source, &patterned, &WriteOptions::default()).unwrap();
    write_report(
        &identity_report,
        &base,
        std::slice::from_ref(&source),
        &[identity()],
    );
    write_report(
        &shifted_report,
        &base,
        std::slice::from_ref(&source),
        &[Similarity {
            a: 1.0,
            b: 0.0,
            tx: 2.0,
            ty: 0.0,
        }],
    );
    fs::write(&mask, full_polygon_mask(8, 4, 0)).unwrap();

    let output_a = dir.join("a.tif");
    let output_b = dir.join("b.tif");
    let output_c = dir.join("c.tif");
    for (report, output) in [
        (&identity_report, &output_a),
        (&shifted_report, &output_b),
        (&shifted_report, &output_c),
    ] {
        retouch(&RetouchRequest {
            base: base.clone(),
            sources: vec![source.clone()],
            report: report.clone(),
            mask: mask.clone(),
            output: Some(output.clone()),
        })
        .expect("retouch");
    }

    assert_ne!(
        fs::read(&output_a).unwrap(),
        fs::read(&output_b).unwrap(),
        "a stored translation must change the result"
    );
    assert_eq!(
        fs::read(&output_b).unwrap(),
        fs::read(&output_c).unwrap(),
        "the same report must reproduce the same bytes"
    );
}

#[test]
fn png_mask_selects_only_white_pixels() {
    let dir = TempDir::new("png");
    let base = dir.join("base.tif");
    let source = dir.join("source.tif");
    let report = dir.join("run.json");
    let mask = dir.join("mask.json");
    let png = dir.join("mask.png");
    let output = dir.join("out.tif");

    write_colour(&base, 4, 4, [1_000, 1_000, 1_000]);
    write_colour(&source, 4, 4, [50_000, 50_000, 50_000]);
    write_report(&report, &base, std::slice::from_ref(&source), &[identity()]);
    write_png_gray(
        &png,
        4,
        4,
        &[
            255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0,
        ],
    );
    fs::write(
        &mask,
        "{\"version\": 1, \"masks\": [{\"path\": \"mask.png\", \"source\": 0, \"feather\": 0}]}",
    )
    .unwrap();

    retouch(&RetouchRequest {
        base,
        sources: vec![source],
        report,
        mask,
        output: Some(output.clone()),
    })
    .expect("retouch");

    let written = read_tiff(&output).unwrap();
    for y in 0..4 {
        for x in 0..4 {
            let index = (y * 4 + x) as usize * 3;
            let expected = if x < 2 { 50_000 } else { 1_000 };
            let actual = written.rgb[index];
            assert!(
                (i32::from(actual) - expected).abs() <= 1,
                "pixel ({x}, {y}) = {actual}, expected {expected}"
            );
        }
    }
}

#[test]
fn hashes_and_invalid_masks_are_rejected() {
    let dir = TempDir::new("invalid");
    let base = dir.join("base.tif");
    let source = dir.join("source.tif");
    let report = dir.join("run.json");
    let mask = dir.join("mask.json");

    write_colour(&base, 4, 3, [1_000, 1_000, 1_000]);
    write_colour(&source, 4, 3, [20_000, 20_000, 20_000]);
    write_report(&report, &base, std::slice::from_ref(&source), &[identity()]);

    let request = || RetouchRequest {
        base: base.clone(),
        sources: vec![source.clone()],
        report: report.clone(),
        mask: mask.clone(),
        output: Some(dir.join("out.tif")),
    };

    fs::write(&mask, "{\"version\": 2}").unwrap();
    assert!(retouch(&request()).is_err());

    fs::write(&mask, full_polygon_mask(4, 3, 1)).unwrap();
    assert!(
        retouch(&request()).is_err(),
        "a source index past the end must be rejected"
    );

    fs::write(&mask, full_polygon_mask(4, 3, 0)).unwrap();
    write_colour(&source, 4, 3, [21_000, 21_000, 21_000]);
    assert!(
        retouch(&request()).is_err(),
        "changed source bytes must fail hash verification"
    );
}

#[test]
fn png_dimension_mismatch_is_rejected() {
    let dir = TempDir::new("png-size");
    let base = dir.join("base.tif");
    let source = dir.join("source.tif");
    let report = dir.join("run.json");
    let mask = dir.join("mask.json");
    let png = dir.join("mask.png");

    write_colour(&base, 4, 4, [1_000, 1_000, 1_000]);
    write_colour(&source, 4, 4, [20_000, 20_000, 20_000]);
    write_report(&report, &base, std::slice::from_ref(&source), &[identity()]);
    write_png_gray(&png, 2, 2, &[255, 255, 255, 255]);
    fs::write(
        &mask,
        "{\"version\": 1, \"masks\": [{\"path\": \"mask.png\", \"source\": 0}]}",
    )
    .unwrap();

    let error = retouch(&RetouchRequest {
        base,
        sources: vec![source],
        report,
        mask,
        output: Some(dir.join("out.tif")),
    })
    .expect_err("dimension mismatch");
    assert!(error.to_string().contains("4x4") || error.to_string().contains("2x2"));
}

#[test]
fn cli_retouch_subcommand_writes_a_new_tiff() {
    let dir = TempDir::new("cli");
    let base = dir.join("base.tif");
    let source = dir.join("source.tif");
    let report = dir.join("run.json");
    let mask = dir.join("mask.json");
    let output = dir.join("out.tif");

    write_colour(&base, 4, 3, [1_000, 1_000, 1_000]);
    write_colour(&source, 4, 3, [42_000, 42_000, 42_000]);
    write_report(&report, &base, std::slice::from_ref(&source), &[identity()]);
    fs::write(&mask, full_polygon_mask(4, 3, 0)).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_focus-stack"))
        .args([
            "retouch",
            "--base",
            base.to_str().unwrap(),
            "--sources",
            source.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--mask",
            mask.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("run focus-stack retouch");

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let written = read_tiff(&output).unwrap();
    assert_colour(&written.rgb, [42_000, 42_000, 42_000], 1);
}
