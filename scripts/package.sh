#!/bin/bash
# Build, ad-hoc sign, and package focus-stack for macOS Apple Silicon.
#
# Usage: scripts/package.sh
#
# Outputs:
#   target/package/focus-stack-<version>-aarch64-apple-darwin.tar.gz
#   Formula/focus-stack.rb (release URL plus the tarball checksum)
#
# The tarball is extracted into a clean temporary directory and
# `focus-stack --version` is run with a minimal environment to verify that the
# artifact works without the build tree or a Rust toolchain.

set -euo pipefail

cd "$(dirname "$0")/.."

TARGET="aarch64-apple-darwin"
DIST_DIR="${DIST_DIR:-target/package}"

fail() {
    printf 'package.sh: %s\n' "$*" >&2
    exit 1
}

if [[ "$(uname -s)" != "Darwin" ]]; then
    fail "macOS is required"
fi
if [[ "$(uname -m)" != "arm64" ]]; then
    fail "Apple Silicon (arm64) is required"
fi

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
[[ -n "$VERSION" ]] || fail "could not read version from Cargo.toml"

GIT_SHORT="$(git rev-parse --short HEAD 2>/dev/null || printf 'unknown')"
NAME="focus-stack-${VERSION}-${TARGET}"
STAGE="${DIST_DIR}/${NAME}"
TARBALL="${DIST_DIR}/${NAME}.tar.gz"
BIN="target/${TARGET}/release/focus-stack"
FORMULA="Formula/focus-stack.rb"

printf 'packaging focus-stack %s (%s)\n' "$VERSION" "$GIT_SHORT"

# docs/cli.md §6: thin LTO, one codegen unit, stripped symbols. Set these via
# the environment so the release profile stays out of Cargo.toml.
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_STRIP=true

cargo build --release --locked --target "$TARGET"
[[ -x "$BIN" ]] || fail "missing release binary at $BIN"

# Ad-hoc signature so Gatekeeper does not quarantine the local build.
codesign --force --sign - "$BIN"
codesign --verify --strict "$BIN"

rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/focus-stack"
cp README.md LICENSE "$STAGE/"

# Fixed ownership and timestamps keep the archive deterministic for the
# checksum recorded in the formula.
find "$STAGE" -exec touch -t 200001010000.00 {} +
COPYFILE_DISABLE=1 tar \
    --format ustar \
    --uid 0 --gid 0 --numeric-owner \
    -cf "${DIST_DIR}/${NAME}.tar" \
    -C "$DIST_DIR" "$NAME"
gzip -9n -c "${DIST_DIR}/${NAME}.tar" > "$TARBALL"
rm -f "${DIST_DIR}/${NAME}.tar"

SHA256="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"

# Verify the artifact the way a clean machine consumes it: extract elsewhere,
# strip the environment, and invoke the binary.
VERIFY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/focus-stack-verify.XXXXXX")"
cleanup() {
    rm -rf "$VERIFY_DIR"
}
trap cleanup EXIT

tar -xzf "$TARBALL" -C "$VERIFY_DIR"
VERIFY_BIN="${VERIFY_DIR}/${NAME}/focus-stack"
[[ -x "$VERIFY_BIN" ]] || fail "tarball does not contain focus-stack"
codesign --verify --strict "$VERIFY_BIN"

VERSION_OUTPUT="$(env -i PATH=/usr/bin:/bin HOME="$VERIFY_DIR" "$VERIFY_BIN" --version)"
case "$VERSION_OUTPUT" in
    "focus-stack ${VERSION}"*) ;;
    *) fail "unexpected --version output: $VERSION_OUTPUT" ;;
esac

mkdir -p Formula
cat > "$FORMULA" <<RUBY
class FocusStack < Formula
  desc "Professional focus stacking for macOS Apple Silicon"
  homepage "https://github.com/davidnorth/stack"
  url "https://github.com/davidnorth/stack/releases/download/v${VERSION}/${NAME}.tar.gz"
  sha256 "${SHA256}"
  license "MIT"

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "focus-stack"
    doc.install "README.md"
    doc.install "LICENSE"
  end

  test do
    assert_match "focus-stack #{version}", shell_output("#{bin}/focus-stack --version")
  end
end
RUBY

printf '\nartifact: %s\n' "$TARBALL"
printf 'sha256:   %s\n' "$SHA256"
printf 'formula:  %s\n' "$FORMULA"
printf 'verified: %s\n' "$VERSION_OUTPUT"
