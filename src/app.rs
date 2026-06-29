//! Host orchestration — the real default code path.
//!
//! This module is the single place where the probe → detect → resolve →
//! classify → (report | run) flow is assembled. Both entry points take the two
//! trait objects (`&dyn HttpClient`, `&dyn CommandRunner`) plus the resolved
//! [`Os`], so the shipping binary (`main.rs`) and the hermetic tests drive the
//! *identical* construction — there is no parallel "old path".
//!
//! Exit codes follow ADR-0002. `main.rs` is the only place that turns these
//! `i32`s into a process exit; this module returns them and writes the
//! human-facing report/error to the provided writers, so tests can assert both
//! the exit code and the rendered output without spawning a process.

use crate::adapters::http::HttpClient;
use crate::adapters::runner::CommandRunner;
use crate::adapters::{probe, resolve_latest_stable};
use crate::core::error::CoreError;
use crate::core::{detect, plan, report::CheckReport, version, Detection, Os, VersionState};
use std::io::Write;

/// `User-Agent` sent on every upstream request (GitHub rejects requests with
/// none). Identifies the tool + version for upstream operators.
pub const USER_AGENT: &str = concat!("pwsh-autoupdate/", env!("CARGO_PKG_VERSION"));

/// Exit codes (ADR-0002). `--check`: 0 up-to-date, 1 update-available, 2 error.
/// Full run: 0 success (incl. up-to-date no-op), non-zero failure.
pub const EXIT_UP_TO_DATE: i32 = 0;
pub const EXIT_UPDATE_AVAILABLE: i32 = 1;
pub const EXIT_CHECK_ERROR: i32 = 2;
pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;

/// Resolve the OS the host is running on into the pure [`Os`] enum.
///
/// Returns `None` for an OS this tool does not support — the host then reports
/// the unsupported platform and exits non-zero, taking no action (FR-1).
pub fn host_os() -> Option<Os> {
    match std::env::consts::OS {
        "windows" => Some(Os::Windows),
        "macos" => Some(Os::Macos),
        "linux" => Some(Os::Linux),
        _ => None,
    }
}

/// The `--check` dry run (FR-7, FR-11; ADR-0002 exit codes).
///
/// Flow: probe pwsh + signals → parse current version → resolve detection method
/// → resolve latest stable over HTTP (the ONLY allowed network read; a failure
/// surfaces with **no** latest-version value, FR-11) → classify → build plan →
/// render the report. **No** package-manager process is run and no mutating
/// side effect occurs (G-3): the `runner` is used only for the read-only probe.
///
/// Returns the ADR-0002 exit code; writes the report to `out` and any error to
/// `err`.
pub fn run_check(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    os: Os,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let signals = probe::probe(runner, os);

    // pwsh absent: report not-installed and exit error — never install (ADR-0001).
    if !signals.pwsh_present {
        let _ = writeln!(
            err,
            "error: PowerShell (pwsh) is not installed; nothing to update (this tool only updates an existing install)"
        );
        return EXIT_CHECK_ERROR;
    }

    // Parse the installed version. A missing/unparseable version is an honest
    // error (code 2) — never fabricated.
    let current = match signals.current_version.as_deref() {
        Some(raw) => match version::parse(raw) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(err, "error: {e}");
                return EXIT_CHECK_ERROR;
            }
        },
        None => {
            let _ = writeln!(
                err,
                "error: could not determine the installed PowerShell version"
            );
            return EXIT_CHECK_ERROR;
        }
    };

    let detection = detect::resolve(os, &signals);

    // Resolve the latest stable over HTTP. On any source failure, print the
    // error and NO latest-version value (FR-11), exit 2.
    let latest = match resolve_latest_stable(http) {
        Ok(info) => info,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_CHECK_ERROR;
        }
    };

    let state = version::classify(&current, &latest.version);

    // Build the plan only when a method is determined AND an update is available
    // (and the (method, os) combination is supported). The same plan is what the
    // update path would execute (FR-9 agreement). A plan-build failure is an
    // honest error.
    let plan = match plan_for(&detection, state, &current, &latest.version, os) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_CHECK_ERROR;
        }
    };

    let report = CheckReport::build(current, latest.version, state, detection, plan);
    let _ = writeln!(out, "{report}");

    match state {
        VersionState::UpToDate => EXIT_UP_TO_DATE,
        VersionState::UpdateAvailable => EXIT_UPDATE_AVAILABLE,
    }
}

/// Build the plan iff a method is selected and an update is available; otherwise
/// `None`. Shared by `run_check` (reporting) and `run_update` (execution) so the
/// reported command equals the executed one (FR-9).
fn plan_for(
    detection: &Detection,
    state: VersionState,
    current: &semver::Version,
    latest: &semver::Version,
    os: Os,
) -> Result<Option<crate::core::UpdatePlan>, CoreError> {
    match (detection.selected, state) {
        (Some(method), VersionState::UpdateAvailable) => {
            let p = plan::build_plan(method, current.clone(), latest.clone(), os)?;
            Ok(Some(p))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::http::{BUILD_INFO_STABLE_URL, GITHUB_RELEASES_LATEST_URL};
    use crate::core::error::SourceError;
    use crate::core::InstallMethod;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    // --- Fakes (the production trait objects, with canned data) --------------

    #[derive(Default)]
    struct FakeHttp {
        bodies: HashMap<String, String>,
        fail_url: Option<String>,
    }
    impl FakeHttp {
        fn ok(current_latest: &str) -> Self {
            let mut bodies = HashMap::new();
            bodies.insert(
                BUILD_INFO_STABLE_URL.to_string(),
                format!(r#"{{ "ReleaseTag": "v{current_latest}" }}"#),
            );
            bodies.insert(
                GITHUB_RELEASES_LATEST_URL.to_string(),
                format!(r#"{{ "tag_name": "v{current_latest}", "prerelease": false }}"#),
            );
            Self {
                bodies,
                fail_url: None,
            }
        }
        fn failing() -> Self {
            Self {
                bodies: HashMap::new(),
                fail_url: Some(BUILD_INFO_STABLE_URL.to_string()),
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
                return Err(SourceError::Fetch("offline".into()));
            }
            self.bodies
                .get(url)
                .cloned()
                .ok_or_else(|| SourceError::Fetch(format!("no fake for {url}")))
        }
    }

    #[derive(Default)]
    struct FakeRunner {
        present: HashSet<String>,
        outputs: HashMap<String, crate::adapters::runner::CmdOutput>,
        runs: RefCell<Vec<(String, Vec<String>)>>,
    }
    impl FakeRunner {
        fn pwsh(version_line: &str) -> Self {
            let mut r = Self::default();
            let exe = probe::pwsh_exe().to_string();
            r.present.insert(exe.clone());
            r.outputs.insert(
                exe,
                crate::adapters::runner::CmdOutput {
                    status: 0,
                    stdout: version_line.to_string(),
                    stderr: String::new(),
                },
            );
            r
        }
        fn with_manager(mut self, program: &str, list_stdout: &str) -> Self {
            self.present.insert(program.to_string());
            self.outputs.insert(
                program.to_string(),
                crate::adapters::runner::CmdOutput {
                    status: 0,
                    stdout: list_stdout.to_string(),
                    stderr: String::new(),
                },
            );
            self
        }
    }
    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
        ) -> std::io::Result<crate::adapters::runner::CmdOutput> {
            self.runs.borrow_mut().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            self.outputs.get(program).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, format!("no fake: {program}"))
            })
        }
        fn exists(&self, program: &str) -> bool {
            self.present.contains(program)
        }
    }

    /// Only the mutating runs (i.e. anything other than the read-only probe
    /// calls `pwsh --version` and the manager-list ownership queries).
    fn mutating_runs(runner: &FakeRunner) -> Vec<(String, Vec<String>)> {
        let exe = probe::pwsh_exe();
        runner
            .runs
            .borrow()
            .iter()
            .filter(|(p, args)| {
                // probe reads: `pwsh --version`, and manager *list/-s/-q/--pkgs*
                // ownership queries. Anything that is an upgrade/refresh/install
                // is mutating.
                let is_pwsh_version = p == exe && args == &vec!["--version".to_string()];
                let is_list_query = args.iter().any(|a| {
                    a == "list"
                        || a == "-s"
                        || a == "-q"
                        || a.starts_with("--pkgs")
                        || a == "--local-only"
                });
                !(is_pwsh_version || is_list_query)
            })
            .cloned()
            .collect()
    }

    #[test]
    fn check_update_available_exits_1_and_runs_nothing_mutating() {
        // Installed 7.4.0, latest 7.5.0, dpkg owns it -> AptDpkg selected.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0")
            .with_manager("dpkg", "Package: powershell\nStatus: install ok installed");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_check(&http, &runner, Os::Linux, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        assert_eq!(code, EXIT_UPDATE_AVAILABLE);
        assert!(stdout.contains("Current version: 7.4.0"));
        assert!(stdout.contains("Latest version:  7.5.0"));
        assert!(stdout.contains("update available"));
        assert!(stdout.contains("apt-get install --only-upgrade -y powershell"));
        // G-3: zero mutating runner calls during --check.
        assert!(
            mutating_runs(&runner).is_empty(),
            "--check must run no mutating commands, saw {:?}",
            mutating_runs(&runner)
        );
    }

    #[test]
    fn check_up_to_date_exits_0() {
        let http = FakeHttp::ok("7.4.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0")
            .with_manager("dpkg", "Package: powershell\nStatus: install ok installed");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_check(&http, &runner, Os::Linux, &mut out, &mut err);
        assert_eq!(code, EXIT_UP_TO_DATE);
        assert!(String::from_utf8(out).unwrap().contains("up to date"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn check_source_failure_exits_2_and_prints_no_version() {
        let http = FakeHttp::failing();
        let runner = FakeRunner::pwsh("PowerShell 7.4.0");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_check(&http, &runner, Os::Linux, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_eq!(code, EXIT_CHECK_ERROR);
        assert!(stdout.is_empty(), "no report on source failure");
        assert!(stderr.contains("error:"));
        // No fabricated "Latest version" anywhere (FR-11).
        assert!(!stdout.contains("Latest version"));
        assert!(!stderr.contains("Latest version"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn check_pwsh_absent_exits_2_without_installing() {
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default(); // pwsh not present
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_check(&http, &runner, Os::Linux, &mut out, &mut err);
        assert_eq!(code, EXIT_CHECK_ERROR);
        assert!(String::from_utf8(err).unwrap().contains("not installed"));
        assert!(runner.runs.borrow().is_empty());
    }

    #[test]
    fn check_undetermined_method_still_reports_versions() {
        // pwsh present, but no manager owns it -> undetermined; update available.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_check(&http, &runner, Os::Linux, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        // Update available even though no command can be produced.
        assert_eq!(code, EXIT_UPDATE_AVAILABLE);
        assert!(stdout.contains("Current version: 7.4.0"));
        assert!(stdout.contains("Latest version:  7.5.0"));
        assert!(stdout.contains("undetermined"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn host_os_maps_supported_platforms() {
        // Whatever this test runs on must be one of the three supported OSes.
        assert!(host_os().is_some());
    }

    #[test]
    fn plan_for_agrees_with_check_report_command() {
        let detection = Detection {
            selected: Some(InstallMethod::AptDpkg),
            also_detected: vec![],
        };
        let p = plan_for(
            &detection,
            VersionState::UpdateAvailable,
            &semver::Version::parse("7.4.0").unwrap(),
            &semver::Version::parse("7.5.0").unwrap(),
            Os::Linux,
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.program, "apt-get");
    }
}
