//! Output metadata policy and preview conformance tests.

use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::io::metadata::MetadataPolicy;
use focus_stack::io::tiff_read::read_tiff;
use focus_stack::io::tiff_write::{Compression, WriteOptions, write_tiff};
use focus_stack::output::preview::{
    DEFAULT_MAX_SIZE, JPEG_QUALITY, encode_jpeg, preview_image, write_preview,
};
use focus_stack::{ExifFields, Frame, FrameMeta, Resolution, VERSION};
use tiff::decoder::Decoder;
use tiff::decoder::ifd::Value;
use tiff::tags::Tag;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/io")
        .join(name)
}

/// Temporary output file removed when the test ends.
struct TempFile(PathBuf);

impl TempFile {
    fn new(label: &str, extension: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-metadata-{}-{id}-{label}.{extension}",
            std::process::id()
        ));
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn frame(width: u32, height: u32) -> Frame {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            for channel in 0..3 {
                let value = ((y * width + x) * 3 + channel) % 65_536;
                rgb.push(value as u16);
            }
        }
    }

    Frame {
        meta: FrameMeta {
            path: PathBuf::from("frame.tif"),
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

fn metadata_frame() -> Frame {
    let mut source = frame(4, 3);
    source.meta.icc = Some(fs::read(fixture("srgb.icc")).unwrap());
    source.meta.xmp = Some(b"<x:xmpmeta>focus-stack</x:xmpmeta>".to_vec());
    source.meta.iptc = Some(vec![0x1c, 0x02, 0x05, 0x00, 0x03, b'a', b'b', b'c']);
    source.meta.exif = Some(ExifFields {
        image_description: Some("stacked frame".to_string()),
        make: Some("Focus Camera".to_string()),
        model: Some("Model 1".to_string()),
        date_time: Some("2026:11:07 10:00:00".to_string()),
        artist: Some("Test Artist".to_string()),
        copyright: Some("(c) 2026".to_string()),
    });
    source.meta.resolution = Some(Resolution {
        x_numerator: 300,
        x_denominator: 1,
        y_numerator: 300,
        y_denominator: 1,
        unit: 2,
    });
    source
}

fn options(policy: MetadataPolicy, bits: u8) -> WriteOptions {
    WriteOptions {
        compression: Compression::Lzw,
        bits,
        metadata: policy,
    }
}

#[test]
fn copy_policy_roundtrips_packets_and_common_exif() {
    let source = metadata_frame();
    let output = TempFile::new("copy", "tif");
    write_tiff(output.path(), &source, &options(MetadataPolicy::Copy, 16)).unwrap();

    let round = read_tiff(output.path()).unwrap();
    assert_eq!(round.meta.icc, source.meta.icc, "ICC bytes");
    assert_eq!(round.meta.xmp, source.meta.xmp, "XMP packet");
    assert_eq!(round.meta.iptc, source.meta.iptc, "IPTC record");
    assert_eq!(round.meta.exif, source.meta.exif, "common EXIF fields");
    assert_eq!(round.meta.resolution, source.meta.resolution, "resolution");
    assert_eq!(round.meta.orientation, 1, "orientation");
    assert_eq!(round.rgb, source.rgb, "pixels");
}

#[test]
fn strip_policy_keeps_icc_resolution_orientation_and_software() {
    let source = metadata_frame();
    let output = TempFile::new("strip", "tif");
    write_tiff(output.path(), &source, &options(MetadataPolicy::Strip, 16)).unwrap();

    let round = read_tiff(output.path()).unwrap();
    assert_eq!(round.meta.icc, source.meta.icc, "ICC survives stripping");
    assert_eq!(round.meta.xmp, None, "XMP is stripped");
    assert_eq!(round.meta.iptc, None, "IPTC is stripped");
    assert_eq!(round.meta.exif, None, "EXIF is stripped");
    assert_eq!(round.meta.resolution, source.meta.resolution);
    assert_eq!(round.meta.orientation, 1);

    let mut decoder = Decoder::new(BufReader::new(File::open(output.path()).unwrap())).unwrap();
    match decoder.find_tag(Tag::Software).unwrap() {
        Some(Value::Ascii(value)) => assert_eq!(value, format!("focus-stack {VERSION}")),
        other => panic!("missing Software tag: {other:?}"),
    }
}

#[test]
fn writes_orientation_one_and_reference_resolution() {
    let source = read_tiff(fixture("orient6.tif")).unwrap();
    assert_ne!(source.meta.orientation, 1, "fixture must need baking");

    let output = TempFile::new("orient6", "tif");
    write_tiff(output.path(), &source, &options(MetadataPolicy::Copy, 16)).unwrap();
    let round = read_tiff(output.path()).unwrap();

    assert_eq!(round.meta.orientation, 1);
    assert_eq!(round.meta.width, source.meta.width);
    assert_eq!(round.meta.height, source.meta.height);
    assert_eq!(round.rgb, source.rgb);

    let mut decoder = Decoder::new(BufReader::new(File::open(output.path()).unwrap())).unwrap();
    assert_eq!(
        decoder.find_tag_unsigned::<u16>(Tag::Orientation).unwrap(),
        Some(1)
    );
}

#[test]
fn preview_samples_match_eight_bit_tiff_output() {
    let source = metadata_frame();
    let output = TempFile::new("preview-reference", "tif");
    write_tiff(output.path(), &source, &options(MetadataPolicy::Copy, 8)).unwrap();
    let written = read_tiff(output.path()).unwrap();

    let preview = preview_image(&source, DEFAULT_MAX_SIZE, source.meta.icc.as_deref()).unwrap();
    assert_eq!((preview.width, preview.height), (4, 3));

    let expected: Vec<u8> = written
        .rgb
        .iter()
        .map(|&value| ((u32::from(value) + 128) / 257) as u8)
        .collect();
    assert_eq!(preview.rgb, expected, "preview color matches output");
}

#[test]
fn preview_resizes_to_the_documented_long_edge() {
    let source = frame(8, 4);
    let preview = preview_image(&source, 4, None).unwrap();
    assert_eq!((preview.width, preview.height), (4, 2));
    assert_eq!(preview.rgb.len(), 4 * 2 * 3);
}

#[test]
fn writes_a_valid_jpeg_preview() {
    let source = frame(16, 8);
    let output = TempFile::new("preview", "jpg");
    write_preview(output.path(), &source, 8, None).unwrap();

    let bytes = fs::read(output.path()).unwrap();
    assert_eq!(&bytes[..2], &[0xff, 0xd8], "JPEG SOI");
    assert_eq!(&bytes[bytes.len() - 2..], &[0xff, 0xd9], "JPEG EOI");

    let image = preview_image(&source, 8, None).unwrap();
    let encoded = encode_jpeg(&image, JPEG_QUALITY).unwrap();
    assert_eq!(encoded, bytes, "file contents are deterministic");
}
