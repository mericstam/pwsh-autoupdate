//! Semver parsing and the pure update decision (FR-8, ADR-0003).
//!
//! `latest` is always a *stable* release (resolved upstream; prereleases are
//! ignored when computing "latest", per ADR-0003). The decision is then plain
//! semver precedence:
//!
//! * `latest > current` ⇒ [`VersionState::UpdateAvailable`].
//! * otherwise           ⇒ [`VersionState::UpToDate`] (never downgrade).
//!
//! semver precedence already encodes the prerelease rule: a prerelease sorts
//! *below* its release (`7.5.0-preview.1 < 7.5.0`). So a user on
//! `7.5.0-preview.1` against latest stable `7.5.0` yields `UpdateAvailable` —
//! "a newer stable supersedes an installed prerelease of the same release
//! number". A user already on the latest stable, or on a stable newer than
//! latest, yields `UpToDate`.

use crate::core::error::DetectError;
use crate::core::VersionState;
use semver::Version;

/// Parse a PowerShell version string into a [`semver::Version`].
///
/// Accepts an optional leading `v` and trims surrounding whitespace, then
/// defers to `semver`. A parse failure surfaces as
/// [`DetectError::UnparseableVersion`] (never fabricated).
pub fn parse(raw: &str) -> Result<Version, DetectError> {
    let trimmed = raw.trim();
    let candidate = trimmed.strip_prefix('v').unwrap_or(trimmed);
    Version::parse(candidate).map_err(|e| DetectError::UnparseableVersion {
        raw: raw.to_string(),
        reason: e.to_string(),
    })
}

/// Decide whether an update is available, given the installed `current` version
/// and the resolved latest `latest` stable version (FR-8, ADR-0003).
pub fn classify(current: &Version, latest: &Version) -> VersionState {
    if latest > current {
        VersionState::UpdateAvailable
    } else {
        VersionState::UpToDate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parse_plain() {
        assert_eq!(parse("7.4.6").unwrap(), v("7.4.6"));
    }

    #[test]
    fn parse_with_v_prefix_and_whitespace() {
        assert_eq!(parse("  v7.4.6 ").unwrap(), v("7.4.6"));
    }

    #[test]
    fn parse_prerelease() {
        assert_eq!(parse("7.5.0-preview.1").unwrap(), v("7.5.0-preview.1"));
    }

    #[test]
    fn parse_rejects_garbage_without_fabricating() {
        let err = parse("not-a-version").unwrap_err();
        match err {
            DetectError::UnparseableVersion { raw, .. } => assert_eq!(raw, "not-a-version"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn equal_is_up_to_date() {
        assert_eq!(classify(&v("7.4.6"), &v("7.4.6")), VersionState::UpToDate);
    }

    #[test]
    fn latest_greater_is_update_available() {
        assert_eq!(
            classify(&v("7.4.6"), &v("7.5.0")),
            VersionState::UpdateAvailable
        );
    }

    #[test]
    fn current_greater_is_up_to_date_never_downgrade() {
        // Installed is newer than the resolved latest stable: do not downgrade.
        assert_eq!(classify(&v("7.6.0"), &v("7.5.0")), VersionState::UpToDate);
    }

    #[test]
    fn current_prerelease_below_latest_stable_is_update_available() {
        // ADR-0003: a newer stable supersedes an installed prerelease of the
        // same release number. semver: 7.5.0-preview.1 < 7.5.0.
        assert_eq!(
            classify(&v("7.5.0-preview.1"), &v("7.5.0")),
            VersionState::UpdateAvailable
        );
    }

    #[test]
    fn current_prerelease_above_latest_stable_is_up_to_date() {
        // Installed preview is for a release beyond the latest stable: no
        // downgrade to the older stable.
        assert_eq!(
            classify(&v("7.6.0-preview.1"), &v("7.5.0")),
            VersionState::UpToDate
        );
    }
}
