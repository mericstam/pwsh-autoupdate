//! Typed core errors (FR-11).
//!
//! Three failure domains are modelled distinctly:
//!
//! * [`SourceError`] — failures resolving the *latest* version from upstream
//!   (network/HTTP, malformed JSON, missing/unparseable version). The decisive
//!   property (FR-11): a source failure is representable with **no
//!   latest-version value**. The type carries only a reason string; it can
//!   never hold a fabricated version. When resolution fails the orchestration
//!   surfaces this error and never substitutes a guessed "latest".
//! * [`DetectError`] — failures parsing the *installed* version or otherwise
//!   making sense of probed signals.
//! * [`PlanError`] — failures building an update plan (e.g. an unsupported
//!   (method, OS) combination).
//!
//! [`CoreError`] is the top-level sum the host maps to an exit code.

use thiserror::Error;

/// Failure resolving the latest stable version from upstream.
///
/// By construction this type holds **no version value** — a source failure
/// must never be turned into a fabricated "latest version" (FR-11).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SourceError {
    /// The upstream fetch failed (network/HTTP transport).
    #[error("failed to fetch latest version: {0}")]
    Fetch(String),

    /// The upstream response could not be parsed into the expected shape.
    #[error("failed to parse upstream response: {0}")]
    Parse(String),

    /// No usable stable version was present in an otherwise-parseable response.
    #[error("no stable version found in upstream response")]
    NoStableVersion,
}

/// Failure understanding the installed PowerShell from probed signals.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DetectError {
    /// The raw installed-version string could not be parsed as semver.
    #[error("could not parse installed version {raw:?}: {reason}")]
    UnparseableVersion { raw: String, reason: String },

    /// PowerShell is not installed. Since ADR-0006 the host installs from
    /// scratch rather than treating this as terminal; this variant remains the
    /// typed representation of the absent state.
    #[error("PowerShell is not installed")]
    NotInstalled,
}

/// Failure building an update or uninstall plan for the selected method.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    /// The (method, OS) pair has no defined procedure.
    #[error("no plan for method {method} on {os}")]
    UnsupportedCombination { method: String, os: String },
}

/// Failure performing the portable tar.gz download-and-replace (A-6).
///
/// Every variant is fail-closed: no variant can carry a "partially verified"
/// or "assumed good" state — the adapter either completes the full
/// download → SHA-256 verify → safe-extract → atomic-swap → re-verify chain or
/// surfaces one of these (and rolls back where a swap already happened).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PortableError {
    /// The installed pwsh binary could not be resolved to an on-disk path.
    #[error("could not resolve the installed pwsh binary to an on-disk path")]
    BinaryNotFound,

    /// The resolved binary does not live in a recognized portable layout; the
    /// tool must never replace an arbitrary directory.
    #[error("the installed PowerShell at {binary} does not look like a portable tar.gz install; refusing to replace it")]
    NotPortableLayout { binary: String },

    /// PowerShell ships no portable Linux build for this CPU.
    #[error("no portable PowerShell Linux build exists for CPU architecture {arch:?}")]
    UnsupportedArch { arch: String },

    /// A download failed (network/HTTP). Carries no version/hash value.
    #[error("failed to download {what}: {reason}")]
    Download { what: String, reason: String },

    /// The release hash manifest has no usable entry for the asset —
    /// verification is required, never skipped.
    #[error("no SHA-256 entry for {asset} in the release hash manifest; refusing to install unverified bytes")]
    MissingHash { asset: String },

    /// The downloaded archive does not match the release manifest.
    #[error(
        "SHA-256 mismatch for {asset}: expected {expected}, got {actual}; refusing to install"
    )]
    HashMismatch {
        asset: String,
        expected: String,
        actual: String,
    },

    /// The archive tried something an installer payload never legitimately
    /// does (path escape, disallowed entry type, oversized expansion).
    #[error("archive rejected: {reason}")]
    UnsafeArchive { reason: String },

    /// The install directory is not writable by this process. FR-12: surface
    /// the requirement; never self-elevate.
    #[error("modifying the portable install at {dir} requires write access ({reason}). Re-run with elevation (e.g. sudo); this tool will not self-elevate")]
    PermissionDenied { dir: String, reason: String },

    /// A filesystem step failed for a non-permission reason.
    #[error("{context}: {reason}")]
    Io { context: String, reason: String },

    /// The swapped-in install did not report the target version; the previous
    /// install was restored (never report success on a real failure, FR-6).
    #[error("the replaced install did not verify at {expected} (reported {actual:?}); the previous install was restored")]
    VerifyFailed {
        expected: String,
        actual: Option<String>,
    },
}

/// Top-level core error, mapped to a process exit code by the host.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error(transparent)]
    Source(#[from] SourceError),

    #[error(transparent)]
    Detect(#[from] DetectError),

    #[error(transparent)]
    Plan(#[from] PlanError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_fetch_displays_reason_and_holds_no_version() {
        let err = SourceError::Fetch("connection refused".to_string());
        assert_eq!(
            err.to_string(),
            "failed to fetch latest version: connection refused"
        );
        // The type has no field that could carry a version — enforced by the
        // type system; this test documents the FR-11 invariant.
    }

    #[test]
    fn source_parse_displays() {
        let err = SourceError::Parse("unexpected token".to_string());
        assert_eq!(
            err.to_string(),
            "failed to parse upstream response: unexpected token"
        );
    }

    #[test]
    fn source_no_stable_version_displays() {
        assert_eq!(
            SourceError::NoStableVersion.to_string(),
            "no stable version found in upstream response"
        );
    }

    #[test]
    fn detect_unparseable_version_displays() {
        let err = DetectError::UnparseableVersion {
            raw: "not-a-version".to_string(),
            reason: "expected MAJOR.MINOR.PATCH".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "could not parse installed version \"not-a-version\": expected MAJOR.MINOR.PATCH"
        );
    }

    #[test]
    fn detect_not_installed_displays() {
        assert_eq!(
            DetectError::NotInstalled.to_string(),
            "PowerShell is not installed"
        );
    }

    #[test]
    fn plan_unsupported_combination_displays() {
        let err = PlanError::UnsupportedCombination {
            method: "Homebrew".to_string(),
            os: "Windows".to_string(),
        };
        assert_eq!(err.to_string(), "no plan for method Homebrew on Windows");
    }

    #[test]
    fn core_error_wraps_each_domain() {
        let s: CoreError = SourceError::NoStableVersion.into();
        assert_eq!(
            s.to_string(),
            "no stable version found in upstream response"
        );
        let d: CoreError = DetectError::NotInstalled.into();
        assert_eq!(d.to_string(), "PowerShell is not installed");
        let p: CoreError = PlanError::UnsupportedCombination {
            method: "Snap".to_string(),
            os: "Macos".to_string(),
        }
        .into();
        assert_eq!(p.to_string(), "no plan for method Snap on Macos");
    }
}
