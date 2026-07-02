//! Portable tar.gz download-and-replace (A-6, FR-6/12).
//!
//! The IO driver for the portable Linux channel: download the release tarball,
//! verify its SHA-256 against the release hash manifest, extract it into a
//! staging directory with hardened rules, atomically swap it into place, and
//! verify the swapped-in binary actually reports the target version before the
//! backup of the previous install is discarded. Any failure after the swap
//! rolls the previous install back — the update is transactional.
//!
//! The pure rules (URL construction, manifest decoding/lookup, layout
//! recognition) live in `core::portable`; this module owns only the IO.
//!
//! Hardening applied to the archive (a malicious tarball attacks the
//! *extractor*):
//! * entries are unpacked with `unpack_in`, which refuses paths escaping the
//!   staging directory (tar-slip);
//! * only regular files, directories, and relative non-`..` symlinks are
//!   accepted — device nodes, FIFOs, hardlinks etc. are rejected;
//! * total unpacked size is capped (decompression bomb);
//! * setuid/setgid bits are stripped after extraction.

use crate::adapters::http::HttpClient;
use crate::adapters::probe;
use crate::adapters::runner::CommandRunner;
use crate::core::error::PortableError;
use crate::core::{portable as rules, version, VersionState};
use semver::Version;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Cap for the `hashes.sha256` manifest download (the real file is ~8 KiB).
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Cap for the tarball download (the real asset is ~75 MiB).
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
/// Cap for the total unpacked size (the real payload is ~280 MiB unpacked).
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Download, verify, extract, and atomically swap the portable install to
/// `target`. On success the previously-installed tree is gone and the resolved
/// `pwsh` binary (same path as before) has been re-run and confirmed to report
/// `target`. On any error the previous install is left (or restored) intact.
///
/// Progress lines go to `out`; errors are returned, not printed (the host owns
/// rendering and exit codes).
pub fn replace_portable(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    target: &Version,
    out: &mut dyn Write,
) -> Result<(), PortableError> {
    // Locate the install. The layout gate means we only ever replace a
    // directory that detection also classifies as portable — never guess.
    let binary = runner
        .resolve_program_path(probe::pwsh_exe())
        .ok_or(PortableError::BinaryNotFound)?;
    let install_dir = rules::install_dir_from_binary(&binary).ok_or_else(|| {
        PortableError::NotPortableLayout {
            binary: binary.clone(),
        }
    })?;

    // Host CPU -> asset label. Probed here (adapter layer) and passed to the
    // pure rules as data.
    let arch = rules::arch_label(std::env::consts::ARCH).ok_or_else(|| {
        PortableError::UnsupportedArch {
            arch: std::env::consts::ARCH.to_string(),
        }
    })?;
    let asset = rules::asset_name(target, arch);
    let tag = rules::release_tag(target);

    // Expected hash first: if the manifest has no entry we refuse before
    // downloading a payload we could never verify.
    let manifest_bytes = http
        .get_bytes(&rules::hashes_url(&tag), MAX_MANIFEST_BYTES)
        .map_err(|e| PortableError::Download {
            what: "release hash manifest (hashes.sha256)".to_string(),
            reason: e.to_string(),
        })?;
    let manifest = rules::decode_hash_manifest(&manifest_bytes);
    let expected =
        rules::expected_sha256(&manifest, &asset).ok_or_else(|| PortableError::MissingHash {
            asset: asset.clone(),
        })?;

    let url = rules::asset_url(&tag, &asset);
    let _ = writeln!(out, "Downloading {url} ...");
    let archive = http
        .get_bytes(&url, MAX_ARCHIVE_BYTES)
        .map_err(|e| PortableError::Download {
            what: asset.clone(),
            reason: e.to_string(),
        })?;

    let actual = hex(&Sha256::digest(&archive));
    if actual != expected {
        return Err(PortableError::HashMismatch {
            asset,
            expected,
            actual,
        });
    }
    let _ = writeln!(out, "SHA-256 verified against the release manifest.");

    // Stage next to the install dir (same filesystem, so the swap renames are
    // atomic). Creating the staging dir doubles as the up-front writability
    // check: a permission failure surfaces the FR-12 elevation message before
    // anything is touched.
    let install = PathBuf::from(&install_dir);
    let staging = sibling(&install, ".pwsh-autoupdate.new");
    let backup = sibling(&install, ".pwsh-autoupdate.old");
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::create_dir_all(&staging)
        .map_err(|e| fs_err(&install_dir, "creating the staging directory", &e))?;

    let extracted = extract_hardened(&archive, &staging);
    if let Err(e) = extracted {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    // Sanity: the payload must actually contain the pwsh binary, executable.
    // Hardcoded "pwsh" (not `pwsh_exe()`): this channel replaces a *Linux*
    // portable payload, whose binary is always `pwsh`, regardless of the OS
    // the code happens to be compiled for.
    let staged_pwsh = staging.join("pwsh");
    if !staged_pwsh.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(PortableError::UnsafeArchive {
            reason: "archive does not contain a pwsh binary at its root".to_string(),
        });
    }
    ensure_executable(&staged_pwsh);

    // Atomic swap with rollback: install -> backup, staging -> install.
    let _ = writeln!(out, "Replacing {install_dir} ...");
    if let Err(e) = std::fs::rename(&install, &backup) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(fs_err(&install_dir, "moving the current install aside", &e));
    }
    if let Err(e) = std::fs::rename(&staging, &install) {
        // Roll the previous install back before surfacing the error.
        let _ = std::fs::rename(&backup, &install);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(fs_err(
            &install_dir,
            "moving the new install into place",
            &e,
        ));
    }

    // Verify the swapped-in binary BEFORE discarding the backup: run it (same
    // resolved path as before the swap) and require the target version. A
    // binary that doesn't run or reports the wrong version (e.g. a musl host
    // given the glibc build) rolls back — never report success on a real
    // failure (FR-6).
    let reported = runner
        .run(&binary, &["--version"])
        .ok()
        .filter(|o| o.status == 0)
        .and_then(|o| probe::extract_version_token(&o.stdout));
    let verified = reported
        .as_deref()
        .and_then(|raw| version::parse(raw).ok())
        .map(|now| version::classify(&now, target) == VersionState::UpToDate)
        .unwrap_or(false);
    if !verified {
        let _ = std::fs::rename(&install, &staging);
        let _ = std::fs::rename(&backup, &install);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(PortableError::VerifyFailed {
            expected: target.to_string(),
            actual: reported,
        });
    }

    let _ = std::fs::remove_dir_all(&backup);
    Ok(())
}

/// A sibling path of `dir` (same parent, so renames stay on one filesystem).
fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "powershell".to_string());
    name.push_str(suffix);
    dir.parent()
        .map(|p| p.join(&name))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Map a filesystem error: permission problems become the FR-12 elevation
/// message; everything else stays an honest IO error with context.
fn fs_err(dir: &str, context: &str, e: &std::io::Error) -> PortableError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        PortableError::PermissionDenied {
            dir: dir.to_string(),
            reason: context.to_string(),
        }
    } else {
        PortableError::Io {
            context: context.to_string(),
            reason: e.to_string(),
        }
    }
}

/// Extract the gzipped tarball into `dest` under the hardening rules in the
/// module docs. `dest` must exist and be empty.
fn extract_hardened(archive: &[u8], dest: &Path) -> Result<(), PortableError> {
    use tar::EntryType;

    let gz = flate2::read::GzDecoder::new(archive);
    let mut ar = tar::Archive::new(gz);
    // NOTE: `preserve_permissions` stays at its default `false` — in that mode
    // the tar crate masks every entry mode with 0o777, which strips
    // setuid/setgid at the source while still applying the rwx (incl. execute)
    // bits. The strip_setuid_bits() walk below is defense-in-depth on top.

    let entries = ar.entries().map_err(|e| PortableError::Io {
        context: "reading the archive".to_string(),
        reason: e.to_string(),
    })?;

    let mut total: u64 = 0;
    for entry in entries {
        let mut entry = entry.map_err(|e| PortableError::Io {
            context: "reading an archive entry".to_string(),
            reason: e.to_string(),
        })?;
        let kind = entry.header().entry_type();
        match kind {
            EntryType::Regular | EntryType::Directory => {}
            EntryType::Symlink => {
                // Only relative, non-escaping targets; a portable payload has
                // no business linking outside its own tree.
                let target = entry
                    .link_name()
                    .ok()
                    .flatten()
                    .map(|t| t.into_owned())
                    .unwrap_or_default();
                let escapes = target.is_absolute()
                    || target
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir));
                if escapes {
                    return Err(PortableError::UnsafeArchive {
                        reason: format!("symlink entry with escaping target {}", target.display()),
                    });
                }
            }
            // Metadata pseudo-entries (pax headers, GNU long names) are handled
            // by the tar reader itself; skip them if surfaced.
            EntryType::XHeader
            | EntryType::XGlobalHeader
            | EntryType::GNULongName
            | EntryType::GNULongLink => continue,
            other => {
                return Err(PortableError::UnsafeArchive {
                    reason: format!("disallowed entry type {other:?}"),
                });
            }
        }

        total = total.saturating_add(entry.header().size().unwrap_or(0));
        if total > MAX_UNPACKED_BYTES {
            return Err(PortableError::UnsafeArchive {
                reason: "archive expands beyond the unpacked-size limit".to_string(),
            });
        }

        // `unpack_in` performs the tar-slip checks: it refuses `..`/absolute
        // paths (Ok(false)) and refuses writing through symlinked ancestors.
        let unpacked = entry.unpack_in(dest).map_err(|e| PortableError::Io {
            context: "extracting an archive entry".to_string(),
            reason: e.to_string(),
        })?;
        if !unpacked {
            return Err(PortableError::UnsafeArchive {
                reason: "entry path escapes the extraction directory".to_string(),
            });
        }
    }

    strip_setuid_bits(dest).map_err(|e| PortableError::Io {
        context: "normalizing extracted permissions".to_string(),
        reason: e.to_string(),
    })
}

/// Recursively clear setuid/setgid bits on everything under `dir` — an
/// installer payload never legitimately needs them.
#[cfg(unix)]
fn strip_setuid_bits(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?; // does not follow symlinks
        if meta.file_type().is_symlink() {
            continue;
        }
        let mode = meta.permissions().mode();
        if mode & 0o6000 != 0 {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(mode & 0o1777))?;
        }
        if meta.is_dir() {
            strip_setuid_bits(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn strip_setuid_bits(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Make sure the staged pwsh binary carries execute permission (the archive
/// should already provide it; this is belt-and-braces, best-effort).
#[cfg(unix)]
fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755));
        }
    }
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) {}

/// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Test fixture builders shared with the host-orchestration tests (`app.rs`):
/// realistic tarball payloads and the UTF-16LE hash manifest matching the live
/// upstream encoding.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// Build a gzipped tarball from (path, contents, mode) file entries.
    pub(crate) fn targz(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, contents, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// A minimal plausible portable payload: pwsh + a support file.
    pub(crate) fn payload_targz() -> Vec<u8> {
        targz(&[
            ("pwsh", b"#!/bin/sh\necho new\n" as &[u8], 0o755),
            ("libpsl-native.so", b"\x7fELF-fake" as &[u8], 0o644),
        ])
    }

    pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    /// UTF-16LE + BOM + CRLF manifest, the live upstream encoding.
    pub(crate) fn manifest_utf16le(entries: &[(&str, &str)]) -> Vec<u8> {
        let text: String = entries
            .iter()
            .map(|(hash, name)| format!("{hash} *{name}\r\n"))
            .collect();
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{manifest_utf16le, payload_targz, sha256_hex, targz};
    use super::*;
    use crate::adapters::runner::CmdOutput;
    use crate::core::error::SourceError;
    use std::cell::RefCell;
    use std::collections::HashMap;

    // --- Fakes ---------------------------------------------------------------

    #[derive(Default)]
    struct FakeHttp {
        bytes: HashMap<String, Vec<u8>>,
    }
    impl FakeHttp {
        fn with(mut self, url: &str, body: Vec<u8>) -> Self {
            self.bytes.insert(url.to_string(), body);
            self
        }
    }
    impl HttpClient for FakeHttp {
        fn get_json(&self, url: &str) -> Result<serde_json::Value, SourceError> {
            Err(SourceError::Fetch(format!("no fake json for {url}")))
        }
        fn get_text(&self, url: &str) -> Result<String, SourceError> {
            Err(SourceError::Fetch(format!("no fake text for {url}")))
        }
        fn get_bytes(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, SourceError> {
            let body = self
                .bytes
                .get(url)
                .cloned()
                .ok_or_else(|| SourceError::Fetch(format!("no fake bytes for {url}")))?;
            if body.len() as u64 > max_bytes {
                return Err(SourceError::Fetch("body over limit".to_string()));
            }
            Ok(body)
        }
    }

    /// Runner fake: resolves pwsh to a caller-chosen path; `pwsh --version`
    /// (by that absolute path) reports a canned line so the post-swap verify
    /// step can be driven to succeed or fail.
    #[derive(Default)]
    struct FakeRunner {
        resolved: Option<String>,
        version_line: Option<String>,
        runs: RefCell<Vec<(String, Vec<String>)>>,
    }
    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CmdOutput> {
            self.runs.borrow_mut().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            match (&self.version_line, args) {
                (Some(line), ["--version"]) => Ok(CmdOutput {
                    status: 0,
                    stdout: line.clone(),
                    stderr: String::new(),
                }),
                _ => Ok(CmdOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "no canned output".to_string(),
                }),
            }
        }
        fn exists(&self, _program: &str) -> bool {
            true
        }
        fn resolve_program_path(&self, _program: &str) -> Option<String> {
            self.resolved.clone()
        }
    }

    /// A portable install tree `<root>/powershell/7.6.2/pwsh` in a tempdir.
    /// Returns (tempdir guard, install dir, binary path).
    fn portable_install(old_contents: &str) -> (tempfile::TempDir, PathBuf, String) {
        let td = tempfile::tempdir().unwrap();
        let install = td.path().join("powershell").join("7.6.2");
        std::fs::create_dir_all(&install).unwrap();
        let binary = install.join("pwsh");
        std::fs::write(&binary, old_contents).unwrap();
        let binary = binary.to_string_lossy().into_owned();
        (td, install, binary)
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    // Asset selection follows the REAL host arch (the one probe passes to the
    // pure rules), so the fixture asset/URLs are derived the same way — the
    // tests run unchanged on x86_64 and aarch64 CI runners alike.
    fn asset() -> String {
        rules::asset_name(
            &v("7.6.3"),
            rules::arch_label(std::env::consts::ARCH).expect("test host arch has a portable build"),
        )
    }
    fn tarball_url() -> String {
        rules::asset_url("v7.6.3", &asset())
    }
    fn hashes_url() -> String {
        rules::hashes_url("v7.6.3")
    }

    fn http_serving(tarball: &[u8]) -> FakeHttp {
        FakeHttp::default()
            .with(
                &hashes_url(),
                manifest_utf16le(&[
                    (&"a".repeat(64), "some-other-asset.zip"),
                    (&sha256_hex(tarball), &asset()),
                ]),
            )
            .with(&tarball_url(), tarball.to_vec())
    }

    #[test]
    fn happy_path_downloads_verifies_swaps_and_keeps_the_binary_path() {
        let tarball = payload_targz();
        let http = http_serving(&tarball);
        let (_td, install, binary) = portable_install("old-binary");
        let runner = FakeRunner {
            resolved: Some(binary.clone()),
            version_line: Some("PowerShell 7.6.3".to_string()),
            ..Default::default()
        };
        let mut out = Vec::new();

        replace_portable(&http, &runner, &v("7.6.3"), &mut out).unwrap();

        // Same path, new contents — every PATH entry / symlink stays valid.
        let now = std::fs::read_to_string(&binary).unwrap();
        assert!(now.contains("echo new"));
        assert!(install.join("libpsl-native.so").is_file());
        // The swapped-in binary was actually run to verify the version.
        assert!(runner
            .runs
            .borrow()
            .iter()
            .any(|(p, a)| p == &binary && a == &vec!["--version".to_string()]));
        // No staging/backup residue.
        let parent = install.parent().unwrap();
        let residue: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".pwsh-autoupdate."))
            .collect();
        assert!(residue.is_empty(), "leftover work dirs: {residue:?}");
        let stdout = String::from_utf8(out).unwrap();
        assert!(stdout.contains("SHA-256 verified"));
    }

    #[test]
    fn hash_mismatch_refuses_and_leaves_install_untouched() {
        let tarball = payload_targz();
        // Manifest advertises a DIFFERENT hash for the asset.
        let http = FakeHttp::default()
            .with(
                &hashes_url(),
                manifest_utf16le(&[(&"c".repeat(64), &asset())]),
            )
            .with(&tarball_url(), tarball);
        let (_td, _install, binary) = portable_install("old-binary");
        let runner = FakeRunner {
            resolved: Some(binary.clone()),
            version_line: Some("PowerShell 7.6.3".to_string()),
            ..Default::default()
        };
        let mut out = Vec::new();

        let err = replace_portable(&http, &runner, &v("7.6.3"), &mut out).unwrap_err();
        assert!(matches!(err, PortableError::HashMismatch { .. }));
        // Nothing replaced, nothing run.
        assert_eq!(std::fs::read_to_string(&binary).unwrap(), "old-binary");
        assert!(runner.runs.borrow().is_empty());
    }

    #[test]
    fn missing_manifest_entry_refuses_before_downloading_the_asset() {
        let http = FakeHttp::default().with(
            &hashes_url(),
            manifest_utf16le(&[(&"a".repeat(64), "unrelated.zip")]),
        );
        // Note: no tarball URL registered — reaching it would error differently.
        let (_td, _install, binary) = portable_install("old-binary");
        let runner = FakeRunner {
            resolved: Some(binary.clone()),
            ..Default::default()
        };
        let mut out = Vec::new();
        let err = replace_portable(&http, &runner, &v("7.6.3"), &mut out).unwrap_err();
        assert!(matches!(err, PortableError::MissingHash { .. }));
        assert_eq!(std::fs::read_to_string(&binary).unwrap(), "old-binary");
    }

    #[test]
    fn archive_without_pwsh_at_root_is_rejected_and_rolled_back() {
        let tarball = targz(&[("readme.txt", b"not a shell" as &[u8], 0o644)]);
        let http = http_serving(&tarball);
        let (_td, _install, binary) = portable_install("old-binary");
        let runner = FakeRunner {
            resolved: Some(binary.clone()),
            version_line: Some("PowerShell 7.6.3".to_string()),
            ..Default::default()
        };
        let mut out = Vec::new();
        let err = replace_portable(&http, &runner, &v("7.6.3"), &mut out).unwrap_err();
        assert!(matches!(err, PortableError::UnsafeArchive { .. }));
        assert_eq!(std::fs::read_to_string(&binary).unwrap(), "old-binary");
    }

    #[test]
    fn verify_failure_rolls_the_previous_install_back() {
        // The swap succeeds but the new binary reports the OLD version (e.g. a
        // payload that does not run on this host) -> rollback + VerifyFailed.
        let tarball = payload_targz();
        let http = http_serving(&tarball);
        let (_td, install, binary) = portable_install("old-binary");
        let runner = FakeRunner {
            resolved: Some(binary.clone()),
            version_line: Some("PowerShell 7.6.2".to_string()), // wrong version
            ..Default::default()
        };
        let mut out = Vec::new();
        let err = replace_portable(&http, &runner, &v("7.6.3"), &mut out).unwrap_err();
        assert!(matches!(err, PortableError::VerifyFailed { .. }));
        // The previous install is back in place, bit for bit.
        assert_eq!(std::fs::read_to_string(&binary).unwrap(), "old-binary");
        assert!(install.is_dir());
    }

    #[test]
    fn non_portable_layout_is_refused() {
        // Binary resolves to a location outside any recognized portable tree.
        let td = tempfile::tempdir().unwrap();
        let bin = td.path().join("usr-bin").join("pwsh");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "x").unwrap();
        let runner = FakeRunner {
            resolved: Some(bin.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let http = FakeHttp::default();
        let mut out = Vec::new();
        let err = replace_portable(&http, &runner, &v("7.6.3"), &mut out).unwrap_err();
        assert!(matches!(err, PortableError::NotPortableLayout { .. }));
    }

    #[test]
    fn unresolvable_binary_is_an_honest_error() {
        let runner = FakeRunner::default(); // resolves to None
        let http = FakeHttp::default();
        let mut out = Vec::new();
        let err = replace_portable(&http, &runner, &v("7.6.3"), &mut out).unwrap_err();
        assert_eq!(err, PortableError::BinaryNotFound);
    }

    // --- extract_hardened ----------------------------------------------------

    #[test]
    fn extraction_rejects_path_traversal_entries() {
        // Craft a raw tar with an entry whose path escapes the destination.
        // `Builder::append_data` refuses `..` paths, so write the header path
        // bytes directly.
        let mut header = tar::Header::new_gnu();
        let contents = b"owned";
        {
            // GNU header name field is bytes[0..100].
            let name = b"../escape.txt";
            header.as_mut_bytes()[..name.len()].copy_from_slice(name);
        }
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        let mut raw = Vec::new();
        raw.extend_from_slice(header.as_bytes());
        raw.extend_from_slice(contents);
        raw.resize(raw.len() + (512 - contents.len() % 512), 0); // pad block
        raw.extend_from_slice(&[0u8; 1024]); // end-of-archive
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        let tarball = gz.finish().unwrap();

        let td = tempfile::tempdir().unwrap();
        let dest = td.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_hardened(&tarball, &dest).unwrap_err();
        assert!(matches!(err, PortableError::UnsafeArchive { .. }));
        // The escape target must not exist.
        assert!(!td.path().join("escape.txt").exists());
    }

    #[test]
    fn extraction_rejects_escaping_symlink_targets() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder
            .append_link(&mut header, "evil-link", "../../etc/passwd")
            .unwrap();
        let tarball = builder.into_inner().unwrap().finish().unwrap();

        let td = tempfile::tempdir().unwrap();
        let dest = td.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_hardened(&tarball, &dest).unwrap_err();
        assert!(matches!(err, PortableError::UnsafeArchive { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn extraction_strips_setuid_bits() {
        use std::os::unix::fs::PermissionsExt;
        let tarball = targz(&[("suid-tool", b"x" as &[u8], 0o4755)]);
        let td = tempfile::tempdir().unwrap();
        let dest = td.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        extract_hardened(&tarball, &dest).unwrap();
        let mode = std::fs::metadata(dest.join("suid-tool"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o6000, 0, "setuid/setgid must be stripped");
        assert_ne!(mode & 0o111, 0, "execute bits are preserved");
    }

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
