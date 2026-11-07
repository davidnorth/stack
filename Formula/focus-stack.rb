class FocusStack < Formula
  desc "Professional focus stacking for macOS Apple Silicon"
  homepage "https://github.com/davidnorth/stack"
  url "https://github.com/davidnorth/stack/releases/download/v0.1.0/focus-stack-0.1.0-aarch64-apple-darwin.tar.gz"
  sha256 "2a1d0ecb33e904b446a134346beffeb3f3d7a4c48df8e92c0da59e81c71b3a46"
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
