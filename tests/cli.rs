//! CLI integration tests.
//!
//! Drives the REAL binary with `assert_cmd`. Beyond the `--help` / `--version`
//! smoke tests, the `--check` flow is exercised end-to-end against an in-process
//! `mockito` HTTP server: the binary's production `RealHttp` -> `app::run_check`
//! path actually performs HTTP GETs (over real sockets, but only to localhost),
//! proving the wiring works without touching the network (FR-10).
//!
//! The two upstream source URLs are redirected at the mock server via the
//! documented test-only env seam (`PWSH_AUTOUPDATE_RELEASES_URL` /
//! `PWSH_AUTOUPDATE_BUILDINFO_URL`); production leaves these unset and hits the
//! real pinned URLs. The assertions are written to hold whether or not the CI
//! host has `pwsh` installed.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn shows_help() {
    Command::cargo_bin("pwsh-autoupdate")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("pwsh-autoupdate"));
}

#[test]
fn shows_version() {
    Command::cargo_bin("pwsh-autoupdate")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}

/// Recorded-realistic stub bodies. A high stable version (v7.6.99) so that when
/// the host DOES have pwsh, an update is "available"; the assertions below do
/// not depend on that, only on the HTTP-resolved value reaching the report.
const STABLE_TAG: &str = "v7.6.99";

fn build_info_body() -> String {
    format!(r#"{{ "ReleaseTag": "{STABLE_TAG}", "ReleaseDate": "2026-01-01", "BlobName": "x" }}"#)
}

fn release_body() -> String {
    format!(
        r#"{{ "tag_name": "{STABLE_TAG}", "prerelease": false, "html_url": "http://ignored.invalid", "assets": [] }}"#
    )
}

/// `--check` driven against a mocked HTTP server through the REAL binary.
///
/// This is the FR-10 end-to-end proof: the actual built executable resolves the
/// latest stable version over HTTP (RealHttp), so if the production HTTP path
/// were bypassed or deleted, the "Latest version: 7.6.99" line could not appear
/// and this test would FAIL. The assertion is robust to the host having or
/// lacking pwsh:
///   * pwsh present  -> `--check` renders a report containing the HTTP-resolved
///     `Latest version:  7.6.99` and exits 0 (up-to-date) or 1 (update avail).
///   * pwsh absent    -> the binary errors honestly ("not installed") and exits
///     2 — it NEVER fabricates a version.
///
/// Either way the exit code is a valid ADR-0002 `--check` code in {0,1,2}.
#[test]
fn check_against_mock_http_server_uses_real_http_path() {
    let mut server = mockito::Server::new();

    let build_info = server
        .mock("GET", "/buildinfo-stable")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body(build_info_body())
        // GitHub's "latest" endpoint may be cross-checked but the build-info feed
        // is always consulted first; allow any number of calls for robustness.
        .expect_at_least(1)
        .create();

    let releases = server
        .mock("GET", "/releases-latest")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(release_body())
        .create();

    let assert = Command::cargo_bin("pwsh-autoupdate")
        .unwrap()
        .arg("--check")
        .env(
            "PWSH_AUTOUPDATE_BUILDINFO_URL",
            format!("{}/buildinfo-stable", server.url()),
        )
        .env(
            "PWSH_AUTOUPDATE_RELEASES_URL",
            format!("{}/releases-latest", server.url()),
        )
        .assert();

    let output = assert.get_output();
    let code = output.status.code().expect("process exited with a signal");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Exit code is always a valid ADR-0002 `--check` code.
    assert!(
        matches!(code, 0..=2),
        "unexpected --check exit code {code}; stdout={stdout:?} stderr={stderr:?}"
    );

    if code == 2 {
        // Host lacks a usable pwsh (or version is unparseable): an honest error,
        // and crucially NO fabricated "Latest version" anywhere (FR-11). The
        // build-info feed is legitimately NOT consulted on this early-exit path,
        // so it is not asserted here (keeps the test robust on a pwsh-less host).
        assert!(
            stderr.contains("error:"),
            "exit 2 must surface an honest error; stderr={stderr:?}"
        );
        assert!(
            !stdout.contains("Latest version"),
            "must not fabricate a version on the error path; stdout={stdout:?}"
        );
    } else {
        // pwsh present: the report carries the HTTP-RESOLVED latest version. This
        // value only exists because the real RealHttp -> resolve path ran against
        // the mock server — deleting that path makes this line disappear.
        //
        // The build-info feed MUST have been hit by the real HTTP path; this is
        // what makes the test fail if the production HTTP path were bypassed.
        build_info.assert();
        assert!(
            stdout.contains("Latest version:  7.6.99"),
            "expected the mock-resolved latest version in the report; stdout={stdout:?}"
        );
        // The GitHub releases cross-check endpoint is consulted on the success
        // path; confirm the real client reached it too.
        releases.assert();
    }
}

/// FR-11 at the CLI seam: when the build-info source fails (HTTP 500), the REAL
/// binary surfaces an honest error and exits 2, with NO fabricated `Latest
/// version` line — regardless of whether the host has pwsh.
#[test]
fn check_source_failure_exits_2_with_no_fabricated_version() {
    let mut server = mockito::Server::new();

    let _build_info = server
        .mock("GET", "/buildinfo-stable")
        .with_status(500)
        .with_body("upstream is down")
        .create();
    // Releases endpoint stubbed too (not reached, since build-info fails first).
    let _releases = server
        .mock("GET", "/releases-latest")
        .with_status(500)
        .with_body("upstream is down")
        .create();

    let assert = Command::cargo_bin("pwsh-autoupdate")
        .unwrap()
        .arg("--check")
        .env(
            "PWSH_AUTOUPDATE_BUILDINFO_URL",
            format!("{}/buildinfo-stable", server.url()),
        )
        .env(
            "PWSH_AUTOUPDATE_RELEASES_URL",
            format!("{}/releases-latest", server.url()),
        )
        .assert();

    let output = assert.get_output();
    let code = output.status.code().expect("process exited with a signal");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Exit 2: either the source failure (pwsh present) or pwsh-absent — both are
    // honest `--check` errors per ADR-0002. Never a fabricated success.
    assert_eq!(
        code, 2,
        "source failure / no-pwsh must exit 2; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("error:"),
        "must surface an honest error; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("Latest version"),
        "must not fabricate a version on the failure path; stdout={stdout:?}"
    );
}
