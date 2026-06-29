//! OS / filesystem probe seam (FR-3/4/5, Section 7).
//!
//! Gathers the plain [`DetectionSignals`] that the pure `core::detect` rules
//! consume — locating `pwsh`, reading its installed version, and collecting the
//! per-OS package-ownership hints. All environment / process / filesystem
//! access lives here (behind the [`CommandRunner`] seam + `cfg`-gated helpers);
//! the emitted signals are OS-agnostic plain values, so the detection rules
//! stay pure and unit-test identically on every platform.
//!
//! Cross-platform by construction: the PowerShell executable is `pwsh.exe` on
//! Windows and `pwsh` elsewhere (chosen via [`pwsh_exe`] / `cfg`), never
//! hardcoded to one platform. Ownership probing is gated behind
//! `cfg(target_os = ...)` so each OS only runs its own managers.

use crate::adapters::runner::CommandRunner;
use crate::core::{DetectionSignals, InstallMethod, Os};

/// The PowerShell executable name for the current platform.
///
/// Windows ships `pwsh.exe`; macOS/Linux ship `pwsh`. Selected by `cfg` so the
/// spawn target is correct by construction (the live-smoke gate only runs on
/// the build OS, so this must be right structurally for the other platforms).
pub fn pwsh_exe() -> &'static str {
    if cfg!(windows) {
        "pwsh.exe"
    } else {
        "pwsh"
    }
}

/// Probe the host into the plain [`DetectionSignals`] consumed by `core::detect`.
///
/// Steps (all behind the runner/`cfg` seam):
/// 1. Is `pwsh` on PATH? If not, `pwsh_present = false` and the host can exit
///    non-zero without installing (Section 7 — no auto-install).
/// 2. Read the installed version via `pwsh --version` and extract the semver
///    token into `current_version` (parsed later by `core::version`).
/// 3. Gather per-OS ownership hints + the set of available managers.
///
/// `os` is supplied by the host so the same function compiles and the emitted
/// signals are OS-agnostic.
pub fn probe(runner: &dyn CommandRunner, os: Os) -> DetectionSignals {
    let exe = pwsh_exe();
    let pwsh_present = runner.exists(exe);

    let (pwsh_path, current_version) = if pwsh_present {
        let version = read_pwsh_version(runner, exe);
        // We know pwsh is on PATH; record the resolved exe name as a hint. The
        // host's full path resolution is not required for detection.
        (Some(exe.to_string()), version)
    } else {
        (None, None)
    };

    let mut signals = DetectionSignals {
        pwsh_present,
        pwsh_path,
        current_version,
        ..Default::default()
    };

    // Ownership probing only makes sense when pwsh is installed.
    if pwsh_present {
        gather_ownership(runner, os, &mut signals);
    }

    signals
}

/// Run `pwsh --version` and extract the semver token (e.g. from
/// `"PowerShell 7.4.6"` → `"7.4.6"`). Returns `None` on a non-zero exit, an IO
/// failure, or empty output — never fabricates a version.
fn read_pwsh_version(runner: &dyn CommandRunner, exe: &str) -> Option<String> {
    let out = runner.run(exe, &["--version"]).ok()?;
    if out.status != 0 {
        return None;
    }
    extract_version_token(&out.stdout)
}

/// Extract the last whitespace-delimited token from a `pwsh --version` line
/// (the version sits after the `PowerShell` product word). Returns `None` if
/// the output is blank.
fn extract_version_token(raw: &str) -> Option<String> {
    let token = raw.split_whitespace().next_back()?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// Gather per-OS package-ownership hints + the available-managers set.
///
/// Each OS branch is `cfg`-independent at the rule level (it only asks the
/// runner which managers exist and queries ownership), but is dispatched by the
/// runtime `os` value so detection unit-tests can drive every OS with a fake.
fn gather_ownership(runner: &dyn CommandRunner, os: Os, signals: &mut DetectionSignals) {
    match os {
        Os::Windows => gather_windows(runner, signals),
        Os::Macos => gather_macos(runner, signals),
        Os::Linux => gather_linux(runner, signals),
    }
}

/// Record a manager as available if its executable is on PATH.
fn note_manager(
    runner: &dyn CommandRunner,
    program: &str,
    method: InstallMethod,
    signals: &mut DetectionSignals,
) -> bool {
    let present = runner.exists(program);
    if present {
        signals.available_managers.push(method);
    }
    present
}

/// True iff the command runs and its stdout (case-insensitively) mentions
/// PowerShell — a generic "does this manager own pwsh?" ownership query.
fn manager_lists_pwsh(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> bool {
    match runner.run(program, args) {
        Ok(out) if out.status == 0 => {
            let hay = out.stdout.to_ascii_lowercase();
            hay.contains("powershell") || hay.contains("pwsh")
        }
        _ => false,
    }
}

fn gather_windows(runner: &dyn CommandRunner, signals: &mut DetectionSignals) {
    if note_manager(runner, "winget", InstallMethod::Winget, signals) {
        signals.winget_lists_pwsh =
            manager_lists_pwsh(runner, "winget", &["list", "--id", "Microsoft.PowerShell"]);
    }
    // MSIX / MSI registration are read via PowerShell/registry queries; modelled
    // here as ownership queries that the host can refine. Default false until a
    // positive signal is seen.
    if note_manager(runner, "choco", InstallMethod::Chocolatey, signals) {
        signals.choco_lists_pwsh = manager_lists_pwsh(
            runner,
            "choco",
            &["list", "--local-only", "powershell-core"],
        );
    }
}

fn gather_macos(runner: &dyn CommandRunner, signals: &mut DetectionSignals) {
    if note_manager(runner, "brew", InstallMethod::Homebrew, signals) {
        signals.brew_lists_pwsh = manager_lists_pwsh(runner, "brew", &["list", "powershell"]);
    }
    // A `.pkg` receipt is read via `pkgutil --pkgs`; modelled as an ownership
    // query.
    if runner.exists("pkgutil") {
        signals.pkg_receipt_present =
            manager_lists_pwsh(runner, "pkgutil", &["--pkgs=com.microsoft.powershell"]);
        if signals.pkg_receipt_present {
            signals.available_managers.push(InstallMethod::MacPkg);
        }
    }
}

fn gather_linux(runner: &dyn CommandRunner, signals: &mut DetectionSignals) {
    if note_manager(runner, "dpkg", InstallMethod::AptDpkg, signals) {
        signals.dpkg_owns_pwsh = manager_lists_pwsh(runner, "dpkg", &["-s", "powershell"]);
    }
    if note_manager(runner, "rpm", InstallMethod::DnfRpm, signals) {
        signals.rpm_owns_pwsh = manager_lists_pwsh(runner, "rpm", &["-q", "powershell"]);
    }
    if note_manager(runner, "snap", InstallMethod::Snap, signals) {
        signals.snap_lists_pwsh = manager_lists_pwsh(runner, "snap", &["list", "powershell"]);
    }
    // Portable tar.gz: pwsh living under a non-package-managed prefix and owned
    // by no manager above. If pwsh is present but no native/snap manager claimed
    // it, treat the common portable install prefix as the location hint.
    if !signals.dpkg_owns_pwsh && !signals.rpm_owns_pwsh && !signals.snap_lists_pwsh {
        const PORTABLE_PREFIX: &str = "/opt/microsoft/powershell";
        if std::path::Path::new(PORTABLE_PREFIX).is_dir() {
            signals.portable_install_dir = Some(PORTABLE_PREFIX.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::runner::CmdOutput;
    use std::collections::{HashMap, HashSet};

    /// Fake runner: programs in `present` "exist"; `outputs` maps a program to a
    /// canned `CmdOutput`. Records nothing (probe is read-only).
    #[derive(Default)]
    struct FakeRunner {
        present: HashSet<String>,
        outputs: HashMap<String, CmdOutput>,
    }

    impl FakeRunner {
        fn present(mut self, program: &str) -> Self {
            self.present.insert(program.to_string());
            self
        }
        fn output(mut self, program: &str, status: i32, stdout: &str) -> Self {
            self.present.insert(program.to_string());
            self.outputs.insert(
                program.to_string(),
                CmdOutput {
                    status,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                },
            );
            self
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, _args: &[&str]) -> std::io::Result<CmdOutput> {
            self.outputs.get(program).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, format!("no fake: {program}"))
            })
        }
        fn exists(&self, program: &str) -> bool {
            self.present.contains(program)
        }
    }

    #[test]
    fn pwsh_exe_is_platform_correct() {
        if cfg!(windows) {
            assert_eq!(pwsh_exe(), "pwsh.exe");
        } else {
            assert_eq!(pwsh_exe(), "pwsh");
        }
    }

    #[test]
    fn extracts_version_token_from_product_line() {
        assert_eq!(
            extract_version_token("PowerShell 7.4.6"),
            Some("7.4.6".to_string())
        );
        assert_eq!(
            extract_version_token("  7.5.0-preview.1\n"),
            Some("7.5.0-preview.1".to_string())
        );
        assert_eq!(extract_version_token("   "), None);
    }

    #[test]
    fn pwsh_absent_is_reported_without_installing() {
        let runner = FakeRunner::default(); // pwsh not present
        let s = probe(&runner, Os::Linux);
        assert!(!s.pwsh_present);
        assert!(s.pwsh_path.is_none());
        assert!(s.current_version.is_none());
        assert!(s.available_managers.is_empty());
    }

    #[test]
    fn reads_version_and_present_flag_when_pwsh_installed() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default().output(exe, 0, "PowerShell 7.4.6");
        let s = probe(&runner, Os::Linux);
        assert!(s.pwsh_present);
        assert_eq!(s.current_version.as_deref(), Some("7.4.6"));
        assert_eq!(s.pwsh_path.as_deref(), Some(exe));
    }

    #[test]
    fn nonzero_version_exit_yields_no_version() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default().output(exe, 1, "");
        let s = probe(&runner, Os::Linux);
        assert!(s.pwsh_present);
        assert!(s.current_version.is_none());
    }

    #[test]
    fn linux_dpkg_ownership_signal_built() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.4.6")
            .output(
                "dpkg",
                0,
                "Package: powershell\nStatus: install ok installed",
            );
        let s = probe(&runner, Os::Linux);
        assert!(s.dpkg_owns_pwsh);
        assert!(s.available_managers.contains(&InstallMethod::AptDpkg));
        // The pure rules then select AptDpkg from these plain signals.
        let d = crate::core::detect::resolve(Os::Linux, &s);
        assert_eq!(d.selected, Some(InstallMethod::AptDpkg));
    }

    #[test]
    fn linux_snap_ownership_signal_built() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.4.6")
            .output("snap", 0, "Name      Version\npowershell  7.4.6");
        let s = probe(&runner, Os::Linux);
        assert!(s.snap_lists_pwsh);
        assert!(s.available_managers.contains(&InstallMethod::Snap));
    }

    #[test]
    fn manager_present_but_not_owning_pwsh_sets_available_not_ownership() {
        let exe = pwsh_exe();
        // dpkg exists but does not report powershell installed.
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.4.6")
            .present("dpkg");
        let s = probe(&runner, Os::Linux);
        assert!(s.available_managers.contains(&InstallMethod::AptDpkg));
        assert!(!s.dpkg_owns_pwsh);
    }

    #[test]
    fn windows_winget_ownership_signal_built() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.4.6")
            .output("winget", 0, "Microsoft.PowerShell 7.4.6");
        let s = probe(&runner, Os::Windows);
        assert!(s.winget_lists_pwsh);
        assert!(s.available_managers.contains(&InstallMethod::Winget));
    }

    #[test]
    fn macos_homebrew_ownership_signal_built() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.4.6")
            .output("brew", 0, "powershell");
        let s = probe(&runner, Os::Macos);
        assert!(s.brew_lists_pwsh);
        assert!(s.available_managers.contains(&InstallMethod::Homebrew));
    }
}
