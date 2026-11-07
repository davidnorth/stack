//! Color pipeline tests (`docs/io-and-color.md` §6).

use std::path::PathBuf;

use focus_stack::color::luma::encoded_luma;
use focus_stack::color::profile::{
    ColorError, ColorPipeline, ProfileKind, detect_profile, resolve_input,
};
use focus_stack::color::transfer::{srgb_decode, srgb_encode};
use lcms2::{CIExyY, CIExyYTRIPLE, Locale, MLU, Profile, Tag, TagSignature, ToneCurve};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/io")
        .join(name)
}

fn srgb_icc() -> Vec<u8> {
    std::fs::read(fixture("srgb.icc")).unwrap()
}

struct Primaries {
    red: (f64, f64),
    green: (f64, f64),
    blue: (f64, f64),
}

fn make_profile(description: &str, white: (f64, f64), primaries: Primaries, gamma: f64) -> Vec<u8> {
    let white = CIExyY {
        x: white.0,
        y: white.1,
        Y: 1.0,
    };
    let triple = CIExyYTRIPLE {
        Red: CIExyY {
            x: primaries.red.0,
            y: primaries.red.1,
            Y: 1.0,
        },
        Green: CIExyY {
            x: primaries.green.0,
            y: primaries.green.1,
            Y: 1.0,
        },
        Blue: CIExyY {
            x: primaries.blue.0,
            y: primaries.blue.1,
            Y: 1.0,
        },
    };
    let curve = ToneCurve::new(gamma);
    let mut profile = Profile::new_rgb(&white, &triple, &[&curve, &curve, &curve]).unwrap();
    let mut mlu = MLU::new(1);
    assert!(mlu.set_text_ascii(description, Locale::none()));
    assert!(profile.write_tag(TagSignature::ProfileDescriptionTag, Tag::MLU(&mlu)));
    profile.icc().unwrap()
}

fn srgb_primaries() -> Primaries {
    Primaries {
        red: (0.64, 0.33),
        green: (0.30, 0.60),
        blue: (0.15, 0.06),
    }
}

fn linear_srgb_profile() -> Vec<u8> {
    make_profile("RGB built-in", (0.3127, 0.3290), srgb_primaries(), 1.0)
}

fn adobe_rgb_profile() -> Vec<u8> {
    make_profile(
        "Adobe RGB (1998)",
        (0.3127, 0.3290),
        Primaries {
            red: (0.64, 0.33),
            green: (0.21, 0.71),
            blue: (0.15, 0.06),
        },
        563.0 / 256.0,
    )
}

fn prophoto_profile() -> Vec<u8> {
    make_profile(
        "ProPhoto RGB",
        (0.3457, 0.3585),
        Primaries {
            red: (0.7347, 0.2653),
            green: (0.1596, 0.8404),
            blue: (0.0366, 0.0001),
        },
        1.8,
    )
}

fn display_p3_profile() -> Vec<u8> {
    make_profile(
        "Display P3",
        (0.3127, 0.3290),
        Primaries {
            red: (0.680, 0.320),
            green: (0.265, 0.690),
            blue: (0.150, 0.060),
        },
        2.2,
    )
}

fn unknown_profile() -> Vec<u8> {
    make_profile(
        "ACME Wide Gamut",
        (0.3127, 0.3290),
        Primaries {
            red: (0.70, 0.29),
            green: (0.17, 0.80),
            blue: (0.13, 0.05),
        },
        2.0,
    )
}

#[test]
fn detects_known_profiles() {
    assert_eq!(detect_profile(&srgb_icc()).unwrap(), ProfileKind::Srgb);
    assert_eq!(
        detect_profile(&linear_srgb_profile()).unwrap(),
        ProfileKind::LinearSrgb,
        "linear profile with a generic description falls back to colorants + TRC"
    );
    assert_eq!(
        detect_profile(&adobe_rgb_profile()).unwrap(),
        ProfileKind::AdobeRgb
    );
    assert_eq!(
        detect_profile(&prophoto_profile()).unwrap(),
        ProfileKind::ProPhoto
    );
    assert_eq!(
        detect_profile(&display_p3_profile()).unwrap(),
        ProfileKind::DisplayP3
    );
}

#[test]
fn detects_unknown_and_malformed_profiles() {
    assert_eq!(
        detect_profile(&unknown_profile()).unwrap(),
        ProfileKind::Unknown
    );
    assert!(matches!(
        detect_profile(&[0, 1, 2]),
        Err(ColorError::MalformedProfile(_))
    ));
}

#[test]
fn srgb_round_trip_is_within_1e_6() {
    let icc = srgb_icc();
    let pipeline = ColorPipeline::new(Some(&icc), None, None).unwrap();
    assert_eq!(pipeline.input().kind, ProfileKind::Srgb);
    assert_eq!(pipeline.output_kind(), ProfileKind::Srgb);

    let samples = [0.0, 0.002, 0.02, 0.040_45, 0.1, 0.25, 0.5, 0.75, 0.95, 1.0];
    let mut rgb = Vec::new();
    for value in samples {
        rgb.extend([value, value, value]);
    }
    let original = rgb.clone();

    pipeline.to_linear_srgb(&mut rgb).unwrap();
    for (linear, encoded) in rgb.iter().zip(&original) {
        assert!(
            (linear - srgb_decode(*encoded)).abs() < 1e-6,
            "decode {encoded}: {linear}"
        );
    }

    pipeline.to_output(&mut rgb).unwrap();
    for (round_trip, encoded) in rgb.iter().zip(&original) {
        assert!(
            (round_trip - encoded).abs() < 1e-6,
            "round trip {encoded}: {round_trip}"
        );
    }
}

#[test]
fn srgb_encode_matches_reference() {
    for (linear, encoded) in [(0.0, 0.0), (0.214_041_14, 0.5), (1.0, 1.0)] {
        assert!((srgb_encode(linear) - encoded).abs() < 1e-6);
    }
}

#[test]
fn linear_srgb_profile_passes_through_unchanged() {
    let icc = linear_srgb_profile();
    let pipeline = ColorPipeline::new(Some(&icc), None, None).unwrap();
    assert_eq!(pipeline.input().kind, ProfileKind::LinearSrgb);

    let mut rgb = vec![0.0, 0.25, 1.0, 0.5, 0.5, 0.5];
    let original = rgb.clone();
    pipeline.to_linear_srgb(&mut rgb).unwrap();
    assert_eq!(rgb, original);
    pipeline.to_output(&mut rgb).unwrap();
    assert_eq!(rgb, original);
    assert_eq!(pipeline.output_icc(), Some(icc.as_slice()));
}

#[test]
fn luma_matches_reference() {
    assert!((encoded_luma(0.5, 0.25, 0.75) - 0.339_25).abs() < 1e-6);
    assert!((encoded_luma(1.0, 0.0, 0.0) - 0.2126).abs() < 1e-6);
    assert!((encoded_luma(0.0, 1.0, 0.0) - 0.7152).abs() < 1e-6);
    assert!((encoded_luma(0.0, 0.0, 1.0) - 0.0722).abs() < 1e-6);
    assert!((encoded_luma(1.0, 1.0, 1.0) - 1.0).abs() < 1e-6);
}

#[test]
fn absent_profile_assumes_srgb() {
    let resolved = resolve_input(None, None).unwrap();
    assert_eq!(resolved.kind, ProfileKind::Srgb);
    assert!(resolved.missing);
    assert!(!resolved.assumed);
    assert!(resolved.icc.is_none());
    assert_eq!(
        resolved.warning(),
        Some("no embedded ICC profile; assuming sRGB")
    );

    let pipeline = ColorPipeline::new(None, None, None).unwrap();
    assert!(pipeline.input().missing);
    assert!(pipeline.output_icc().is_none());

    let mut rgb = vec![0.5, 0.5, 0.5];
    pipeline.to_linear_srgb(&mut rgb).unwrap();
    assert!((rgb[0] - srgb_decode(0.5)).abs() < 1e-6);
}

#[test]
fn unknown_profile_errors_without_assume_profile() {
    let icc = unknown_profile();
    assert_eq!(detect_profile(&icc).unwrap(), ProfileKind::Unknown);
    assert_eq!(
        resolve_input(Some(&icc), None),
        Err(ColorError::UnknownProfile)
    );
    assert!(matches!(
        ColorPipeline::new(Some(&icc), None, None),
        Err(ColorError::UnknownProfile)
    ));
}

#[test]
fn assume_profile_overrides_unknown_profile() {
    let unknown = unknown_profile();
    let assumed = srgb_icc();
    let resolved = resolve_input(Some(&unknown), Some(&assumed)).unwrap();
    assert_eq!(resolved.kind, ProfileKind::Srgb);
    assert!(resolved.assumed);
    assert!(!resolved.missing);
    assert_eq!(resolved.icc.as_deref(), Some(assumed.as_slice()));

    let pipeline = ColorPipeline::new(Some(&unknown), Some(&assumed), None).unwrap();
    assert_eq!(pipeline.input().kind, ProfileKind::Srgb);
    assert_eq!(pipeline.output_icc(), Some(assumed.as_slice()));

    let mut rgb = vec![0.5, 0.25, 0.75];
    pipeline.to_linear_srgb(&mut rgb).unwrap();
    assert!((rgb[0] - srgb_decode(0.5)).abs() < 1e-6);
}

#[test]
fn assume_profile_supplies_absent_profile() {
    let assumed = adobe_rgb_profile();
    let resolved = resolve_input(None, Some(&assumed)).unwrap();
    assert_eq!(resolved.kind, ProfileKind::AdobeRgb);
    assert!(resolved.assumed);
    assert!(resolved.missing);
    assert_eq!(resolved.icc.as_deref(), Some(assumed.as_slice()));
    assert_eq!(
        resolved.warning(),
        Some("no embedded ICC profile; using --assume-profile")
    );
}

#[test]
fn assume_profile_must_be_known() {
    let assumed = unknown_profile();
    assert_eq!(
        resolve_input(None, Some(&assumed)),
        Err(ColorError::UnknownProfile)
    );
}

#[test]
fn wide_gamut_round_trip_preserves_profile_bytes() {
    let icc = adobe_rgb_profile();
    let pipeline = ColorPipeline::new(Some(&icc), None, None).unwrap();
    assert_eq!(pipeline.input().kind, ProfileKind::AdobeRgb);
    assert_eq!(pipeline.output_icc(), Some(icc.as_slice()));

    let original = vec![0.5, 0.3, 0.2, 0.2, 0.5, 0.4, 0.6, 0.6, 0.6, 0.05, 0.1, 0.15];
    let mut rgb = original.clone();
    pipeline.to_linear_srgb(&mut rgb).unwrap();
    pipeline.to_output(&mut rgb).unwrap();
    for (round_trip, encoded) in rgb.iter().zip(&original) {
        assert!(
            (round_trip - encoded).abs() < 1e-5,
            "round trip {encoded}: {round_trip}"
        );
    }
}

#[test]
fn output_profile_converts_instead_of_preserving() {
    let input = srgb_icc();
    let output = adobe_rgb_profile();
    let pipeline = ColorPipeline::new(Some(&input), None, Some(&output)).unwrap();
    assert_eq!(pipeline.input().kind, ProfileKind::Srgb);
    assert_eq!(pipeline.output_kind(), ProfileKind::AdobeRgb);
    assert_eq!(pipeline.output_icc(), Some(output.as_slice()));

    let mut rgb = vec![0.5, 0.5, 0.5];
    pipeline.to_linear_srgb(&mut rgb).unwrap();
    pipeline.to_output(&mut rgb).unwrap();

    // Neutral gray keeps its luminance; Adobe RGB re-encodes it with its
    // 563/256 gamma rather than sRGB's piecewise curve.
    let expected = srgb_decode(0.5).powf(256.0 / 563.0);
    for channel in rgb {
        assert!(
            (channel - expected).abs() < 1e-4,
            "gray channel: {channel}, expected {expected}"
        );
    }
}

#[test]
fn output_profile_must_be_known() {
    let input = srgb_icc();
    let output = unknown_profile();
    assert!(matches!(
        ColorPipeline::new(Some(&input), None, Some(&output)),
        Err(ColorError::UnknownProfile)
    ));
}

#[test]
fn rejects_non_rgb_buffers() {
    let icc = srgb_icc();
    let pipeline = ColorPipeline::new(Some(&icc), None, None).unwrap();
    let mut rgb = vec![0.0; 4];
    assert_eq!(
        pipeline.to_linear_srgb(&mut rgb),
        Err(ColorError::BufferLength(4))
    );
    assert_eq!(
        pipeline.to_output(&mut rgb),
        Err(ColorError::BufferLength(4))
    );
}
