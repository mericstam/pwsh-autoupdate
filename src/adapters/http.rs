//! HTTP client seam (FR-2).
//!
//! The real implementation (`RealHttp`) is the only place in the crate that
//! touches `ureq` types. Network/HTTP/parse failures map to
//! [`SourceError`](crate::core::error::SourceError) — never to a fabricated
//! version (FR-11). Tests inject a fake behind the [`HttpClient`] trait.
//!
//! Two upstream sources are consumed (ADR-0003): the GitHub Releases "latest"
//! API ([`GITHUB_RELEASES_LATEST_URL`]) and the stable build-info feed
//! ([`BUILD_INFO_STABLE_URL`]). The serde structs below are tolerant of
//! extra/missing fields (`#[serde(default)]`, `Option`) because external JSON
//! evolves — defensive parsing of the real upstream shape, not a stub.

use crate::core::error::SourceError;
use serde::Deserialize;

/// GitHub Releases "latest" API for PowerShell (FR-2). This is the REAL pinned
/// production URL and the default used when the override env var is unset.
pub const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/PowerShell/PowerShell/releases/latest";

/// Stable build-info feed (ADR-0003 anchor, cross-checked with the GitHub tag).
/// REAL pinned production URL and default when the override env var is unset.
pub const BUILD_INFO_STABLE_URL: &str = "https://aka.ms/pwsh-buildinfo-stable";

/// Env var that overrides the GitHub Releases "latest" source URL.
/// **Test-only seam:** unset in production (the default below is the real pinned
/// URL), it lets integration tests point the REAL `RealHttp` path at an
/// in-process mock HTTP server. It does NOT change production behavior and does
/// NOT weaken FR-11 — a failed fetch against whatever URL is in effect still
/// surfaces a [`SourceError`], never a fabricated version.
pub const RELEASES_URL_ENV: &str = "PWSH_AUTOUPDATE_RELEASES_URL";

/// Env var that overrides the stable build-info source URL (same test-only
/// seam contract as [`RELEASES_URL_ENV`]).
pub const BUILD_INFO_URL_ENV: &str = "PWSH_AUTOUPDATE_BUILDINFO_URL";

/// The effective GitHub Releases "latest" URL: the [`RELEASES_URL_ENV`] override
/// if set and non-empty, otherwise the real pinned [`GITHUB_RELEASES_LATEST_URL`].
/// Production never sets the env var, so this returns the real URL unchanged.
pub fn releases_url() -> String {
    env_override(RELEASES_URL_ENV).unwrap_or_else(|| GITHUB_RELEASES_LATEST_URL.to_string())
}

/// The effective stable build-info URL: the [`BUILD_INFO_URL_ENV`] override if
/// set and non-empty, otherwise the real pinned [`BUILD_INFO_STABLE_URL`].
pub fn build_info_url() -> String {
    env_override(BUILD_INFO_URL_ENV).unwrap_or_else(|| BUILD_INFO_STABLE_URL.to_string())
}

/// Read an override env var, treating unset/empty as "no override" so the real
/// default is always used in production.
fn env_override(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// GET from an upstream source. The orchestration uses this to resolve the
/// latest stable PowerShell release; tests inject a fake. Both methods map any
/// failure to [`SourceError`] so no caller can fabricate a version on failure.
pub trait HttpClient {
    /// GET the URL and parse the body as JSON. Network/HTTP failures map to
    /// [`SourceError::Fetch`]; malformed JSON maps to [`SourceError::Parse`].
    fn get_json(&self, url: &str) -> Result<serde_json::Value, SourceError>;

    /// GET the URL and return the raw response body as text (build-info feed).
    /// Same failure mapping as [`HttpClient::get_json`] for the fetch path.
    fn get_text(&self, url: &str) -> Result<String, SourceError>;

    /// GET the URL and return the raw response body bytes, refusing bodies
    /// larger than `max_bytes` (release-asset downloads: the tar.gz payload and
    /// the `hashes.sha256` manifest, which is UTF-16 — not valid `get_text`
    /// input). Failures map to [`SourceError::Fetch`]; an over-limit body is a
    /// fetch failure, never a truncated "success".
    fn get_bytes(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, SourceError>;
}

/// GitHub Releases payload (only the fields we consume). Tolerant of extra and
/// missing fields so upstream evolution does not break parsing (FR-2).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct GitHubRelease {
    /// e.g. "v7.4.6".
    #[serde(default)]
    pub tag_name: String,
    /// True for preview/RC builds (ignored when computing latest stable).
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GitHubAsset>,
}

/// A single downloadable asset on a release.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct GitHubAsset {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub browser_download_url: String,
}

/// The stable build-info feed payload. PowerShell's `aka.ms/pwsh-buildinfo-*`
/// feeds expose a `ReleaseTag` (e.g. "v7.4.6"); other fields are ignored.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct BuildInfo {
    /// e.g. "v7.4.6". The live feed uses PascalCase `ReleaseTag`; aliases
    /// tolerate camelCase/snake_case variants too.
    #[serde(
        default,
        rename = "ReleaseTag",
        alias = "releaseTag",
        alias = "release_tag"
    )]
    pub release_tag: String,
}

impl BuildInfo {
    /// Parse a raw build-info JSON body (the feed is served as text/plain by
    /// `aka.ms`, so we deserialize from a string). A malformed body maps to
    /// [`SourceError::Parse`].
    pub fn from_text(body: &str) -> Result<Self, SourceError> {
        serde_json::from_str(body).map_err(|e| SourceError::Parse(e.to_string()))
    }
}

/// Real blocking HTTP client over `ureq` (3.x). Sets a `User-Agent` (GitHub
/// rejects requests without one). Confined to this adapter — nothing else in
/// the crate touches `ureq` types.
pub struct RealHttp {
    agent: ureq::Agent,
    user_agent: String,
}

impl RealHttp {
    /// Construct a real client with the given `User-Agent`.
    pub fn new(user_agent: impl Into<String>) -> Self {
        Self {
            agent: ureq::Agent::new_with_defaults(),
            user_agent: user_agent.into(),
        }
    }
}

impl HttpClient for RealHttp {
    fn get_json(&self, url: &str) -> Result<serde_json::Value, SourceError> {
        // ureq 3.3's `Body::read_json` is gated behind the non-default `json`
        // capability; to keep the pinned dep (ADR-0005) feature-free we read the
        // body as text and parse with the crate's own `serde_json` dependency.
        let body = self.get_text(url)?;
        serde_json::from_str(&body).map_err(|e| SourceError::Parse(e.to_string()))
    }

    fn get_text(&self, url: &str) -> Result<String, SourceError> {
        let body = self
            .agent
            .get(url)
            .header("User-Agent", &self.user_agent)
            .call()
            .map_err(|e| SourceError::Fetch(e.to_string()))?
            .body_mut()
            .read_to_string()
            .map_err(|e| SourceError::Fetch(e.to_string()))?;
        Ok(body)
    }

    fn get_bytes(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, SourceError> {
        // `read_to_vec` defaults to a 10 MB cap; the caller-provided limit
        // raises it for the tarball while still bounding a runaway body.
        self.agent
            .get(url)
            .header("User-Agent", &self.user_agent)
            .call()
            .map_err(|e| SourceError::Fetch(e.to_string()))?
            .body_mut()
            .with_config()
            .limit(max_bytes)
            .read_to_vec()
            .map_err(|e| SourceError::Fetch(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_urls_are_the_mandated_sources() {
        assert_eq!(
            GITHUB_RELEASES_LATEST_URL,
            "https://api.github.com/repos/PowerShell/PowerShell/releases/latest"
        );
        assert_eq!(
            BUILD_INFO_STABLE_URL,
            "https://aka.ms/pwsh-buildinfo-stable"
        );
    }

    #[test]
    fn github_release_parses_with_only_known_fields_and_ignores_extras() {
        let json = r#"{
            "tag_name": "v7.4.6",
            "prerelease": false,
            "html_url": "https://example.invalid/ignored",
            "assets": [
                { "name": "powershell-7.4.6-linux-x64.tar.gz",
                  "browser_download_url": "https://example.invalid/a.tar.gz",
                  "size": 12345 }
            ]
        }"#;
        let rel: GitHubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(rel.tag_name, "v7.4.6");
        assert!(!rel.prerelease);
        assert_eq!(rel.assets.len(), 1);
        assert_eq!(rel.assets[0].name, "powershell-7.4.6-linux-x64.tar.gz");
    }

    #[test]
    fn github_release_tolerates_missing_optional_fields() {
        // Only tag_name present; assets/prerelease default.
        let rel: GitHubRelease = serde_json::from_str(r#"{ "tag_name": "v7.5.0" }"#).unwrap();
        assert_eq!(rel.tag_name, "v7.5.0");
        assert!(!rel.prerelease);
        assert!(rel.assets.is_empty());
    }

    #[test]
    fn build_info_parses_release_tag() {
        let info = BuildInfo::from_text(
            r#"{ "ReleaseTag": "v7.4.6", "ReleaseDate": "2024-01-01", "BlobName": "x" }"#,
        )
        .unwrap();
        assert_eq!(info.release_tag, "v7.4.6");
    }

    #[test]
    fn build_info_malformed_is_a_parse_source_error_no_version() {
        let err = BuildInfo::from_text("not json").unwrap_err();
        assert!(matches!(err, SourceError::Parse(_)));
    }
}
