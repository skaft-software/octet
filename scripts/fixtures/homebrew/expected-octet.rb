# Generated from verified immutable octet release metadata.
# Release tag: v0.7.0
# Release source commit: 0123456789abcdef0123456789abcdef01234567
# Release workflow commit: abcdef0123456789abcdef0123456789abcdef01
# Release workflow ref: skaft-software/octet/.github/workflows/release-octet.yml@refs/tags/octet-binaries-v0.7.0
# OCTET_SHA256SUMS SHA-256: 113e5d14f78a22fee2bb525b25be57ed512aa2610761d5a1a60d43b410f163b3
class Octet < Formula
  desc "High-performance coding agent"
  homepage "https://github.com/skaft-software/octet"
  version "0.7.0"
  depends_on :macos
  depends_on "ripgrep"

  on_arm do
    url "https://github.com/skaft-software/octet/releases/download/v0.7.0/octet-0.7.0-aarch64-apple-darwin.tar.gz"
    sha256 "e7ab051b6c1c9e079171c9f264c39d954116cae49944eec831aebccac8da986b"
  end

  on_intel do
    url "https://github.com/skaft-software/octet/releases/download/v0.7.0/octet-0.7.0-x86_64-apple-darwin.tar.gz"
    sha256 "533d5d66c05286dfc249c5464d60129193b98fe810f9e6bd10a96408b2164ea4"
  end

  def install
    root = Dir["octet-*/"].find { |candidate| File.executable?(File.join(candidate, "octet")) }
    odie "octet release archive has no executable octet binary" unless root
    bin.install File.join(root, "octet")
    bin.install File.join(root, "octet-host")
  end

  test do
    assert_match "octet #{version}", shell_output("#{bin}/octet --version")
  end
end
