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
use crate::core::{detect, DetectionSignals, InstallMethod, Os};

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

    // Always enumerate the per-OS managers. When pwsh is present this fills the
    // ownership hints used to pick the *update* channel; when pwsh is ABSENT the
    // ownership queries simply report not-owned, but `available_managers` is
    // still populated so the host can pick a from-scratch *install* channel
    // (ADR-0006). Portable detection resolves the real binary path, which is
    // `None` when pwsh is absent, so no portable hint is set in that case.
    gather_ownership(runner, os, &mut signals);

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
/// the output is blank. Shared with the portable-replace adapter, which runs
/// the swapped-in binary to verify the update took.
pub(crate) fn extract_version_token(raw: &str) -> Option<String> {
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

/// Read-only Windows PowerShell 5.1 query (present on every supported Windows)
/// reporting the install technologies registered for PowerShell 7:
/// * `MSI <product-code>` per MSI uninstall registration (`WindowsInstaller=1`,
///   stable channel only — previews are a separate product);
/// * `MSIX <package-full-name>` per registered `Microsoft.PowerShell` AppX.
///
/// Uses only single quotes so it survives Windows argv quoting unchanged.
pub(crate) const WINDOWS_INSTALL_QUERY: &str = "$ErrorActionPreference='SilentlyContinue'; \
Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*','HKLM:\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*' \
| Where-Object { $_.DisplayName -like 'PowerShell 7*' -and $_.DisplayName -notlike '*preview*' -and $_.WindowsInstaller -eq 1 } \
| ForEach-Object { 'MSI ' + $_.PSChildName }; \
Get-AppxPackage -Name 'Microsoft.PowerShell' | ForEach-Object { 'MSIX ' + $_.PackageFullName }";

fn gather_windows(runner: &dyn CommandRunner, signals: &mut DetectionSignals) {
    let winget = note_manager(runner, "winget", InstallMethod::Winget, signals);
    if winget {
        // Weak signal: winget correlates ANY installed PowerShell with this id.
        signals.winget_lists_pwsh =
            manager_lists_pwsh(runner, "winget", &["list", "--id", "Microsoft.PowerShell"]);
    }

    // The install TECHNOLOGY of the pwsh actually on PATH decides the channel.
    if signals.pwsh_present {
        let query = runner
            .run(
                "powershell.exe",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    WINDOWS_INSTALL_QUERY,
                ],
            )
            .ok()
            .filter(|out| out.status == 0)
            .map(|out| detect::parse_windows_install_query(&out.stdout))
            .unwrap_or_default();

        // The resolved binary path is decisive when known: an MSIX package (or
        // its App Execution Alias) lives under `WindowsApps`. Without a path,
        // fall back to the AppX registration.
        let on_path_is_msix = match runner.resolve_program_path(pwsh_exe()) {
            Some(path) => detect::is_windowsapps_path(&path),
            None => query.appx_registered,
        };
        signals.msix_registered = on_path_is_msix;
        signals.msi_registered = !on_path_is_msix && query.msi_product_code.is_some();
        signals.msi_product_code = query.msi_product_code.filter(|_| signals.msi_registered);

        // For an MSI install, learn which technology winget's latest package
        // uses: 7.7.0+ is MSIX-only, which winget cannot upgrade an MSI to.
        if signals.msi_registered && winget {
            signals.winget_installer_type = runner
                .run(
                    "winget",
                    &[
                        "show",
                        "--id",
                        "Microsoft.PowerShell",
                        "--exact",
                        "--source",
                        "winget",
                        "--accept-source-agreements",
                    ],
                )
                .ok()
                .filter(|out| out.status == 0)
                .and_then(|out| detect::parse_winget_installer_type(&out.stdout));
        }
    }

    if note_manager(runner, "choco", InstallMethod::Chocolatey, signals) {
        // Chocolatey v1 needs `--local-only`; v2 removed that flag (it errors)
        // and lists local packages by default. `--limit-output` keeps v2 from
        // echoing anything when the package is absent.
        signals.choco_lists_pwsh = manager_lists_pwsh(
            runner,
            "choco",
            &["list", "--local-only", "powershell-core"],
        ) || manager_lists_pwsh(
            runner,
            "choco",
            &["list", "powershell-core", "--exact", "--limit-output"],
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
    // Portable tar.gz: pwsh owned by no native/snap manager above. Recognize
    // both a system-prefix install AND a user-local / manual extraction by
    // resolving where the real binary lives. The dir stat and path resolution
    // go through the runner seam (not `std` directly) so detection stays
    // hermetic — otherwise a real pwsh on the host (e.g. a CI runner) leaks
    // into fake-driven unit tests.
    if !signals.dpkg_owns_pwsh && !signals.rpm_owns_pwsh && !signals.snap_lists_pwsh {
        signals.portable_install_dir = portable_root_for(runner);
    }
}

/// Resolve the portable install root for pwsh, if any. Checks the system prefix
/// first (a root `/opt/microsoft/powershell` install), then resolves the real
/// binary path to recognize a user-local / manual extraction such as
/// `~/.local/share/powershell/<version>/pwsh`. Both go through the runner seam.
fn portable_root_for(runner: &dyn CommandRunner) -> Option<String> {
    const SYSTEM_PREFIX: &str = "/opt/microsoft/powershell";
    if runner.dir_exists(SYSTEM_PREFIX) {
        return Some(SYSTEM_PREFIX.to_string());
    }
    let resolved = runner.resolve_program_path(pwsh_exe())?;
    // The layout-recognition rule is pure and shared with the portable-replace
    // path (core::portable), so detection and replacement agree by construction
    // on what counts as a portable install.
    crate::core::portable::install_root_from_path(&resolved)
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
        dirs: HashSet<String>,
        resolved: Option<String>,
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
        /// Model a directory existing on the (fake) filesystem.
        fn dir(mut self, path: &str) -> Self {
            self.dirs.insert(path.to_string());
            self
        }
        /// Model the resolved (symlink-followed) path of the pwsh binary.
        fn resolved_path(mut self, path: &str) -> Self {
            self.resolved = Some(path.to_string());
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
        fn dir_exists(&self, path: &str) -> bool {
            self.dirs.contains(path)
        }
        fn resolve_program_path(&self, _program: &str) -> Option<String> {
            self.resolved.clone()
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
    fn windows_msi_install_listed_by_winget_is_detected_as_msi() {
        // Regression (0x8A15008E): an MSI-installed pwsh that winget lists, and
        // whose latest winget installer is MSIX, must resolve to Msi with the
        // product code + installer type captured — not to Winget.
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.6.9")
            .output("powershell.exe", 0, "MSI {5417B253-1A6D-4638-BC94-743A6EC2E6F4}\r\n")
            .output(
                "winget",
                0,
                "PowerShell 7.6.9.0-x64 Microsoft.PowerShell 7.6.9.0 7.7.0.0 winget\n  Installer Type: msix\n",
            )
            .resolved_path(r"\\?\C:\Program Files\PowerShell\7\pwsh.exe");
        let s = probe(&runner, Os::Windows);
        assert!(s.winget_lists_pwsh);
        assert!(s.msi_registered);
        assert!(!s.msix_registered);
        assert_eq!(
            s.msi_product_code.as_deref(),
            Some("{5417B253-1A6D-4638-BC94-743A6EC2E6F4}")
        );
        assert_eq!(s.winget_installer_type.as_deref(), Some("msix"));
        let d = crate::core::detect::resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Msi));
        assert!(crate::core::detect::msi_needs_msix_migration(
            d.selected, &s
        ));
    }

    #[test]
    fn windows_pwsh_under_windowsapps_is_msix_even_with_stale_msi_entry() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.7.0")
            .output(
                "powershell.exe",
                0,
                "MSI {5417B253-1A6D-4638-BC94-743A6EC2E6F4}\nMSIX Microsoft.PowerShell_7.7.0.0_x64__8wekyb3d8bbwe\n",
            )
            .resolved_path(
                r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7.7.0.0_x64__8wekyb3d8bbwe\pwsh.exe",
            );
        let s = probe(&runner, Os::Windows);
        assert!(s.msix_registered);
        assert!(!s.msi_registered);
        assert!(s.msi_product_code.is_none());
        let d = crate::core::detect::resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Msix));
    }

    #[test]
    fn windows_unresolvable_path_falls_back_to_appx_registration() {
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.7.0")
            .output(
                "powershell.exe",
                0,
                "MSIX Microsoft.PowerShell_7.7.0.0_x64__8wekyb3d8bbwe\n",
            );
        let s = probe(&runner, Os::Windows);
        assert!(s.msix_registered);
        assert!(!s.msi_registered);
    }

    #[test]
    fn linux_portable_prefix_detected_via_runner_seam() {
        // pwsh present, no manager owns it, but the portable prefix dir exists
        // (modelled on the fake) -> location hint recorded, method undetermined.
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.4.6")
            .dir("/opt/microsoft/powershell");
        let s = probe(&runner, Os::Linux);
        assert_eq!(
            s.portable_install_dir.as_deref(),
            Some("/opt/microsoft/powershell")
        );
        assert!(!s.dpkg_owns_pwsh && !s.rpm_owns_pwsh && !s.snap_lists_pwsh);
    }

    #[test]
    fn linux_portable_prefix_is_hermetic_when_dir_absent() {
        // No manager owns pwsh and the fake reports no portable dir / no resolved
        // path -> no host leakage even if the real machine has a portable pwsh.
        let exe = pwsh_exe();
        let runner = FakeRunner::default().output(exe, 0, "PowerShell 7.4.6");
        let s = probe(&runner, Os::Linux);
        assert!(s.portable_install_dir.is_none());
    }

    #[test]
    fn linux_user_local_portable_detected_from_resolved_path() {
        // pwsh present, no manager owns it, no system prefix dir — but the real
        // binary resolves under ~/.local/share/powershell/<ver>/pwsh. Detection
        // must recognize this as a portable install (the bug: it was reporting
        // "undetermined" for a fully-supported method).
        let exe = pwsh_exe();
        let runner = FakeRunner::default()
            .output(exe, 0, "PowerShell 7.6.2")
            .resolved_path("/home/u/.local/share/powershell/7.6.2/pwsh");
        let s = probe(&runner, Os::Linux);
        assert_eq!(
            s.portable_install_dir.as_deref(),
            Some("/home/u/.local/share/powershell")
        );
        // The pure rules then select PortableTarGz from these plain signals.
        let d = crate::core::detect::resolve(Os::Linux, &s);
        assert_eq!(d.selected, Some(InstallMethod::PortableTarGz));
    }

    // Portable-layout path recognition is a pure rule shared with the
    // portable-replace adapter; its unit tests live in `core::portable`.

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
