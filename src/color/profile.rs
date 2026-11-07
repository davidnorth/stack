//! ICC detection and conversion.
//!
//! The working space is linear-light sRGB, D65, `f32` (`docs/io-and-color.md`
//! §6). Input profiles are identified from their ICC description, with a
//! colorant/TRC fallback for profiles that lack a useful description.
//! Wide-gamut conversion is delegated to bundled `lcms2`; sRGB and linear
//! sRGB use the exact transfer functions in [`super::transfer`].

use std::fmt;

use lcms2::{
    CIEXYZ, CIExyY, CIExyYTRIPLE, ColorSpaceSignature, InfoType, Intent, Locale, PixelFormat,
    Profile, Tag, TagSignature, ToneCurve, Transform,
};

use super::transfer::{srgb_decode, srgb_encode};

type FailedTransform = Transform<[f32; 3], [f32; 3]>;

/// Known RGB profile families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    Srgb,
    LinearSrgb,
    AdobeRgb,
    ProPhoto,
    DisplayP3,
    Unknown,
}

impl ProfileKind {
    /// Human-readable name used in warnings and reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB",
            Self::LinearSrgb => "linear sRGB",
            Self::AdobeRgb => "Adobe RGB",
            Self::ProPhoto => "ProPhoto RGB",
            Self::DisplayP3 => "Display P3",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the profile can be converted to the linear sRGB working space.
    #[must_use]
    pub fn is_known(self) -> bool {
        self != Self::Unknown
    }
}

/// Color-management failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorError {
    /// The ICC payload is too short, has a bad header, or fails to parse.
    MalformedProfile(String),
    /// A valid RGB profile that is not one of the supported families.
    UnknownProfile,
    /// A non-RGB profile was supplied as input or output.
    UnsupportedColorSpace,
    /// The pixel buffer is not an interleaved RGB triple.
    BufferLength(usize),
    /// `lcms2` could not build a transform.
    Transform(String),
}

impl fmt::Display for ColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedProfile(reason) => write!(f, "malformed ICC profile: {reason}"),
            Self::UnknownProfile => write!(
                f,
                "unknown ICC profile; use --assume-profile <path> to identify it"
            ),
            Self::UnsupportedColorSpace => write!(f, "unsupported ICC color space; expected RGB"),
            Self::BufferLength(len) => {
                write!(f, "RGB buffer length {len} is not a multiple of 3")
            }
            Self::Transform(reason) => write!(f, "color transform failed: {reason}"),
        }
    }
}

impl std::error::Error for ColorError {}

/// The input profile after applying the absent/unknown policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub kind: ProfileKind,
    /// Profile bytes used for the conversion. `None` means the built-in sRGB
    /// fallback for an absent profile.
    pub icc: Option<Vec<u8>>,
    /// `--assume-profile` supplied the profile.
    pub assumed: bool,
    /// The source had no embedded ICC profile; sRGB was assumed.
    pub missing: bool,
}

impl ResolvedProfile {
    /// Warning to emit when the source lacks an ICC profile.
    #[must_use]
    pub fn warning(&self) -> Option<&'static str> {
        match (self.missing, self.assumed) {
            (true, true) => Some("no embedded ICC profile; using --assume-profile"),
            (true, false) => Some("no embedded ICC profile; assuming sRGB"),
            _ => None,
        }
    }
}

/// Resolve an embedded ICC profile according to `docs/io-and-color.md` §6.
///
/// - known profiles are used as-is;
/// - an absent profile assumes sRGB (`missing = true`);
/// - an unknown profile errors unless `assume_icc` is supplied.
pub fn resolve_input(
    embedded: Option<&[u8]>,
    assume_icc: Option<&[u8]>,
) -> Result<ResolvedProfile, ColorError> {
    match embedded {
        Some(bytes) => match detect_profile(bytes)? {
            ProfileKind::Unknown => match assume_icc {
                Some(assumed) => {
                    let kind = detect_profile(assumed)?;
                    if !kind.is_known() {
                        return Err(ColorError::UnknownProfile);
                    }
                    Ok(ResolvedProfile {
                        kind,
                        icc: Some(assumed.to_vec()),
                        assumed: true,
                        missing: false,
                    })
                }
                None => Err(ColorError::UnknownProfile),
            },
            kind => Ok(ResolvedProfile {
                kind,
                icc: Some(bytes.to_vec()),
                assumed: false,
                missing: false,
            }),
        },
        None => match assume_icc {
            Some(assumed) => {
                let kind = detect_profile(assumed)?;
                if !kind.is_known() {
                    return Err(ColorError::UnknownProfile);
                }
                Ok(ResolvedProfile {
                    kind,
                    icc: Some(assumed.to_vec()),
                    assumed: true,
                    missing: true,
                })
            }
            None => Ok(ResolvedProfile {
                kind: ProfileKind::Srgb,
                icc: None,
                assumed: false,
                missing: true,
            }),
        },
    }
}

/// Identify a supported RGB profile from its ICC bytes.
pub fn detect_profile(bytes: &[u8]) -> Result<ProfileKind, ColorError> {
    validate_header(bytes)?;
    let profile =
        Profile::new_icc(bytes).map_err(|err| ColorError::MalformedProfile(err.to_string()))?;
    if profile.color_space() != ColorSpaceSignature::RgbData {
        return Err(ColorError::UnsupportedColorSpace);
    }

    if let Some(description) = profile.info(InfoType::Description, Locale::none())
        && let Some(kind) = classify_description(&description)
    {
        return Ok(kind);
    }

    if let Some(colorants) = read_colorants(&profile)
        && let Some(kind) = classify_colorants(&colorants)
    {
        if kind == ProfileKind::Srgb && has_linear_trc(&profile) {
            return Ok(ProfileKind::LinearSrgb);
        }
        return Ok(kind);
    }

    Ok(ProfileKind::Unknown)
}

/// A ready-to-use input/output color pipeline.
///
/// Create it with the embedded ICC bytes, the optional `--assume-profile`
/// bytes, and the optional `--output-profile` bytes. [`Self::output_icc`]
/// returns the bytes to write: the explicit output profile, or the original
/// input bytes when the profile is preserved.
pub struct ColorPipeline {
    input: ResolvedProfile,
    input_to_linear: Option<FailedTransform>,
    output_kind: ProfileKind,
    linear_to_output: Option<FailedTransform>,
    output_icc: Option<Vec<u8>>,
}

impl ColorPipeline {
    /// Build transforms for the given profiles.
    ///
    /// `output_icc == None` preserves the input profile.
    pub fn new(
        input_icc: Option<&[u8]>,
        assume_icc: Option<&[u8]>,
        output_icc: Option<&[u8]>,
    ) -> Result<Self, ColorError> {
        let input = resolve_input(input_icc, assume_icc)?;

        let input_to_linear = match input.kind {
            ProfileKind::Srgb | ProfileKind::LinearSrgb => None,
            _ => Some(build_transform(
                input
                    .icc
                    .as_deref()
                    .ok_or(ColorError::UnsupportedColorSpace)?,
                true,
            )?),
        };

        let explicit_output = output_icc.map(<[u8]>::to_vec);
        let output_kind = match explicit_output.as_deref() {
            Some(bytes) => detect_profile(bytes)?,
            None => input.kind,
        };
        if !output_kind.is_known() {
            return Err(ColorError::UnknownProfile);
        }

        let linear_to_output = match output_kind {
            ProfileKind::Srgb | ProfileKind::LinearSrgb => None,
            _ => Some(build_transform(
                explicit_output
                    .as_deref()
                    .or(input.icc.as_deref())
                    .ok_or(ColorError::UnsupportedColorSpace)?,
                false,
            )?),
        };

        let output_icc = explicit_output.or_else(|| input.icc.clone());

        Ok(Self {
            input,
            input_to_linear,
            output_kind,
            linear_to_output,
            output_icc,
        })
    }

    /// The resolved input profile.
    #[must_use]
    pub fn input(&self) -> &ResolvedProfile {
        &self.input
    }

    /// The profile that output pixels are encoded in.
    #[must_use]
    pub fn output_kind(&self) -> ProfileKind {
        self.output_kind
    }

    /// ICC bytes to write, or `None` when no profile was present and none was
    /// requested.
    #[must_use]
    pub fn output_icc(&self) -> Option<&[u8]> {
        self.output_icc.as_deref()
    }

    /// Convert encoded input RGB to linear-light sRGB, in place.
    pub fn to_linear_srgb(&self, rgb: &mut [f32]) -> Result<(), ColorError> {
        if !rgb.len().is_multiple_of(3) {
            return Err(ColorError::BufferLength(rgb.len()));
        }
        match self.input.kind {
            ProfileKind::Srgb => {
                for channel in rgb.iter_mut() {
                    *channel = srgb_decode(*channel);
                }
            }
            ProfileKind::LinearSrgb => {}
            _ => {
                if let Some(transform) = &self.input_to_linear {
                    transform.transform_in_place(rgb.as_chunks_mut::<3>().0);
                }
            }
        }
        Ok(())
    }

    /// Convert linear-light sRGB to the output profile's encoding, in place.
    pub fn to_output(&self, rgb: &mut [f32]) -> Result<(), ColorError> {
        if !rgb.len().is_multiple_of(3) {
            return Err(ColorError::BufferLength(rgb.len()));
        }
        match self.output_kind {
            ProfileKind::Srgb => {
                for channel in rgb.iter_mut() {
                    *channel = srgb_encode(*channel);
                }
            }
            ProfileKind::LinearSrgb => {}
            _ => {
                if let Some(transform) = &self.linear_to_output {
                    transform.transform_in_place(rgb.as_chunks_mut::<3>().0);
                }
            }
        }
        Ok(())
    }
}

fn validate_header(bytes: &[u8]) -> Result<(), ColorError> {
    if bytes.len() < 132 {
        return Err(ColorError::MalformedProfile(
            "shorter than the minimum ICC profile size".to_owned(),
        ));
    }
    let declared = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if declared < 132 || declared > bytes.len() {
        return Err(ColorError::MalformedProfile(format!(
            "declared size {declared} does not fit {} bytes",
            bytes.len()
        )));
    }
    if &bytes[36..40] != b"acsp" {
        return Err(ColorError::MalformedProfile(
            "missing 'acsp' signature".to_owned(),
        ));
    }
    Ok(())
}

fn classify_description(description: &str) -> Option<ProfileKind> {
    let compact: String = description
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();

    if compact.contains("adobergb") || (compact.contains("adobe") && compact.contains("rgb")) {
        return Some(ProfileKind::AdobeRgb);
    }
    if compact.contains("prophoto") {
        return Some(ProfileKind::ProPhoto);
    }
    if compact.contains("displayp3") || compact == "p3" || compact.contains("p3d65") {
        return Some(ProfileKind::DisplayP3);
    }
    if compact.contains("srgb") {
        return Some(if compact.contains("linear") {
            ProfileKind::LinearSrgb
        } else {
            ProfileKind::Srgb
        });
    }
    if compact.contains("linearrgb") || (compact.contains("linear") && compact.contains("rgb")) {
        return Some(ProfileKind::LinearSrgb);
    }
    None
}

fn read_colorants(profile: &Profile) -> Option<[[f64; 3]; 3]> {
    let mut colorants = [[0.0; 3]; 3];
    for (slot, signature) in colorants.iter_mut().zip([
        TagSignature::RedColorantTag,
        TagSignature::GreenColorantTag,
        TagSignature::BlueColorantTag,
    ]) {
        let CIEXYZ { X, Y, Z } = match profile.read_tag(signature) {
            Tag::CIEXYZ(value) => *value,
            _ => return None,
        };
        *slot = [X, Y, Z];
    }
    Some(colorants)
}

/// Standard D50-adapted ICC colorant tags for the supported spaces.
const SRGB_COLORANTS: [[f64; 3]; 3] = [
    [0.436_074_7, 0.222_504_5, 0.013_932_2],
    [0.385_064_9, 0.716_878_6, 0.097_104_5],
    [0.143_080_4, 0.060_616_9, 0.714_173_3],
];
const ADOBE_RGB_COLORANTS: [[f64; 3]; 3] = [
    [0.609_755_9, 0.311_124_2, 0.019_481_1],
    [0.205_240_1, 0.625_656_0, 0.060_890_2],
    [0.149_224_0, 0.063_219_7, 0.744_838_7],
];
const PROPHOTO_COLORANTS: [[f64; 3]; 3] = [
    [0.797_674_9, 0.288_040_2, 0.0],
    [0.135_191_7, 0.711_874_1, 0.0],
    [0.031_353_4, 0.000_085_7, 0.825_210_0],
];
const DISPLAY_P3_COLORANTS: [[f64; 3]; 3] = [
    [0.515_118_8, 0.241_189_1, -0.001_050_2],
    [0.291_977_1, 0.692_244_3, 0.041_884_0],
    [0.157_103_5, 0.066_569_1, 0.784_058_8],
];

fn classify_colorants(colorants: &[[f64; 3]; 3]) -> Option<ProfileKind> {
    for (kind, reference) in [
        (ProfileKind::Srgb, &SRGB_COLORANTS),
        (ProfileKind::AdobeRgb, &ADOBE_RGB_COLORANTS),
        (ProfileKind::ProPhoto, &PROPHOTO_COLORANTS),
        (ProfileKind::DisplayP3, &DISPLAY_P3_COLORANTS),
    ] {
        if colorants
            .iter()
            .flatten()
            .zip(reference.iter().flatten())
            .all(|(actual, expected)| (actual - expected).abs() < 5e-3)
        {
            return Some(kind);
        }
    }
    None
}

fn has_linear_trc(profile: &Profile) -> bool {
    [
        TagSignature::RedTRCTag,
        TagSignature::GreenTRCTag,
        TagSignature::BlueTRCTag,
    ]
    .into_iter()
    .all(|signature| match profile.read_tag(signature) {
        Tag::ToneCurve(curve) => curve.is_linear(),
        _ => false,
    })
}

fn build_transform(icc: &[u8], to_linear: bool) -> Result<FailedTransform, ColorError> {
    let profile =
        Profile::new_icc(icc).map_err(|err| ColorError::MalformedProfile(err.to_string()))?;
    if profile.color_space() != ColorSpaceSignature::RgbData {
        return Err(ColorError::UnsupportedColorSpace);
    }
    let linear = linear_srgb_profile()?;
    let result = if to_linear {
        Transform::new(
            &profile,
            PixelFormat::RGB_FLT,
            &linear,
            PixelFormat::RGB_FLT,
            Intent::RelativeColorimetric,
        )
    } else {
        Transform::new(
            &linear,
            PixelFormat::RGB_FLT,
            &profile,
            PixelFormat::RGB_FLT,
            Intent::RelativeColorimetric,
        )
    };
    result.map_err(|err| ColorError::Transform(err.to_string()))
}

fn linear_srgb_profile() -> Result<Profile, ColorError> {
    let white = CIExyY {
        x: 0.312_7,
        y: 0.329_0,
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: CIExyY {
            x: 0.64,
            y: 0.33,
            Y: 1.0,
        },
        Green: CIExyY {
            x: 0.30,
            y: 0.60,
            Y: 1.0,
        },
        Blue: CIExyY {
            x: 0.15,
            y: 0.06,
            Y: 1.0,
        },
    };
    let curve = ToneCurve::new(1.0);
    Profile::new_rgb(&white, &primaries, &[&curve, &curve, &curve])
        .map_err(|err| ColorError::Transform(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_profile() {
        assert!(matches!(
            detect_profile(&[0; 64]),
            Err(ColorError::MalformedProfile(_))
        ));
    }

    #[test]
    fn rejects_bad_signature() {
        let mut bytes = vec![0u8; 132];
        bytes[0..4].copy_from_slice(&132u32.to_be_bytes());
        assert!(matches!(
            detect_profile(&bytes),
            Err(ColorError::MalformedProfile(_))
        ));
    }
}
