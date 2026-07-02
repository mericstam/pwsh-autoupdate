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
use crate::adapters::{portable, probe, resolve_latest_stable};
use crate::core::error::CoreError;
use crate::core::{
    detect, plan, report::CheckReport, version, Detection, InstallMethod, Os, ReleaseInfo,
    VersionState,
};
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

    // pwsh absent: dry-run report of the from-scratch install (ADR-0006). No
    // side effect — `--check` only reports what `would` install.
    if !signals.pwsh_present {
        match plan::resolve_install(os, &signals.available_managers) {
            Some(ip) => {
                let _ = writeln!(out, "PowerShell is not installed.");
                // The latest version is informational; the native installer
                // always fetches the current release itself. A source failure
                // must not block the install report (and never fabricates).
                if let Ok(latest) = resolve_latest_stable(http) {
                    let _ = writeln!(out, "Latest version:  {}", latest.version);
                }
                let _ = writeln!(
                    out,
                    "Would install:   {} (via {})",
                    render_command(&ip.program, &ip.args),
                    ip.method.label()
                );
                return EXIT_UPDATE_AVAILABLE;
            }
            None => {
                let _ = writeln!(
                    err,
                    "error: PowerShell is not installed and no supported package manager (winget/Chocolatey, Homebrew, snap) was found to install it; install PowerShell manually."
                );
                return EXIT_CHECK_ERROR;
            }
        }
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

/// The default update path (FR-1/6/11/12; ADR-0002/0006).
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
/// 1. probe; pwsh absent → auto-install from scratch via the native channel
///    (ADR-0006); no native channel → honest "cannot install" error.
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

    // pwsh absent: auto-install from scratch (ADR-0006) instead of erroring. We
    // only install when NONE is present, so this never creates a second,
    // conflicting install.
    if !signals.pwsh_present {
        return install_missing(runner, os, &signals.available_managers, elevated, out, err);
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

    // Portable tar.gz runs the download-and-replace IN-PROCESS. The plan still
    // reads `pwsh-autoupdate --replace-portable` (FR-9: running that command
    // manually does exactly this), but we never spawn ourselves via a PATH
    // lookup — a shadowing binary earlier in PATH must not be executed instead.
    // Privilege needs are checked against the actual install dir at runtime
    // (a user-owned ~/powershell needs none; /opt/... surfaces the FR-12 error).
    if plan.method == InstallMethod::PortableTarGz {
        let _ = writeln!(
            out,
            "Updating PowerShell {} -> {} via {} ...",
            current,
            latest.version,
            method.label()
        );
        let _ = writeln!(
            out,
            "Running: {}",
            render_command(&plan.program, &plan.args)
        );
        return portable_update(http, runner, &latest, out, err);
    }

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

    let cmd_display = render_command(&plan.program, &plan.args);
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
            run_post_steps(runner, &plan, out, err);
            let _ = writeln!(
                out,
                "PowerShell updated successfully to {}.",
                latest.version
            );
            EXIT_SUCCESS
        }
        Ok(output) => {
            // Non-zero manager exit. Some managers (notably winget) return a
            // non-zero status even when the upgrade was actually applied — e.g.
            // the package being upgraded is the running shell, or a follow-up
            // invocation finds "no applicable upgrade" because the work is
            // already done. We must still NEVER report success on a real failure
            // (FR-6), so we do not trust the exit code in *either* direction:
            // re-probe the installed version and report success only if
            // PowerShell is now actually at (or above) the target version.
            let reprobed = probe::probe(runner, os);
            let verified = reprobed
                .current_version
                .as_deref()
                .and_then(|raw| version::parse(raw).ok())
                .map(|now| version::classify(&now, &latest.version) == VersionState::UpToDate)
                .unwrap_or(false);

            if verified {
                run_post_steps(runner, &plan, out, err);
                let _ = writeln!(
                    out,
                    "PowerShell updated successfully to {} ('{}' exited with status {}, but the installed version now matches the target).",
                    latest.version, plan.program, output.status
                );
                return EXIT_SUCCESS;
            }

            // Genuine failure: surface stderr, NEVER report success (FR-6).
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

/// The `--replace-portable` entry point (A-6): the command the update path
/// reports for a portable tar.gz install, runnable directly. Probes and
/// validates that the installed pwsh really is a portable install (never
/// replaces anything else), resolves the latest stable, and performs the
/// download-and-replace. Exit codes: 0 success (incl. up-to-date no-op),
/// non-zero failure.
pub fn run_replace_portable(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    os: Os,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    // The portable tar.gz channel is defined for Linux only (core::plan).
    if os != Os::Linux {
        let _ = writeln!(
            err,
            "error: --replace-portable supports Linux portable tar.gz installs only."
        );
        return EXIT_FAILURE;
    }

    let signals = probe::probe(runner, os);
    if !signals.pwsh_present {
        let _ = writeln!(
            err,
            "error: PowerShell is not installed; there is no portable install to replace."
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

    // Only a portable-detected install may be replaced (FR-6: never use a
    // different channel than the owning one).
    let detection = detect::resolve(os, &signals);
    if detection.selected != Some(InstallMethod::PortableTarGz) {
        let _ = writeln!(
            err,
            "error: the installed PowerShell is not a portable tar.gz install (detected: {}); use `pwsh-autoupdate` to update via the owning channel.",
            detection
                .selected
                .map(InstallMethod::label)
                .unwrap_or("undetermined")
        );
        return EXIT_FAILURE;
    }

    let latest = match resolve_latest_stable(http) {
        Ok(info) => info,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_FAILURE;
        }
    };

    if version::classify(&current, &latest.version) == VersionState::UpToDate {
        let _ = writeln!(
            out,
            "PowerShell {current} is up to date (latest stable: {}). Nothing to do.",
            latest.version
        );
        return EXIT_SUCCESS;
    }

    let _ = writeln!(
        out,
        "Updating PowerShell {} -> {} via portable tar.gz ...",
        current, latest.version
    );
    portable_update(http, runner, &latest, out, err)
}

/// Perform the portable download-and-replace to `latest` and map the outcome
/// to an exit code. The adapter verifies the swapped-in binary reports the
/// target version before discarding the previous install (and rolls back
/// otherwise), so an `Ok` here IS the verified outcome — success is never
/// reported on a real failure (FR-6).
fn portable_update(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    latest: &ReleaseInfo,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    match portable::replace_portable(http, runner, &latest.version, out) {
        Ok(()) => {
            let _ = writeln!(
                out,
                "PowerShell updated successfully to {}.",
                latest.version
            );
            EXIT_SUCCESS
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}. PowerShell was not updated.");
            EXIT_FAILURE
        }
    }
}

/// Run a plan's follow-up steps after the primary upgrade succeeded.
///
/// These are best-effort self-healing steps (Homebrew's
/// `brew link --overwrite powershell`, which repairs the `pwsh` symlink that
/// `brew upgrade` leaves unlinked on a file conflict). The new version is
/// already installed by the time we get here, so a failing step is surfaced as
/// a WARNING with the manual remediation command — never as a failed update.
fn run_post_steps(
    runner: &dyn CommandRunner,
    plan: &crate::core::UpdatePlan,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    for step in &plan.post_steps {
        let display = render_command(&step.program, &step.args);
        let _ = writeln!(out, "Running: {display}");
        let arg_refs: Vec<&str> = step.args.iter().map(String::as_str).collect();
        match runner.run(&step.program, &arg_refs) {
            Ok(o) if o.status == 0 => {}
            Ok(o) => {
                let _ = writeln!(
                    err,
                    "warning: follow-up step '{display}' exited with status {}; PowerShell was updated but you may need to run `{display}` manually if `pwsh` still points at the old version.",
                    o.status
                );
                if !o.stderr.trim().is_empty() {
                    let _ = writeln!(err, "{}", o.stderr.trim_end());
                }
            }
            Err(e) => {
                let _ = writeln!(
                    err,
                    "warning: failed to run follow-up step '{display}': {e}; PowerShell was updated but you may need to run `{display}` manually if `pwsh` still points at the old version."
                );
            }
        }
    }
}

/// Render a `program args...` command line for display in reports and logs.
fn render_command(program: &str, args: &[String]) -> String {
    if args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", args.join(" "))
    }
}

/// Install PowerShell from scratch when none is present (ADR-0006): resolve the
/// native channel and run it. Honors FR-12 (never self-elevate) and FR-6 (the
/// owning manager must be on PATH; never fall back to another channel). When no
/// native channel is available the host falls back to the portable installer
/// (delivered separately) — until then this is an honest "cannot install"
/// error, never a stub. We are called only when pwsh is absent, so installing
/// can never create a second, conflicting install.
fn install_missing(
    runner: &dyn CommandRunner,
    os: Os,
    available: &[InstallMethod],
    elevated: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let ip = match plan::resolve_install(os, available) {
        Some(ip) => ip,
        None => {
            let _ = writeln!(
                err,
                "error: PowerShell is not installed and no supported package manager (winget/Chocolatey, Homebrew, snap) was found to install it; install PowerShell manually."
            );
            return EXIT_FAILURE;
        }
    };

    // FR-12: surface a privilege requirement; never self-elevate.
    if ip.requires_elevation && !elevated {
        let _ = writeln!(
            err,
            "error: installing PowerShell via {} requires elevated privileges. Re-run with elevation (e.g. sudo / an elevated shell); this tool will not self-elevate.",
            ip.method.label()
        );
        return EXIT_FAILURE;
    }

    // FR-6: the install manager must be on PATH (it came from the available set,
    // but assert before the mutating call rather than assume).
    if !runner.exists(&ip.program) {
        let _ = writeln!(
            err,
            "error: the required package manager '{}' was not found on PATH; not attempting any other channel.",
            ip.program
        );
        return EXIT_FAILURE;
    }

    let _ = writeln!(
        out,
        "PowerShell is not installed. Installing via {} ...",
        ip.method.label()
    );
    let _ = writeln!(out, "Running: {}", render_command(&ip.program, &ip.args));

    // The ONLY mutating call.
    let arg_refs: Vec<&str> = ip.args.iter().map(String::as_str).collect();
    match runner.run(&ip.program, &arg_refs) {
        Ok(output) if output.status == 0 => {
            let _ = writeln!(
                out,
                "PowerShell installed successfully via {}.",
                ip.method.label()
            );
            EXIT_SUCCESS
        }
        Ok(output) => {
            let _ = writeln!(
                err,
                "error: install via {} failed (exit {}). PowerShell was not installed.",
                ip.method.label(),
                output.status
            );
            if !output.stderr.trim().is_empty() {
                let _ = writeln!(err, "{}", output.stderr.trim_end());
            }
            EXIT_FAILURE
        }
        Err(e) => {
            let _ = writeln!(
                err,
                "error: failed to run installer '{}': {e}. PowerShell was not installed.",
                ip.program
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
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    // --- Fakes (the production trait objects, with canned data) --------------

    #[derive(Default)]
    struct FakeHttp {
        bodies: HashMap<String, String>,
        raw: HashMap<String, Vec<u8>>,
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
                raw: HashMap::new(),
                fail_url: None,
            }
        }
        fn failing() -> Self {
            Self {
                bodies: HashMap::new(),
                raw: HashMap::new(),
                fail_url: Some(BUILD_INFO_STABLE_URL.to_string()),
            }
        }
        /// Register a binary body (release-asset download fixture).
        fn raw(mut self, url: &str, body: Vec<u8>) -> Self {
            self.raw.insert(url.to_string(), body);
            self
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
        fn get_bytes(&self, url: &str, _max_bytes: u64) -> Result<Vec<u8>, SourceError> {
            if let Some(body) = self.raw.get(url) {
                return Ok(body.clone());
            }
            self.get_text(url).map(String::into_bytes)
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
        /// When set, `pwsh --version` returns this line *after* a mutating
        /// invocation has run — modelling a manager that applied the upgrade
        /// (so a re-probe sees the new version) even if it exited non-zero.
        post_upgrade_version: Option<String>,
        /// When set, a `link`-style follow-up step fails with this status/stderr
        /// while the primary upgrade still succeeds — modelling Homebrew's
        /// relink failing after a successful upgrade.
        fail_post_step: Option<(i32, String)>,
        /// When set, `resolve_program_path` reports this as the canonical pwsh
        /// binary path — modelling a portable install at that location.
        resolved_pwsh: Option<String>,
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
        fn fail_post_step(mut self, status: i32, stderr: &str) -> Self {
            self.fail_post_step = Some((status, stderr.to_string()));
            self
        }
        /// After any mutating call, `pwsh --version` reports `version_line`.
        fn upgrades_to(mut self, version_line: &str) -> Self {
            self.post_upgrade_version = Some(version_line.to_string());
            self
        }
        /// Model the canonical on-disk location of the pwsh binary (portable
        /// installs resolve to a real path inside the install tree).
        fn resolved_at(mut self, path: &str) -> Self {
            self.resolved_pwsh = Some(path.to_string());
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
            // Model a manager that actually applied the upgrade: once a mutating
            // call has run, a re-probe of `pwsh --version` sees the new version.
            if let Some(newv) = &self.post_upgrade_version {
                let exe = probe::pwsh_exe();
                if program == exe && args == ["--version"] {
                    let upgraded = self.runs.borrow().iter().any(|(_, a)| {
                        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
                        is_mutating_args(&refs)
                    });
                    if upgraded {
                        return Ok(crate::adapters::runner::CmdOutput {
                            status: 0,
                            stdout: newv.clone(),
                            stderr: String::new(),
                        });
                    }
                }
            }
            if let Some((status, stderr)) = &self.fail_upgrade {
                if is_mutating_args(args) {
                    return Ok(crate::adapters::runner::CmdOutput {
                        status: *status,
                        stdout: String::new(),
                        stderr: stderr.clone(),
                    });
                }
            }
            if let Some((status, stderr)) = &self.fail_post_step {
                if args.contains(&"link") {
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
        fn resolve_program_path(&self, _program: &str) -> Option<String> {
            self.resolved_pwsh.clone()
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
    fn check_pwsh_absent_no_native_manager_exits_2() {
        // Absent + no supported installer -> honest error, exit 2, no mutation.
        // (Portable install is delivered separately, ADR-0006 step 2.)
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default(); // pwsh not present, no managers
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_check(&http, &runner, Os::Linux, &mut out, &mut err);
        assert_eq!(code, EXIT_CHECK_ERROR);
        assert!(String::from_utf8(err).unwrap().contains("not installed"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn check_pwsh_absent_with_manager_reports_would_install_no_side_effect() {
        // Absent + snap available -> dry-run reports the install command, exit 1,
        // and performs NO mutation (--check is side-effect free, ADR-0006).
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default().with_manager("snap", "");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_check(&http, &runner, Os::Linux, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        assert_eq!(code, EXIT_UPDATE_AVAILABLE);
        assert!(stdout.contains("not installed"));
        assert!(stdout.contains("Would install:"));
        assert!(stdout.contains("snap install powershell --classic"));
        assert!(stdout.contains("Latest version:  7.5.0"));
        assert!(mutating_runs(&runner).is_empty());
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
        // The exact detected command ran (FR-9 agreement: brew upgrade powershell),
        // followed by the self-healing relink (brew link --overwrite powershell).
        let muts = mutating_runs(&runner);
        assert_eq!(
            muts,
            vec![
                (
                    "brew".to_string(),
                    vec!["upgrade".to_string(), "powershell".to_string()]
                ),
                (
                    "brew".to_string(),
                    vec![
                        "link".to_string(),
                        "--overwrite".to_string(),
                        "powershell".to_string()
                    ]
                )
            ]
        );
    }

    #[test]
    fn update_brew_failed_relink_warns_but_still_reports_success() {
        // The upgrade succeeded; only the follow-up `brew link --overwrite`
        // failed. That is a WARNING (pwsh is installed) — never a failed update.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::pwsh("PowerShell 7.4.0")
            .with_manager("brew", "powershell")
            .fail_post_step(1, "Error: Could not symlink bin/pwsh");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Macos, false, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_eq!(code, EXIT_SUCCESS);
        assert!(stdout.contains("updated successfully"));
        assert!(stderr.contains("warning:"));
        assert!(stderr.contains("brew link --overwrite powershell"));
        assert!(!stderr.contains("error:"));
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
    fn update_nonzero_exit_but_version_now_at_target_reports_success() {
        // winget commonly exits non-zero even when the upgrade was applied (e.g.
        // the upgraded package is the running shell). If a re-probe shows the
        // installed version is now at the target, this is a success, not a
        // failure — we verify the actual outcome rather than trusting the code.
        let http = FakeHttp::ok("7.6.3");
        let runner = FakeRunner::pwsh("PowerShell 7.6.1")
            .with_manager("winget", "Microsoft.PowerShell 7.6.1")
            .fail_upgrade(-1978335189, "No available upgrade found.")
            .upgrades_to("PowerShell 7.6.3");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Windows, true, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_eq!(code, EXIT_SUCCESS);
        assert!(stdout.contains("updated successfully to 7.6.3"));
        // The non-zero status is surfaced for transparency, but not as an error.
        assert!(!stderr.contains("error:"));
        // The upgrade really was attempted.
        assert!(!mutating_runs(&runner).is_empty());
    }

    #[test]
    fn update_nonzero_exit_and_version_unchanged_still_fails() {
        // Same non-zero exit, but the version did NOT advance -> genuine failure
        // (FR-6: never report success on a real failure).
        let http = FakeHttp::ok("7.6.3");
        let runner = FakeRunner::pwsh("PowerShell 7.6.1")
            .with_manager("winget", "Microsoft.PowerShell 7.6.1")
            .fail_upgrade(-1978335189, "Installer failed.");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Windows, true, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(!stdout.contains("updated successfully"));
        assert!(stderr.contains("error:"));
        assert!(stderr.contains("Installer failed."));
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
    fn update_pwsh_absent_no_native_manager_errors_without_installing() {
        // Absent + no supported installer -> honest error, no mutation. (Portable
        // install is delivered separately, ADR-0006 step 2.)
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Linux, false, &mut out, &mut err);
        assert_ne!(code, EXIT_SUCCESS);
        let stderr = String::from_utf8(err).unwrap();
        assert!(stderr.contains("not installed"));
        assert!(stderr.contains("no supported package manager"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn update_pwsh_absent_auto_installs_via_native_manager() {
        // Absent + snap available + elevated -> runs the install command (ADR-0006).
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default().with_manager("snap", "");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Linux, true, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        assert_eq!(code, EXIT_SUCCESS);
        assert!(stdout.contains("Installing via snap"));
        assert!(stdout.contains("installed successfully"));
        let muts = mutating_runs(&runner);
        assert_eq!(muts.len(), 1, "exactly one mutating (install) call");
        assert_eq!(muts[0].0, "snap");
        assert_eq!(muts[0].1, vec!["install", "powershell", "--classic"]);
    }

    #[test]
    fn update_pwsh_absent_install_needs_elevation_does_not_self_elevate() {
        // Absent + snap available but not elevated -> surface requirement, no
        // mutation, never self-elevate (FR-12).
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default().with_manager("snap", "");
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
    fn update_pwsh_absent_install_failure_surfaces_and_exits_nonzero() {
        // Absent + manager present but the install command fails -> surface
        // stderr, never report success.
        let http = FakeHttp::ok("7.5.0");
        let runner = FakeRunner::default()
            .with_manager("snap", "")
            .fail_upgrade(1, "snap: install failed");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Linux, true, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(!stdout.contains("installed successfully"));
        assert!(stderr.contains("install via snap failed"));
        assert!(stderr.contains("snap: install failed"));
    }

    // --- portable tar.gz path -------------------------------------------------

    /// A portable install tree `<td>/powershell/7.6.2/pwsh`; returns the guard,
    /// install dir, and binary path.
    fn portable_tree(old: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let td = tempfile::tempdir().unwrap();
        let install = td.path().join("powershell").join("7.6.2");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("pwsh"), old).unwrap();
        let binary = install.join("pwsh").to_string_lossy().into_owned();
        (td, install, binary)
    }

    #[test]
    fn update_portable_runs_in_process_never_spawning_self() {
        // Portable install detected, update available, but the fake HTTP has no
        // release-asset bytes -> the in-process portable path is attempted and
        // fails honestly at download. Crucially, NO `pwsh-autoupdate` process
        // is spawned (the plan's display command is not PATH-executed).
        let http = FakeHttp::ok("7.6.3");
        let (_td, _install, binary) = portable_tree("old");
        let runner = FakeRunner::pwsh("PowerShell 7.6.2").resolved_at(&binary);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_update_with(&http, &runner, Os::Linux, false, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        // FR-9: the reported command is the documented flag form.
        assert!(stdout.contains("Running: pwsh-autoupdate --replace-portable"));
        assert!(stderr.contains("failed to download"));
        assert!(!stdout.contains("updated successfully"));
        // No subprocess named pwsh-autoupdate was ever run.
        assert!(runner
            .runs
            .borrow()
            .iter()
            .all(|(p, _)| p != "pwsh-autoupdate"));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn replace_portable_happy_path_updates_and_exits_0() {
        use crate::adapters::portable::fixtures;
        use crate::core::portable as rules;

        let (_td, install, binary) = portable_tree("old-binary");
        let tarball = fixtures::payload_targz();
        let asset = "powershell-7.6.3-linux-x64.tar.gz";
        let http = FakeHttp::ok("7.6.3")
            .raw(
                &rules::hashes_url("v7.6.3"),
                fixtures::manifest_utf16le(&[(&fixtures::sha256_hex(&tarball), asset)]),
            )
            .raw(&rules::asset_url("v7.6.3", asset), tarball);
        // Probe sees 7.6.2 via `pwsh`; the post-swap verify runs the binary by
        // its absolute path, which reports the new version.
        let runner = FakeRunner::pwsh("PowerShell 7.6.2")
            .with_manager(&binary, "PowerShell 7.6.3")
            .resolved_at(&binary);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_replace_portable(&http, &runner, Os::Linux, &mut out, &mut err);
        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert_eq!(code, EXIT_SUCCESS, "stderr: {stderr}");
        assert!(stdout.contains("updated successfully to 7.6.3"));
        // The install dir now holds the new payload at the same path.
        assert!(std::fs::read_to_string(install.join("pwsh"))
            .unwrap()
            .contains("echo new"));
    }

    #[test]
    fn replace_portable_up_to_date_is_noop_exit_0() {
        let http = FakeHttp::ok("7.6.2"); // latest == installed
        let (_td, _install, binary) = portable_tree("old");
        let runner = FakeRunner::pwsh("PowerShell 7.6.2").resolved_at(&binary);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_replace_portable(&http, &runner, Os::Linux, &mut out, &mut err);
        assert_eq!(code, EXIT_SUCCESS);
        assert!(String::from_utf8(out).unwrap().contains("Nothing to do"));
    }

    #[test]
    fn replace_portable_refuses_a_manager_owned_install() {
        // dpkg owns pwsh -> --replace-portable must refuse (FR-6: never a
        // different channel than the owning one) and point at the update path.
        let http = FakeHttp::ok("7.6.3");
        let runner = FakeRunner::pwsh("PowerShell 7.6.2")
            .with_manager("dpkg", "Package: powershell\nStatus: install ok installed");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_replace_portable(&http, &runner, Os::Linux, &mut out, &mut err);
        let stderr = String::from_utf8(err).unwrap();
        assert_ne!(code, EXIT_SUCCESS);
        assert!(stderr.contains("not a portable tar.gz install"));
        assert!(stderr.contains("apt/dpkg"));
        assert!(mutating_runs(&runner).is_empty());
    }

    #[test]
    fn replace_portable_is_linux_only() {
        let http = FakeHttp::ok("7.6.3");
        let runner = FakeRunner::pwsh("PowerShell 7.6.2");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_replace_portable(&http, &runner, Os::Windows, &mut out, &mut err);
        assert_ne!(code, EXIT_SUCCESS);
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("Linux portable tar.gz installs only"));
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
