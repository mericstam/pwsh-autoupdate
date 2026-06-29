//! Adapters — the thin layer that touches the outside world behind traits.
//!
//! The HTTP client and the process runner sit behind traits so tests inject
//! fakes; production wires the real impls. The OS probe is `cfg`-gated and
//! emits plain `DetectionSignals` consumed by the pure core.
//!
//! This module also hosts the `version-resolve` orchestration
//! ([`resolve_latest_stable`]) — a thin, IO-driving function that lives in the
//! adapter layer (NOT in core): it uses an [`HttpClient`] to fetch the two
//! upstream sources, cross-checks them, and parses the resolved stable tag into
//! a [`semver::Version`] via the pure `core::version` parser. On any
//! network/HTTP/parse failure it surfaces a [`SourceError`] (wrapped in
//! [`CoreError`]) and produces NO version value (FR-11).

pub mod http;
pub mod probe;
pub mod runner;

use crate::adapters::http::{
    BuildInfo, GitHubRelease, HttpClient, BUILD_INFO_STABLE_URL, GITHUB_RELEASES_LATEST_URL,
};
use crate::core::error::{CoreError, SourceError};
use crate::core::{version, ReleaseInfo};

/// Resolve the latest STABLE PowerShell release (FR-2, ADR-0003).
///
/// Strategy:
/// 1. Fetch the stable build-info feed; its `ReleaseTag` is the authoritative
///    stable anchor (ADR-0003).
/// 2. Fetch the GitHub Releases "latest" payload and cross-check: a prerelease
///    "latest" is ignored for the stable channel; a *newer* confirmed-stable
///    GitHub tag may supersede the anchor, but never overrides it downward.
/// 3. Parse the resolved stable tag to a [`semver::Version`] via the pure
///    `core::version` parser.
///
/// On network/HTTP/parse failure — or an empty/unparseable tag — this returns a
/// [`CoreError::Source`] and yields NO version value (FR-11): the type system
/// makes a fabricated "latest" unrepresentable on the error path.
pub fn resolve_latest_stable(http: &dyn HttpClient) -> Result<ReleaseInfo, CoreError> {
    // --- 1. build-info stable anchor ---------------------------------------
    let build_info_body = http.get_text(BUILD_INFO_STABLE_URL)?;
    let build_info = BuildInfo::from_text(&build_info_body)?;
    let anchor_tag = build_info.release_tag.trim().to_string();
    if anchor_tag.is_empty() {
        return Err(SourceError::NoStableVersion.into());
    }

    // --- 2. GitHub Releases cross-check ------------------------------------
    // Fetch + parse the GitHub "latest" payload so a transport/parse failure
    // there is surfaced honestly (FR-11) rather than silently skipped, and to
    // cross-check the anchor. Per ADR-0003 the build-info `ReleaseTag` is the
    // canonical stable floor: a prerelease GitHub "latest" is ignored
    // (stable-only) and never downgrades below the anchor.
    let release_value = http.get_json(GITHUB_RELEASES_LATEST_URL)?;
    let release: GitHubRelease =
        serde_json::from_value(release_value).map_err(|e| SourceError::Parse(e.to_string()))?;

    // Resolve the stable tag. Default to the canonical build-info anchor. If the
    // GitHub "latest" is itself stable and parses to a strictly greater semver
    // than the anchor, take the (newer, confirmed-stable) GitHub tag; a
    // prerelease or older/equal GitHub tag never overrides the stable feed.
    let mut resolved_tag = anchor_tag.clone();
    let mut resolved_version =
        version::parse(&anchor_tag).map_err(|e| SourceError::Parse(e.to_string()))?;
    if !release.prerelease && !release.tag_name.trim().is_empty() {
        if let Ok(gh_version) = version::parse(release.tag_name.trim()) {
            if gh_version > resolved_version {
                resolved_version = gh_version;
                resolved_tag = release.tag_name.trim().to_string();
            }
        }
    }

    Ok(ReleaseInfo {
        version: resolved_version,
        tag: resolved_tag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory fake: maps URL -> recorded body, or a forced fetch error.
    #[derive(Default)]
    struct FakeHttp {
        bodies: HashMap<String, String>,
        fail_url: Option<String>,
    }

    impl FakeHttp {
        fn with(url: &str, body: &str) -> Self {
            let mut bodies = HashMap::new();
            bodies.insert(url.to_string(), body.to_string());
            Self {
                bodies,
                fail_url: None,
            }
        }
        fn and(mut self, url: &str, body: &str) -> Self {
            self.bodies.insert(url.to_string(), body.to_string());
            self
        }
        fn failing(url: &str) -> Self {
            Self {
                bodies: HashMap::new(),
                fail_url: Some(url.to_string()),
            }
        }
    }

    impl HttpClient for FakeHttp {
        fn get_json(&self, url: &str) -> Result<serde_json::Value, SourceError> {
            let body = self.get_text(url)?;
            serde_json::from_str(&body).map_err(|e| SourceError::Parse(e.to_string()))
        }
        fn get_text(&self, url: &str) -> Result<String, SourceError> {
            if self.fail_url.as_deref() == Some(url) {
                return Err(SourceError::Fetch("connection refused".into()));
            }
            self.bodies
                .get(url)
                .cloned()
                .ok_or_else(|| SourceError::Fetch(format!("no fake for {url}")))
        }
    }

    fn release_json(tag: &str, prerelease: bool) -> String {
        format!(r#"{{ "tag_name": "{tag}", "prerelease": {prerelease}, "assets": [] }}"#)
    }
    fn build_info_json(tag: &str) -> String {
        format!(r#"{{ "ReleaseTag": "{tag}", "ReleaseDate": "2024-01-01" }}"#)
    }

    #[test]
    fn resolves_agreeing_stable_tag_to_semver() {
        let http = FakeHttp::with(BUILD_INFO_STABLE_URL, &build_info_json("v7.4.6"))
            .and(GITHUB_RELEASES_LATEST_URL, &release_json("v7.4.6", false));
        let info = resolve_latest_stable(&http).unwrap();
        assert_eq!(info.version, semver::Version::parse("7.4.6").unwrap());
        assert_eq!(info.tag, "v7.4.6");
    }

    #[test]
    fn ignores_github_prerelease_and_uses_stable_anchor() {
        // GitHub "latest" is a preview; build-info stable anchor wins (ADR-0003).
        let http = FakeHttp::with(BUILD_INFO_STABLE_URL, &build_info_json("v7.4.6")).and(
            GITHUB_RELEASES_LATEST_URL,
            &release_json("v7.5.0-preview.1", true),
        );
        let info = resolve_latest_stable(&http).unwrap();
        assert_eq!(info.version, semver::Version::parse("7.4.6").unwrap());
    }

    #[test]
    fn keeps_anchor_when_github_stable_is_older() {
        // GitHub stable (v7.4.5) is below the anchor (v7.4.6): never downgrade.
        let http = FakeHttp::with(BUILD_INFO_STABLE_URL, &build_info_json("v7.4.6"))
            .and(GITHUB_RELEASES_LATEST_URL, &release_json("v7.4.5", false));
        let info = resolve_latest_stable(&http).unwrap();
        assert_eq!(info.tag, "v7.4.6");
    }

    #[test]
    fn newer_github_stable_supersedes_anchor() {
        // GitHub stable (v7.5.0) exceeds the anchor (v7.4.6): take the newer.
        let http = FakeHttp::with(BUILD_INFO_STABLE_URL, &build_info_json("v7.4.6"))
            .and(GITHUB_RELEASES_LATEST_URL, &release_json("v7.5.0", false));
        let info = resolve_latest_stable(&http).unwrap();
        assert_eq!(info.version, semver::Version::parse("7.5.0").unwrap());
        assert_eq!(info.tag, "v7.5.0");
    }

    #[test]
    fn fetch_failure_on_build_info_surfaces_source_error_no_version() {
        let http = FakeHttp::failing(BUILD_INFO_STABLE_URL);
        let err = resolve_latest_stable(&http).unwrap_err();
        match err {
            CoreError::Source(SourceError::Fetch(_)) => {}
            other => panic!("expected source fetch error, got {other:?}"),
        }
    }

    #[test]
    fn fetch_failure_on_github_surfaces_source_error_no_version() {
        // build-info succeeds, GitHub fetch fails -> still an honest failure.
        let mut http = FakeHttp::with(BUILD_INFO_STABLE_URL, &build_info_json("v7.4.6"));
        http.fail_url = Some(GITHUB_RELEASES_LATEST_URL.to_string());
        let err = resolve_latest_stable(&http).unwrap_err();
        assert!(matches!(err, CoreError::Source(SourceError::Fetch(_))));
    }

    #[test]
    fn malformed_build_info_is_a_parse_source_error_no_version() {
        let http = FakeHttp::with(BUILD_INFO_STABLE_URL, "not json");
        let err = resolve_latest_stable(&http).unwrap_err();
        assert!(matches!(err, CoreError::Source(SourceError::Parse(_))));
    }

    #[test]
    fn empty_build_info_tag_is_no_stable_version() {
        let http = FakeHttp::with(BUILD_INFO_STABLE_URL, &build_info_json(""));
        let err = resolve_latest_stable(&http).unwrap_err();
        assert!(matches!(
            err,
            CoreError::Source(SourceError::NoStableVersion)
        ));
    }

    #[test]
    fn unparseable_tag_surfaces_parse_error_not_a_fabricated_version() {
        let http = FakeHttp::with(BUILD_INFO_STABLE_URL, &build_info_json("not-a-version")).and(
            GITHUB_RELEASES_LATEST_URL,
            &release_json("not-a-version", false),
        );
        let err = resolve_latest_stable(&http).unwrap_err();
        assert!(matches!(err, CoreError::Source(SourceError::Parse(_))));
    }
}
