# Session log

Append-only. One entry per completed (or blocked) session. Newest at the bottom.

Format:

```markdown
## S<ID> — YYYY-MM-DD

- Scope:
- Changed:
- Evidence:
- Decisions:
- Follow-ups:
```

---

## S0.1 — 2026-11-07

- Scope: repository scaffold and CI
- Changed: `Cargo.toml`, `rust-toolchain.toml`, `src/main.rs`, `src/lib.rs`, module skeleton under `src/`, `.github/workflows/ci.yml`, `.gitignore`, `LICENSE`
- Evidence: `cargo fmt --all --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (2 passed)
- Decisions: pinned Rust 1.99.1; MIT license; module tree mirrors `docs/architecture.md` §2; no runtime dependencies yet; CI runs on `macos-15` (arm64) and asserts `uname -m`
- Follow-ups: confirm the GitHub Actions run is green after the loop pushes

## S0.2 — 2026-11-07

- Scope: TIFF reader
- Changed: `Cargo.toml`, `Cargo.lock`, `src/io/tiff_read.rs`, `src/io/exif.rs`, `tests/io_read.rs`, `fixtures/io/*`
- Evidence: `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test` (11 unit + 11 integration passed); fixtures cover 8/16-bit RGB, strips/tiles, none/LZW/Deflate/PackBits, predictor 2, BigTIFF, ICC bytes, orientation 1/6/8, and grayscale; CMYK/planar/LZMA/truncated inputs fail with named errors
- Decisions: D011 `tiff` crate with only `deflate` and `lzw` features; orientation is baked upright while `FrameMeta.orientation` keeps the source tag; chunky unsigned 8/16-bit only, alpha dropped (associated alpha unpremultiplied); EXIF payload copy deferred to S6.2
- Follow-ups: float TIFF and planar-separate decoding remain unsupported with clear errors; a real 24 MP Lightroom export still needs `scripts/fetch_fixtures.sh`

## S0.3 — 2026-11-07

- Scope: TIFF writer and round-trip
- Changed: `src/io/tiff_write.rs`, `src/io/metadata.rs`, `tests/io_roundtrip.rs`, `src/lib.rs`, `src/io/tiff_read.rs` (resolution fields required for the copy policy)
- Evidence: `cargo fmt --all --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (13 unit + 11 read + 6 round-trip passed); all orientation=1 fixtures round-trip pixel- and ICC-identical; macOS ImageIO (`sips`) opened a generated 16-bit LZW output with the embedded sRGB profile
- Decisions: classic stripped TIFF, `Orientation = 1`, `SampleFormat = Unsigned`, default LZW and 16-bit; none/LZW/Deflate selectable; 8-bit output supported for the documented `--bits 8` round trip; metadata policy copies ICC/XMP/resolution and strips XMP with `Strip`; EXIF remains deferred to S6.2
- Follow-ups: Lightroom/Photoshop GUI opening was not available on this machine; ImageIO validation used as the automated proxy. EXIF/IPTC/Software reporting lands in S6.2.
