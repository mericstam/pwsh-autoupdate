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

    /// PowerShell is not installed (ADR-0001: report and exit, do not install).
    #[error("PowerShell is not installed")]
    NotInstalled,
}

/// Failure building an update plan for the selected method.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    /// The (method, OS) pair has no defined upgrade procedure.
    #[error("no update plan for method {method} on {os}")]
    UnsupportedCombination { method: String, os: String },
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
        assert_eq!(
            err.to_string(),
            "no update plan for method Homebrew on Windows"
        );
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
        assert_eq!(p.to_string(), "no update plan for method Snap on Macos");
    }
}
