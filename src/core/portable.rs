//! Pure portable tar.gz update rules (A-6, FR-6).
//!
//! Everything here is IO-free: URL/asset-name construction, hash-manifest
//! decoding and lookup, and portable-layout path recognition. The adapter
//! (`adapters::portable`) drives the actual download / verify / extract /
//! replace steps against these rules.
//!
//! Security stance encoded here:
//! * The asset URL is **constructed** from the already-cross-checked resolved
//!   version (ADR-0003) — never taken from the GitHub API response — so a
//!   tampered API payload cannot redirect the download to an arbitrary host.
//! * The expected SHA-256 comes from the release's `hashes.sha256` manifest.
//!   That manifest ships from the same origin as the tarball, so it protects
//!   against corruption and CDN-level tampering — not against a full
//!   compromise of the GitHub release itself (TLS to `github.com` is the trust
//!   root, as it is for every package manager pulling from GitHub releases).

use semver::Version;

/// Pinned base for PowerShell release asset downloads. Only the tag path
/// segment varies, and it is derived from a parsed [`semver::Version`] — the
/// URL can never point outside this release tree.
pub const DOWNLOAD_BASE: &str = "https://github.com/PowerShell/PowerShell/releases/download";

/// Map the host CPU architecture (a `std::env::consts::ARCH` value, probed by
/// the adapter and passed in as data) to the label used in PowerShell's Linux
/// tar.gz asset names. Returns `None` for an architecture PowerShell does not
/// ship a portable Linux build for — the caller surfaces an honest error.
pub fn arch_label(rust_arch: &str) -> Option<&'static str> {
    match rust_arch {
        "x86_64" => Some("x64"),
        "aarch64" => Some("arm64"),
        "arm" => Some("arm32"),
        _ => None,
    }
}

/// The tar.gz asset name for a version + arch label, e.g.
/// `powershell-7.6.3-linux-x64.tar.gz`.
pub fn asset_name(version: &Version, arch: &str) -> String {
    format!("powershell-{version}-linux-{arch}.tar.gz")
}

/// The release tag for a resolved stable version (`v7.6.3`). Derived from the
/// parsed semver — never from a raw upstream string — so it is well-formed by
/// construction.
pub fn release_tag(version: &Version) -> String {
    format!("v{version}")
}

/// Constructed download URL for an asset of the given release tag.
pub fn asset_url(tag: &str, asset: &str) -> String {
    format!("{DOWNLOAD_BASE}/{tag}/{asset}")
}

/// Constructed download URL for the release's SHA-256 hash manifest.
pub fn hashes_url(tag: &str) -> String {
    format!("{DOWNLOAD_BASE}/{tag}/hashes.sha256")
}

/// Decode the raw `hashes.sha256` manifest bytes to text.
///
/// The live manifest is UTF-16LE with a BOM and CRLF line endings (verified
/// against the real v7.6.3 asset); this decoder also tolerates UTF-16BE, a
/// UTF-8 BOM, and plain UTF-8 so an upstream encoding change does not break
/// verification. Undecodable bytes are replaced, which can only cause a hash
/// lookup MISS (fail-closed), never a false match.
pub fn decode_hash_manifest(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8_lossy(text).into_owned()
}

/// Find the expected SHA-256 (lowercase hex) for `asset` in a decoded hash
/// manifest. Lines follow the `sha256sum` convention `<hex> *<filename>` (the
/// `*` marks binary mode; a plain space form is tolerated too). Returns `None`
/// when the asset has no well-formed 64-hex-digit entry — the caller must
/// treat that as a hard failure, never "skip verification".
pub fn expected_sha256(manifest: &str, asset: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        let mut parts = line.splitn(2, char::is_whitespace);
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        let name = name.trim().trim_start_matches('*');
        if name == asset && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Given the resolved pwsh binary path, return the portable install root if the
/// binary lives under a `.../powershell[/<version>]/pwsh` tree (the layout of a
/// manual tar.gz / user-local extraction). Pure: no IO, so it unit-tests on any
/// OS. Returns `None` for any other location (kept "undetermined" — never guess).
pub fn install_root_from_path(binary: &str) -> Option<String> {
    let parent = std::path::Path::new(binary).parent()?;
    let is_powershell_dir = |dir: &std::path::Path| {
        dir.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("powershell"))
    };
    // Layout A: <root=powershell>/pwsh  (binary directly in a `powershell` dir).
    if is_powershell_dir(parent) {
        return Some(parent.to_string_lossy().into_owned());
    }
    // Layout B: <root=powershell>/<version>/pwsh  (versioned subdir — the common
    // `~/.local/share/powershell/7.6.2/pwsh` and `/opt/microsoft/powershell/7`).
    let grandparent = parent.parent()?;
    if is_powershell_dir(grandparent) {
        return Some(grandparent.to_string_lossy().into_owned());
    }
    None
}

/// The directory whose CONTENTS the portable update replaces: the directory
/// the resolved binary lives in — accepted only when the layout is a
/// recognized portable tree ([`install_root_from_path`]), never guessed.
///
/// Replacing the binary's own directory (rather than creating a new versioned
/// sibling) keeps every existing pointer — PATH entries, `~/.local/bin/pwsh`
/// symlinks, shell hash caches — valid across the update, and matches what
/// Microsoft's own install script does to `/opt/microsoft/powershell/7`.
pub fn install_dir_from_binary(binary: &str) -> Option<String> {
    install_root_from_path(binary)?;
    Some(
        std::path::Path::new(binary)
            .parent()?
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn arch_labels_map_supported_cpus_only() {
        assert_eq!(arch_label("x86_64"), Some("x64"));
        assert_eq!(arch_label("aarch64"), Some("arm64"));
        assert_eq!(arch_label("arm"), Some("arm32"));
        // No portable Linux build exists for these — must be None, never a guess.
        assert_eq!(arch_label("riscv64"), None);
        assert_eq!(arch_label("s390x"), None);
        assert_eq!(arch_label(""), None);
    }

    #[test]
    fn asset_name_matches_upstream_convention() {
        assert_eq!(
            asset_name(&v("7.6.3"), "x64"),
            "powershell-7.6.3-linux-x64.tar.gz"
        );
        assert_eq!(
            asset_name(&v("7.4.0"), "arm64"),
            "powershell-7.4.0-linux-arm64.tar.gz"
        );
    }

    #[test]
    fn urls_are_constructed_under_the_pinned_release_tree() {
        let tag = release_tag(&v("7.6.3"));
        assert_eq!(tag, "v7.6.3");
        assert_eq!(
            asset_url(&tag, "powershell-7.6.3-linux-x64.tar.gz"),
            "https://github.com/PowerShell/PowerShell/releases/download/v7.6.3/powershell-7.6.3-linux-x64.tar.gz"
        );
        assert_eq!(
            hashes_url(&tag),
            "https://github.com/PowerShell/PowerShell/releases/download/v7.6.3/hashes.sha256"
        );
    }

    #[test]
    fn decodes_utf16le_bom_manifest() {
        // The live hashes.sha256 encoding: UTF-16LE with BOM, CRLF endings.
        let text = "abcd1234 *file.tar.gz\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_hash_manifest(&bytes), text);
    }

    #[test]
    fn decodes_utf16be_utf8_bom_and_plain_utf8() {
        let text = "aa *f\n";
        let mut be = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            be.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(decode_hash_manifest(&be), text);
        let mut u8bom = vec![0xEF, 0xBB, 0xBF];
        u8bom.extend_from_slice(text.as_bytes());
        assert_eq!(decode_hash_manifest(&u8bom), text);
        assert_eq!(decode_hash_manifest(text.as_bytes()), text);
    }

    const HASH_A: &str = "885fec414308983 2d9a1f3ef063e6f7cf3118e007";

    #[test]
    fn finds_expected_hash_in_sha256sum_binary_mode_lines() {
        let h =
            "856d0765d23323 77f9d7a4aea76efdfde4de51446e7738dde2dfda41dba9e2a7".replace(' ', "");
        let manifest = format!(
            "{}  *other-1.0.rpm\r\n{h} *powershell-7.6.3-linux-x64.tar.gz\r\n",
            "a".repeat(64)
        );
        assert_eq!(
            expected_sha256(&manifest, "powershell-7.6.3-linux-x64.tar.gz").as_deref(),
            Some(h.as_str())
        );
    }

    #[test]
    fn hash_lookup_normalizes_case_and_tolerates_plain_space_form() {
        let upper = "ABCDEF0123456789".repeat(4);
        let manifest = format!("{upper}  file.tar.gz\n");
        assert_eq!(
            expected_sha256(&manifest, "file.tar.gz").as_deref(),
            Some(upper.to_ascii_lowercase().as_str())
        );
    }

    #[test]
    fn hash_lookup_fails_closed_on_missing_or_malformed_entries() {
        // Missing asset -> None (caller hard-fails; verification is never skipped).
        assert_eq!(expected_sha256("", "x.tar.gz"), None);
        // A partial line for HASH_A (wrong length / non-hex) never matches.
        let manifest = format!("{HASH_A} *x.tar.gz\nzz *x.tar.gz\n");
        assert_eq!(expected_sha256(&manifest, "x.tar.gz"), None);
        // Same-prefix asset names do not cross-match.
        let manifest = format!("{} *x.tar.gz.sig\n", "b".repeat(64));
        assert_eq!(expected_sha256(&manifest, "x.tar.gz"), None);
    }

    #[test]
    fn install_root_recognizes_known_layouts() {
        // Versioned subdir (user-local and system).
        assert_eq!(
            install_root_from_path("/home/u/.local/share/powershell/7.6.2/pwsh").as_deref(),
            Some("/home/u/.local/share/powershell")
        );
        assert_eq!(
            install_root_from_path("/opt/microsoft/powershell/7/pwsh").as_deref(),
            Some("/opt/microsoft/powershell")
        );
        // Binary directly inside a `powershell` dir.
        assert_eq!(
            install_root_from_path("/home/u/powershell/pwsh").as_deref(),
            Some("/home/u/powershell")
        );
        // Unrelated locations stay undetermined — never guess.
        assert!(install_root_from_path("/usr/bin/pwsh").is_none());
        assert!(install_root_from_path("/home/u/.dotnet/tools/pwsh").is_none());
        assert!(install_root_from_path("pwsh").is_none());
    }

    #[test]
    fn install_dir_is_the_binary_parent_only_for_portable_layouts() {
        // Versioned layout: the versioned dir's contents get replaced in place,
        // keeping every symlink/PATH pointer valid.
        assert_eq!(
            install_dir_from_binary("/home/u/.local/share/powershell/7.6.2/pwsh").as_deref(),
            Some("/home/u/.local/share/powershell/7.6.2")
        );
        assert_eq!(
            install_dir_from_binary("/opt/microsoft/powershell/7/pwsh").as_deref(),
            Some("/opt/microsoft/powershell/7")
        );
        // Flat layout: the root itself.
        assert_eq!(
            install_dir_from_binary("/home/u/powershell/pwsh").as_deref(),
            Some("/home/u/powershell")
        );
        // Non-portable locations are refused — the update must never replace an
        // arbitrary directory.
        assert!(install_dir_from_binary("/usr/bin/pwsh").is_none());
    }
}
