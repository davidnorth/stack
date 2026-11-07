//! TIFF writer round-trip tests.

use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::io::metadata::MetadataPolicy;
use focus_stack::io::tiff_read::read_tiff;
use focus_stack::io::tiff_write::{Compression, WriteOptions, write_tiff};
use focus_stack::{Frame, Resolution};
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
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-roundtrip-{}-{id}-{label}.tif",
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

fn assert_roundtrip(source: &Frame, output: &Path, bits: u8) {
    let round = read_tiff(output).unwrap_or_else(|err| panic!("{}: {err}", output.display()));
    assert_eq!(round.meta.width, source.meta.width, "width");
    assert_eq!(round.meta.height, source.meta.height, "height");
    assert_eq!(round.meta.bits, bits, "bit depth");
    assert_eq!(round.meta.orientation, 1, "orientation");
    assert_eq!(round.rgb, source.rgb, "pixels");
    assert_eq!(round.meta.icc, source.meta.icc, "ICC bytes");
}

#[test]
fn orientation_one_fixtures_roundtrip_bit_exactly() {
    let fixtures = [
        "rgb16_none.tif",
        "rgb16_lzw.tif",
        "rgb16_lzw_predictor.tif",
        "rgb16_deflate.tif",
        "rgb16_deflate_predictor.tif",
        "rgb16_packbits.tif",
        "rgb16_bigendian.tif",
        "rgb16_tiled.tif",
        "rgb16_bigtiff.tif",
        "rgb8_none.tif",
        "rgb8_lzw.tif",
        "rgb8_lzw_predictor.tif",
        "gray16_none.tif",
        "gray8_lzw.tif",
        "icc_srgb.tif",
    ];

    for name in fixtures {
        let source = read_tiff(fixture(name)).unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(source.meta.orientation, 1, "{name}: fixture orientation");

        let output = TempFile::new(name);
        write_tiff(output.path(), &source, &WriteOptions::default())
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_roundtrip(&source, output.path(), 16);
    }
}

#[test]
fn writes_none_lzw_and_deflate() {
    let source = read_tiff(fixture("rgb16_none.tif")).unwrap();

    for (compression, tag) in [
        (Compression::None, 1),
        (Compression::Lzw, 5),
        (Compression::Deflate, 8),
    ] {
        let output = TempFile::new("compression");
        let options = WriteOptions {
            compression,
            ..WriteOptions::default()
        };
        write_tiff(output.path(), &source, &options).unwrap();
        assert_roundtrip(&source, output.path(), 16);

        let mut decoder = Decoder::new(BufReader::new(File::open(output.path()).unwrap())).unwrap();
        let actual = decoder
            .find_tag_unsigned::<u16>(Tag::Compression)
            .unwrap()
            .unwrap_or(1);
        assert_eq!(actual, tag, "{compression:?}: compression tag");
    }
}

#[test]
fn preserves_icc_xmp_and_resolution() {
    let mut frame = read_tiff(fixture("rgb16_none.tif")).unwrap();
    frame.meta.icc = Some(fs::read(fixture("srgb.icc")).unwrap());
    frame.meta.xmp = Some(b"<x:xmpmeta>focus-stack</x:xmpmeta>".to_vec());
    frame.meta.resolution = Some(Resolution {
        x_numerator: 300,
        x_denominator: 1,
        y_numerator: 300,
        y_denominator: 1,
        unit: 2,
    });

    let output = TempFile::new("metadata-copy");
    write_tiff(output.path(), &frame, &WriteOptions::default()).unwrap();
    let round = read_tiff(output.path()).unwrap();
    assert_eq!(round.meta.icc, frame.meta.icc);
    assert_eq!(round.meta.xmp, frame.meta.xmp);
    assert_eq!(round.meta.resolution, frame.meta.resolution);
    assert_eq!(round.meta.orientation, 1);
    assert_eq!(round.rgb, frame.rgb);

    let stripped = TempFile::new("metadata-strip");
    let options = WriteOptions {
        metadata: MetadataPolicy::Strip,
        ..WriteOptions::default()
    };
    write_tiff(stripped.path(), &frame, &options).unwrap();
    let round = read_tiff(stripped.path()).unwrap();
    assert_eq!(round.meta.icc, frame.meta.icc, "ICC survives stripping");
    assert_eq!(round.meta.xmp, None, "XMP is stripped");
    assert_eq!(round.meta.resolution, frame.meta.resolution);
}

#[test]
fn writes_eight_bit_output_when_requested() {
    let source = read_tiff(fixture("rgb8_none.tif")).unwrap();

    let output = TempFile::new("bits8");
    let options = WriteOptions {
        bits: 8,
        ..WriteOptions::default()
    };
    write_tiff(output.path(), &source, &options).unwrap();
    assert_roundtrip(&source, output.path(), 8);
}

#[test]
fn writes_lightroom_friendly_tags() {
    let source = read_tiff(fixture("rgb16_none.tif")).unwrap();
    let output = TempFile::new("tags");
    write_tiff(output.path(), &source, &WriteOptions::default()).unwrap();

    let mut decoder = Decoder::new(BufReader::new(File::open(output.path()).unwrap())).unwrap();
    assert_eq!(
        decoder.find_tag_unsigned::<u16>(Tag::Orientation).unwrap(),
        Some(1)
    );
    assert_eq!(
        decoder
            .find_tag_unsigned::<u16>(Tag::ResolutionUnit)
            .unwrap()
            .unwrap_or(1),
        1
    );
    assert_eq!(
        decoder
            .find_tag_unsigned::<u16>(Tag::PlanarConfiguration)
            .unwrap()
            .unwrap_or(1),
        1
    );
    assert_eq!(
        decoder
            .find_tag_unsigned_vec::<u16>(Tag::SampleFormat)
            .unwrap()
            .unwrap_or_else(|| vec![1, 1, 1]),
        vec![1, 1, 1]
    );

    match decoder.find_tag(Tag::Software).unwrap() {
        Some(Value::Ascii(software)) => {
            assert!(
                software.starts_with("focus-stack "),
                "unexpected software tag: {software}"
            );
        }
        other => panic!("missing Software tag: {other:?}"),
    }
}

#[test]
fn bakes_non_upright_input_to_orientation_one() {
    let source = read_tiff(fixture("orient6.tif")).unwrap();
    assert_eq!(source.meta.orientation, 6);

    let output = TempFile::new("orient6");
    write_tiff(output.path(), &source, &WriteOptions::default()).unwrap();
    let round = read_tiff(output.path()).unwrap();

    assert_eq!(round.meta.orientation, 1);
    assert_eq!(round.meta.width, source.meta.width);
    assert_eq!(round.meta.height, source.meta.height);
    assert_eq!(round.rgb, source.rgb);
}
