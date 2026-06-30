#!/usr/bin/env bash
# Generate package-manager manifests (Homebrew formula + Scoop manifest) for a
# released version, pulling the per-asset SHA256 checksums from the published
# GitHub Release. Pure text generation — reproducible locally and in CI.
#
#   scripts/gen-manifests.sh <version-without-v> [output-dir]
#
# Emits <output-dir>/pwsh-autoupdate.rb and <output-dir>/pwsh-autoupdate.json.
set -euo pipefail

version="${1:?usage: gen-manifests.sh <version-without-v> [output-dir]}"
outdir="${2:-.}"
repo="mericstam/pwsh-autoupdate"
base="https://github.com/${repo}/releases/download/v${version}"

# Fetch and return just the hex digest from a published <asset>.sha256 file.
fetch_sha() {
  local asset="pwsh-autoupdate-v${version}-$1"
  curl -fsSL "${base}/${asset}.sha256" | awk '{print $1}'
}

sha_mac_arm="$(fetch_sha aarch64-apple-darwin.tar.gz)"
sha_mac_x64="$(fetch_sha x86_64-apple-darwin.tar.gz)"
sha_linux_x64="$(fetch_sha x86_64-unknown-linux-gnu.tar.gz)"
sha_win_x64="$(fetch_sha x86_64-pc-windows-msvc.zip)"

mkdir -p "${outdir}"

# --- Homebrew formula (macOS arm/intel + Linux intel) ----------------------
cat > "${outdir}/pwsh-autoupdate.rb" <<RUBY
# typed: false
# frozen_string_literal: true

# Homebrew formula for pwsh-autoupdate. Regenerated per release by
# scripts/gen-manifests.sh — do not edit by hand.
class PwshAutoupdate < Formula
  desc "Detect how PowerShell was installed and update (or install) it via the owning manager"
  homepage "https://github.com/${repo}"
  version "${version}"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "${base}/pwsh-autoupdate-v${version}-aarch64-apple-darwin.tar.gz"
      sha256 "${sha_mac_arm}"
    end
    on_intel do
      url "${base}/pwsh-autoupdate-v${version}-x86_64-apple-darwin.tar.gz"
      sha256 "${sha_mac_x64}"
    end
  end

  on_linux do
    on_intel do
      url "${base}/pwsh-autoupdate-v${version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${sha_linux_x64}"
    end
  end

  def install
    bin.install "pwsh-autoupdate"
  end

  test do
    assert_match "pwsh-autoupdate #{version}", shell_output("#{bin}/pwsh-autoupdate --version")
  end
end
RUBY

# --- Scoop manifest (Windows x64) ------------------------------------------
cat > "${outdir}/pwsh-autoupdate.json" <<JSON
{
  "version": "${version}",
  "description": "Detect how PowerShell was installed and update (or install) it via the owning manager",
  "homepage": "https://github.com/${repo}",
  "license": "MIT OR Apache-2.0",
  "architecture": {
    "64bit": {
      "url": "${base}/pwsh-autoupdate-v${version}-x86_64-pc-windows-msvc.zip",
      "hash": "${sha_win_x64}",
      "extract_dir": "pwsh-autoupdate-v${version}-x86_64-pc-windows-msvc"
    }
  },
  "bin": "pwsh-autoupdate.exe",
  "checkver": "github",
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "https://github.com/${repo}/releases/download/v\$version/pwsh-autoupdate-v\$version-x86_64-pc-windows-msvc.zip",
        "extract_dir": "pwsh-autoupdate-v\$version-x86_64-pc-windows-msvc"
      }
    },
    "hash": {
      "url": "\$url.sha256"
    }
  }
}
JSON

echo "wrote ${outdir}/pwsh-autoupdate.rb and ${outdir}/pwsh-autoupdate.json (v${version})"
