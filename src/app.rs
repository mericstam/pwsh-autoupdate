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

/// Best-effort check of whether the current process holds the elevated
/// privileges a system-scope upgrade needs (FR-12). The tool NEVER self-elevates
/// — it only reports the requirement when it is unmet.
///
/// On Unix this is "are we root (euid 0)?". On Windows there is no portable
/// libc check without extra crates; conservatively report `false` so the host
/// surfaces the requirement rather than silently attempting a privileged action
/// it may not be allowed to perform.
pub fn has_elevated_privileges() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` is always safe to call and has no preconditions.
        unsafe { libc_geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

/// The default update path (FR-1/6/11/12; ADR-0001/0002).
///
/// Builds the real privilege check and delegates to [`run_update_with`]. This is
/// the function `main.rs` calls; tests drive [`run_update_with`] with an injected
/// `elevated` flag for determinism.
pub fn run_update(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    os: Os,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    run_update_with(http, runner, os, has_elevated_privileges(), out, err)
}

/// The update flow with the privilege state injected (testable).
///
/// Flow (design Section 7):
/// 1. probe; pwsh absent → report not-installed, exit non-zero, no install
///    (ADR-0001).
/// 2. parse current version (honest error on failure).
/// 3. resolve latest stable over HTTP; failure → exit non-zero, no fabricated
///    version (FR-11).
/// 4. up-to-date → exit 0 (no-op).
/// 5. method undetermined → report, exit non-zero, attempt no update.
/// 6. build the plan (same plan as `--check`, FR-9).
/// 7. `requires_elevation && !elevated` → surface the requirement, exit
///    non-zero, NEVER self-elevate (FR-12).
/// 8. manager not on PATH → name it, exit non-zero, try no other channel (FR-6).
/// 9. run the command (the ONLY mutating call); non-zero status → surface
///    stderr, exit non-zero, never report success (FR-6).
/// 10. success → exit 0.
pub fn run_update_with(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    os: Os,
    elevated: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let signals = probe::probe(runner, os);

    if !signals.pwsh_present {
        let _ = writeln!(
            err,
            "error: PowerShell (pwsh) is not installed; nothing to update (this tool only updates an existing install)"
        );
        return EXIT_FAILURE;
    }

    let current = match signals.current_version.as_deref() {
        Some(raw) => match version::parse(raw) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(err, "error: {e}");
                return EXIT_FAILURE;
            }
        },
        None => {
            let _ = writeln!(
                err,
                "error: could not determine the installed PowerShell version"
            );
            return EXIT_FAILURE;
        }
    };

    let detection = detect::resolve(os, &signals);

    let latest = match resolve_latest_stable(http) {
        Ok(info) => info,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_FAILURE;
        }
    };

    let state = version::classify(&current, &latest.version);
    if state == VersionState::UpToDate {
        let _ = writeln!(
            out,
            "PowerShell {current} is up to date (latest stable: {}). Nothing to do.",
            latest.version
        );
        return EXIT_SUCCESS;
    }

    // Update is available — we need a determined method to proceed.
    let method = match detection.selected {
        Some(m) => m,
        None => {
            let _ = writeln!(
                err,
                "error: could not determine how PowerShell was installed; no upgrade command can be produced. Update PowerShell manually."
            );
            return EXIT_FAILURE;
        }
    };

    let plan = match plan::build_plan(method, current.clone(), latest.version.clone(), os) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_FAILURE;
        }
    };

    // FR-12: surface a privilege requirement; never self-elevate.
    if plan.requires_elevation && !elevated {
        let _ = writeln!(
            err,
            "error: updating PowerShell via {} requires elevated privileges. Re-run with elevation (e.g. sudo / an elevated shell); this tool will not self-elevate.",
            method.label()
        );
        return EXIT_FAILURE;
    }

    // FR-6: the owning manager must be on PATH; do not fall back to another channel.
    if !runner.exists(&plan.program) {
        let _ = writeln!(
            err,
            "error: the required package manager '{}' (owning channel: {}) was not found on PATH; not attempting any other channel.",
            plan.program,
            method.label()
        );
        return EXIT_FAILURE;
    }

    let cmd_display = if plan.args.is_empty() {
        plan.program.clone()
    } else {
        format!("{} {}", plan.program, plan.args.join(" "))
    };
    let _ = writeln!(
        out,
        "Updating PowerShell {} -> {} via {} ...",
        current,
        latest.version,
        method.label()
    );
    let _ = writeln!(out, "Running: {cmd_display}");

    // The ONLY mutating call.
    let arg_refs: Vec<&str> = plan.args.iter().map(String::as_str).collect();
    match runner.run(&plan.program, &arg_refs) {
        Ok(output) if output.status == 0 => {
            let _ = writeln!(
                out,
                "PowerShell updated successfully to {}.",
                latest.version
            );
            EXIT_SUCCESS
        }
        Ok(output) => {
            // Non-zero manager exit: surface stderr, NEVER report success (FR-6).
            let _ = writeln!(
                err,
                "error: '{}' exited with status {}.",
                plan.program, output.status
            );
            if !output.stderr.trim().is_empty() {
                let _ = writeln!(err, "{}", output.stderr.trim_end());
            }
            EXIT_FAILURE
        }
        Err(e) => {
            let _ = writeln!(
                err,
                "error: failed to run '{}': {e}. PowerShell was not updated.",
                plan.program
            );
            EXIT_FAILURE
        }
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
        /// When set, a mutating (upgrade/refresh/install) invocation returns this
        /// non-zero status with the given stderr — modelling a manager failure
        /// while leaving the read-only probe list-queries succeeding.
        fail_upgrade: Option<(i32, String)>,
    }
    fn is_mutating_args(args: &[&str]) -> bool {
        args.iter()
            .any(|a| *a == "upgrade" || *a == "refresh" || *a == "install")
    }
    impl FakeRunner {
        fn fail_upgrade(mut self, status: i32, stderr: &str) -> Self {
            self.fail_upgrade = Some((status, stderr.to_string()));
            self
        }
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
            if let Some((status, stderr)) = &self.fail_upgrade {
                if is_mutating_args(args) {
                    return Ok(crate::adapters::runner::CmdOutput {
                        status: *status,
                        stdout: String::new(),
                        stderr: stderr.clone(),
                    });
                }
            }
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

    // --- run_update path ----------------------------------------------------

    #[test]
    fn update_runs_detected_command_and_exits_0_on_success() {
        // macOS Homebrew: no elevation required; happy path.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0").with_manager("brew", "powershell");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Macos, false, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        assert_eq!(code, EXIT_SUCCESS);
        assert!(stdout.contains("updated successfully"));
        // The exact detected command ran (FR-9 agreement: brew upgrade powershell).
        let muts = mutating_runs(&runner);
        assert_eq!(
            muts,
            vec![(
                "brew".to_string(),
                vec!["upgrade".to_string(), "powershell".to_string()]
            )]
        );
    }

    #[test]
    fn update_up_to_date_is_noop_exit_0_no_mutation() {
        let http = FakeHttp::ok("7.4.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0").with_manager("brew", "powershell");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Macos, false, &mut out, &mut err);
        assert_eq!(code, EXIT_SUCCESS);
        assert!(String::from_utf8(out).unwrap().contains("up to date"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn update_manager_nonzero_exit_surfaces_failure_never_success() {
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0")
            .with_manager("brew", "powershell")
            .fail_upgrade(1, "brew: upgrade failed: locked");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Macos, false, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(!stdout.contains("updated successfully"));
        assert!(stderr.contains("exited with status 1"));
        assert!(stderr.contains("upgrade failed"));
    }

    #[test]
    fn update_manager_missing_on_path_names_it_and_exits_nonzero() {
        // dpkg owns pwsh (detected) but apt-get is not on PATH.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0")
            .with_manager("dpkg", "Package: powershell\nStatus: install ok installed");
        // apt-get is the plan program and is NOT present -> not on PATH.
        let mut out = Vec::new();
        let mut err = Vec::new();
        // elevated=true so the elevation gate does not trip first.
        let code = run_update_with(&http, &runner, Os::Linux, true, &mut out, &mut err);
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(stderr.contains("apt-get"));
        assert!(stderr.contains("not found on PATH"));
        assert!(stderr.contains("not attempting any other channel"));
        assert!(
            mutating_runs(&runner).is_empty(),
            "must not run the upgrade"
        );
    }

    #[test]
    fn update_elevation_required_but_absent_surfaces_and_exits_nonzero() {
        // Linux apt requires elevation; not elevated -> surface, no self-elevate.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0")
            .with_manager("dpkg", "Package: powershell\nStatus: install ok installed")
            .with_manager("apt-get", "");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Linux, false, &mut out, &mut err);
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(stderr.contains("elevated privileges"));
        assert!(stderr.contains("will not self-elevate"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn update_undetermined_method_exits_nonzero_no_action() {
        // pwsh present, update available, but no manager owns it.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Linux, true, &mut out, &mut err);
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(stderr.contains("could not determine how PowerShell was installed"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn update_pwsh_absent_exits_nonzero_without_installing() {
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Linux, false, &mut out, &mut err);
        assert_ne!(code, EXIT_SUCCESS);
        assert!(String::from_utf8(err).unwrap().contains("not installed"));
        assert!(runner.runs.borrow().is_empty());
    }

    #[test]
    fn update_source_failure_exits_nonzero_no_fabricated_version() {
        let http = FakeHttp::failing();
        let runner = FakeRunner::pwsh("PowerShell 7.4.0").with_manager("brew", "powershell");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Macos, false, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(!stdout.contains("Latest"));
        assert!(!stderr.contains("Latest version"));
        assert!(mutating_runs(&runner).is_empty());
    }
}
