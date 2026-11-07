class FocusStack < Formula
  desc "Professional focus stacking for macOS Apple Silicon"
  homepage "https://github.com/davidnorth/stack"
  url "https://github.com/davidnorth/stack/releases/download/v0.1.0/focus-stack-0.1.0-aarch64-apple-darwin.tar.gz"
  sha256 "be2a7ace7567f4c7e3109b1c86e5d5e18a9fd5268885be3fa86ba82882a90196"
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
