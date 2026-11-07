//! Report schema, determinism, and input hashing (`docs/outputs.md` §4).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::Similarity;
use focus_stack::align::diagnostics::ResidualStats;
use focus_stack::output::report::{
    AlignmentReport, CoverageBox, FocusReport, FrameAlignment, InputReport, MethodReport,
    OutputReport, ParamsReport, Report, SCHEMA_VERSION, TimingReport, sha256_file, sha256_hex,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-report-{}-{id}-{label}",
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

fn sample_report() -> Report {
    Report {
        tool: focus_stack::output::report::ToolInfo::default().with_git("abc1234"),
        inputs: vec![InputReport::new(
            "a.tif",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            6000,
            4000,
            16,
            1,
            Some("sRGB IEC61966-2.1".to_string()),
        )],
        params: ParamsReport {
            estimation_radius: Some(16),
            keep_detail: Some(0.5),
            ..ParamsReport::default()
        },
        alignment: AlignmentReport {
            reference_index: 1,
            frames: vec![FrameAlignment::new(
                Similarity {
                    a: 1.0,
                    b: 0.0,
                    tx: 0.42,
                    ty: -1.13,
                },
                4.2,
                0.998,
            )],
            residual_px: ResidualStats {
                median: 0.04,
                p95: 0.11,
            },
            warnings: vec!["alignment warning".to_string()],
        },
        focus: FocusReport {
            levels: 6,
            noise_sigma: vec![vec![0.0012, 0.0004, 0.0002]],
            threshold: 0.0031,
            confident_pct: 0.74,
            coverage_gap_pct: 0.02,
            coverage_boxes: vec![CoverageBox::new(10, 20, 30, 40)],
            warnings: vec!["focus warning".to_string()],
        },
        method: MethodReport {
            chosen: "hybrid".to_string(),
            reason: "confident_pct=0.74".to_string(),
        },
        output: OutputReport {
            path: "out.tif".to_string(),
            sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            clipped_pct: 0.0,
            profile: Some("sRGB IEC61966-2.1".to_string()),
        },
        warnings: vec!["run warning".to_string()],
        ..Report::default()
    }
}

#[test]
fn report_matches_the_documented_schema() {
    let report = sample_report();
    let json = parse_json(&report.to_json());

    assert_eq!(
        json.get("schema_version").as_u64(),
        u64::from(SCHEMA_VERSION)
    );
    assert_eq!(json.keys(), expected_top_level_keys(false));

    let tool = json.get("tool");
    assert_eq!(tool.get("name").as_str(), "focus-stack");
    assert_eq!(tool.get("version").as_str(), focus_stack::VERSION);
    assert_eq!(tool.get("git").as_str(), "abc1234");

    let inputs = json.get("inputs").as_array();
    assert_eq!(inputs.len(), 1);
    let input = &inputs[0];
    assert_eq!(input.get("path").as_str(), "a.tif");
    assert_eq!(input.get("width").as_u64(), 6000);
    assert_eq!(input.get("height").as_u64(), 4000);
    assert_eq!(input.get("bits").as_u64(), 16);
    assert_eq!(input.get("orientation").as_u64(), 1);
    assert_eq!(input.get("profile").as_str(), "sRGB IEC61966-2.1");
    assert_eq!(input.get("sha256").as_str().len(), 64);

    let params = json.get("params");
    assert_eq!(params.get("method").as_str(), "auto");
    assert_eq!(params.get("align").as_str(), "similarity");
    assert_eq!(params.get("reference").as_str(), "auto");
    assert_eq!(params.get("estimation_radius").as_u64(), 16);
    assert!((params.get("keep_detail").as_f64() - 0.5).abs() < 1.0e-6);
    assert!((params.get("detail_transfer").as_f64() - 0.7).abs() < 1.0e-6);
    assert_eq!(params.get("exposure_match").as_str(), "off");
    assert_eq!(params.get("compression").as_str(), "lzw");
    assert_eq!(params.get("output_profile").as_str(), "same");
    assert_eq!(params.get("seed").as_u64(), 0);

    let alignment = json.get("alignment");
    assert_eq!(alignment.get("reference_index").as_u64(), 1);
    assert_eq!(alignment.get("transforms").as_array().len(), 1);
    assert_eq!(
        alignment.get("transforms").as_array()[0].get("a").as_f64(),
        1.0
    );
    assert!((alignment.get("residual_px").get("median").as_f64() - 0.04).abs() < 1.0e-9);
    assert_eq!(alignment.get("correlation").as_array()[0].as_f64(), 0.998);
    assert_eq!(alignment.get("warnings").as_array().len(), 1);

    let focus = json.get("focus");
    assert_eq!(focus.get("levels").as_u64(), 6);
    assert_eq!(focus.get("noise_sigma").as_array()[0].as_array().len(), 3);
    assert!((focus.get("threshold").as_f64() - 0.0031).abs() < 1.0e-7);
    assert!((focus.get("confident_pct").as_f64() - 0.74).abs() < 1.0e-6);
    assert!((focus.get("coverage_gap_pct").as_f64() - 0.02).abs() < 1.0e-6);
    let box_json = &focus.get("coverage_boxes").as_array()[0];
    assert_eq!(box_json.get("x").as_u64(), 10);
    assert_eq!(box_json.get("y").as_u64(), 20);
    assert_eq!(box_json.get("width").as_u64(), 30);
    assert_eq!(box_json.get("height").as_u64(), 40);

    assert_eq!(json.get("method").get("chosen").as_str(), "hybrid");
    assert_eq!(
        json.get("method").get("reason").as_str(),
        "confident_pct=0.74"
    );
    assert_eq!(json.get("output").get("path").as_str(), "out.tif");
    assert!((json.get("output").get("clipped_pct").as_f64() - 0.0).abs() < 1.0e-12);

    let warnings: Vec<&str> = json
        .get("warnings")
        .as_array()
        .iter()
        .map(Json::as_str)
        .collect();
    assert_eq!(
        warnings,
        ["run warning", "alignment warning", "focus warning"]
    );
}

#[test]
fn report_is_byte_identical_without_timings_or_stamp() {
    let first = sample_report().to_json();
    let second = sample_report().to_json();

    assert_eq!(first, second);
    assert!(!first.contains("\"timing_ms\""));
    assert!(!first.contains("\"created_at\""));

    let json = parse_json(&first);
    assert_eq!(json.keys(), expected_top_level_keys(false));
}

#[test]
fn timings_and_stamp_are_opt_in_and_change_bytes() {
    let baseline = sample_report().to_json();

    let mut report = sample_report();
    report.timing_ms = Some(TimingReport {
        load: 320,
        align: 410,
        focus: 880,
        stack: 1900,
        encode: 260,
    });
    let timed = report.to_json();
    assert_ne!(timed, baseline);
    assert_eq!(parse_json(&timed).keys(), expected_top_level_keys(true));
    let timing = parse_json(&timed);
    let timing = timing.get("timing_ms");
    assert_eq!(timing.get("load").as_u64(), 320);
    assert_eq!(timing.get("align").as_u64(), 410);
    assert_eq!(timing.get("focus").as_u64(), 880);
    assert_eq!(timing.get("stack").as_u64(), 1900);
    assert_eq!(timing.get("encode").as_u64(), 260);

    report.created_at = Some("2026-11-07T12:00:00Z".to_string());
    let stamped = report.to_json();
    assert_eq!(
        parse_json(&stamped).get("created_at").as_str(),
        "2026-11-07T12:00:00Z"
    );
}

#[test]
fn input_hashes_are_sha256_of_file_bytes() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );

    let dir = TempDir::new("sha256");
    let path = dir.join("input.bin");
    let bytes: Vec<u8> = (0..200_000u32)
        .map(|index| (index.wrapping_mul(31) & 0xff) as u8)
        .collect();
    fs::write(&path, &bytes).unwrap();

    assert_eq!(sha256_file(&path).unwrap(), sha256_hex(&bytes));
}

#[test]
fn warnings_are_deduplicated_in_first_seen_order() {
    let mut report = sample_report();
    report.warnings = vec!["shared".to_string(), "run".to_string()];
    report.alignment.warnings = vec!["shared".to_string(), "align".to_string()];
    report.focus.warnings = vec!["focus".to_string(), "shared".to_string()];

    assert_eq!(report.warnings(), ["shared", "run", "align", "focus"]);

    let json = parse_json(&report.to_json());
    let warnings: Vec<&str> = json
        .get("warnings")
        .as_array()
        .iter()
        .map(Json::as_str)
        .collect();
    assert_eq!(warnings, ["shared", "run", "align", "focus"]);
    assert_eq!(
        json.get("alignment").get("warnings").as_array()[0].as_str(),
        "shared"
    );
    assert_eq!(
        json.get("focus").get("warnings").as_array()[0].as_str(),
        "focus"
    );
}

fn expected_top_level_keys(with_timings: bool) -> Vec<&'static str> {
    let mut keys = vec![
        "schema_version",
        "tool",
        "inputs",
        "params",
        "alignment",
        "focus",
        "method",
        "output",
    ];
    if with_timings {
        keys.push("timing_ms");
    }
    keys.push("warnings");
    keys
}

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> &Self {
        match self {
            Self::Object(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map_or_else(|| panic!("missing key '{key}'"), |(_, value)| value),
            _ => panic!("value is not an object"),
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self {
            Self::Object(fields) => fields.iter().map(|(name, _)| name.as_str()).collect(),
            _ => panic!("value is not an object"),
        }
    }

    fn as_array(&self) -> &[Self] {
        match self {
            Self::Array(items) => items,
            _ => panic!("value is not an array"),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::String(value) => value,
            _ => panic!("value is not a string"),
        }
    }

    fn as_u64(&self) -> u64 {
        match self {
            Self::Number(value) => *value as u64,
            _ => panic!("value is not a number"),
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            Self::Number(value) => *value,
            _ => panic!("value is not a number"),
        }
    }
}

fn parse_json(input: &str) -> Json {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        position: 0,
    };
    let value = parser.value();
    parser.whitespace();
    assert_eq!(parser.position, input.len(), "trailing JSON data");
    value
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }

    fn value(&mut self) -> Json {
        self.whitespace();
        match self.bytes[self.position] {
            b'n' => {
                self.literal(b"null");
                Json::Null
            }
            b't' => {
                self.literal(b"true");
                Json::Bool(true)
            }
            b'f' => {
                self.literal(b"false");
                Json::Bool(false)
            }
            b'"' => Json::String(self.string()),
            b'[' => self.array(),
            b'{' => self.object(),
            _ => self.number(),
        }
    }

    fn literal(&mut self, literal: &[u8]) {
        assert_eq!(
            &self.bytes[self.position..self.position + literal.len()],
            literal
        );
        self.position += literal.len();
    }

    fn string(&mut self) -> String {
        assert_eq!(self.bytes[self.position], b'"');
        self.position += 1;
        let mut out = String::new();
        loop {
            match self.bytes[self.position] {
                b'"' => {
                    self.position += 1;
                    return out;
                }
                b'\\' => {
                    self.position += 1;
                    let escape = self.bytes[self.position];
                    self.position += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let digits = &self.bytes[self.position..self.position + 4];
                            let text = std::str::from_utf8(digits).unwrap();
                            let code = u32::from_str_radix(text, 16).unwrap();
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            self.position += 4;
                        }
                        other => panic!("unsupported escape '\\{}'", char::from(other)),
                    }
                }
                _ => {
                    let rest = std::str::from_utf8(&self.bytes[self.position..]).unwrap();
                    let ch = rest.chars().next().unwrap();
                    out.push(ch);
                    self.position += ch.len_utf8();
                }
            }
        }
    }

    fn number(&mut self) -> Json {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
        {
            self.position += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position]).unwrap();
        Json::Number(text.parse().unwrap())
    }

    fn array(&mut self) -> Json {
        assert_eq!(self.bytes[self.position], b'[');
        self.position += 1;
        let mut items = Vec::new();
        self.whitespace();
        if self.bytes[self.position] == b']' {
            self.position += 1;
            return Json::Array(items);
        }
        loop {
            items.push(self.value());
            self.whitespace();
            match self.bytes[self.position] {
                b',' => self.position += 1,
                b']' => {
                    self.position += 1;
                    return Json::Array(items);
                }
                other => panic!("unexpected byte in array: {}", char::from(other)),
            }
        }
    }

    fn object(&mut self) -> Json {
        assert_eq!(self.bytes[self.position], b'{');
        self.position += 1;
        let mut fields = Vec::new();
        self.whitespace();
        if self.bytes[self.position] == b'}' {
            self.position += 1;
            return Json::Object(fields);
        }
        loop {
            self.whitespace();
            let key = self.string();
            self.whitespace();
            assert_eq!(self.bytes[self.position], b':');
            self.position += 1;
            let value = self.value();
            fields.push((key, value));
            self.whitespace();
            match self.bytes[self.position] {
                b',' => self.position += 1,
                b'}' => {
                    self.position += 1;
                    return Json::Object(fields);
                }
                other => panic!("unexpected byte in object: {}", char::from(other)),
            }
        }
    }
}
