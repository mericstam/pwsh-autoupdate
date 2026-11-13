//! Per-OS update-plan / command builder (FR-6/9/12).
//!
//! Pure: maps a selected `(method, os)` plus the from/to versions to the exact
//! `program` + `args` and the `requires_elevation` flag. No execution happens
//! here — the command is built so it can be asserted in unit tests and so the
//! `--check` report shows exactly what the adapter would run (FR-9 agreement).
//!
//! For `.pkg` and portable tar.gz there is no single manager invocation; the
//! "command" is the OS tool that performs the replace step (`installer`, or a
//! download-and-replace step). The *asset URL* resolution stays in the adapter,
//! not here — core only fixes the program/args shape.

use crate::core::error::PlanError;
use crate::core::{
    InstallMethod, InstallPlan, MigrationPlan, Os, PlanCommand, UninstallPlan, UpdatePlan,
};
use semver::Version;

/// Resolve the NATIVE from-scratch install command for `os`, given which manager
/// executables are available on PATH (ADR-0006). Precedence per OS mirrors the
/// update detection order. Returns `None` when no native channel is available —
/// the host then falls back to the portable installer (delivered separately).
///
/// `apt`/`dnf` are intentionally excluded as auto-install channels: a from-zero
/// install through them requires configuring Microsoft's package repository
/// first (multi-step, root) — those hosts get the portable install instead.
pub fn resolve_install(os: Os, available: &[InstallMethod]) -> Option<InstallPlan> {
    let has = |m: InstallMethod| available.contains(&m);
    let plan = |method, program: &str, args: &[&str], requires_elevation| InstallPlan {
        method,
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        requires_elevation,
    };
    match os {
        Os::Windows => {
            if has(InstallMethod::Winget) {
                Some(plan(
                    InstallMethod::Winget,
                    "winget",
                    &[
                        "install",
                        "--id",
                        "Microsoft.PowerShell",
                        "--source",
                        "winget",
                        "--silent",
                        "--accept-source-agreements",
                        "--accept-package-agreements",
                    ],
                    false, // per-user winget scope
                ))
            } else if has(InstallMethod::Chocolatey) {
                Some(plan(
                    InstallMethod::Chocolatey,
                    "choco",
                    &["install", "powershell-core", "-y"],
                    true,
                ))
            } else {
                None
            }
        }
        Os::Macos => {
            if has(InstallMethod::Homebrew) {
                // The Homebrew *formula* (`brew install powershell`) is the
                // current method; the old `--cask powershell` is deprecated and
                // unavailable (Microsoft docs even tell you to uninstall it).
                Some(plan(
                    InstallMethod::Homebrew,
                    "brew",
                    &["install", "powershell"],
                    false,
                ))
            } else {
                None
            }
        }
        Os::Linux => {
            if has(InstallMethod::Snap) {
                Some(plan(
                    InstallMethod::Snap,
                    "snap",
                    &["install", "powershell", "--classic"],
                    true,
                ))
            } else {
                None
            }
        }
    }
}

/// Build the update plan for the selected method on the given OS.
///
/// Returns [`PlanError::UnsupportedCombination`] for a `(method, os)` pair that
/// has no defined upgrade procedure (e.g. a Windows method on Linux).
pub fn build_plan(
    method: InstallMethod,
    from: Version,
    to: Version,
    os: Os,
) -> Result<UpdatePlan, PlanError> {
    let (program, args, requires_elevation): (&str, &[&str], bool) = match (os, method) {
        // --- Windows ---------------------------------------------------------
        (Os::Windows, InstallMethod::Winget) => (
            "winget",
            &[
                "upgrade",
                "--id",
                "Microsoft.PowerShell",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
            false, // per-user winget scope
        ),
        (Os::Windows, InstallMethod::Msix) => (
            "winget",
            &[
                "upgrade",
                "--id",
                "Microsoft.PowerShell",
                "--source",
                "winget",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
            false,
        ),
        (Os::Windows, InstallMethod::Msi) => (
            "winget",
            &[
                "upgrade",
                "--id",
                "Microsoft.PowerShell",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
            true, // MSI writes machine-scope system paths
        ),
        (Os::Windows, InstallMethod::Chocolatey) => {
            ("choco", &["upgrade", "powershell-core", "-y"], true)
        }

        // --- macOS -----------------------------------------------------------
        (Os::Macos, InstallMethod::Homebrew) => ("brew", &["upgrade", "powershell"], false),
        (Os::Macos, InstallMethod::MacPkg) => (
            // Fixed procedure: the adapter fetches the latest .pkg asset, then
            // installs it system-wide. `installer -target /` requires root.
            "installer",
            &["-pkg", "PowerShell.pkg", "-target", "/"],
            true,
        ),

        // --- Linux -----------------------------------------------------------
        (Os::Linux, InstallMethod::AptDpkg) => (
            "apt-get",
            &["install", "--only-upgrade", "-y", "powershell"],
            true,
        ),
        (Os::Linux, InstallMethod::DnfRpm) => ("dnf", &["upgrade", "-y", "powershell"], true),
        (Os::Linux, InstallMethod::Snap) => ("snap", &["refresh", "powershell"], true),
        (Os::Linux, InstallMethod::PortableTarGz) => (
            // Fixed procedure: download the latest tar.gz asset, verify its
            // SHA-256, and atomically replace the portable install dir. The
            // command names the documented `--replace-portable` flag (FR-9:
            // running it manually does exactly this), but the update path
            // executes the same routine IN-PROCESS — it never PATH-spawns
            // itself. Elevation depends on who owns the install dir; treat as
            // not-required by default (a user-owned ~/powershell needs none) —
            // the adapter surfaces the FR-12 elevation error when the dir is
            // not writable.
            "pwsh-autoupdate",
            &["--replace-portable"],
            false,
        ),

        // --- Cross-OS mismatches are unsupported ----------------------------
        (os, method) => {
            return Err(PlanError::UnsupportedCombination {
                method: format!("{method:?}"),
                os: format!("{os:?}"),
            })
        }
    };

    // Follow-up steps that run only after the primary command succeeds.
    // Homebrew's `powershell` formula frequently fails to relink `pwsh` after
    // `brew upgrade` (a file already exists in the prefix -> brew leaves the keg
    // unlinked and prints "run brew link --overwrite"). We fold that remediation
    // into the plan so every upgrade is self-healing; the step is idempotent
    // (a clean relink when nothing was wrong).
    let post_steps: Vec<PlanCommand> = match (os, method) {
        (Os::Macos, InstallMethod::Homebrew) => vec![PlanCommand {
            program: "brew".to_string(),
            args: ["link", "--overwrite", "powershell"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }],
        _ => Vec::new(),
    };

    Ok(UpdatePlan {
        method,
        from,
        to,
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        requires_elevation,
        post_steps,
    })
}

/// Build the MSI -> MSIX migration plan (opt-in `--migrate`).
///
/// Step 1 removes the MSI by its registered product code; step 2 installs the
/// winget-source `Microsoft.PowerShell` package, which is MSIX from 7.7.0 on.
/// The product code is re-validated here (it is passed to `msiexec`), so a
/// malformed value can never reach the command line.
pub fn build_msi_to_msix_migration(
    msi_product_code: &str,
    from: Version,
    to: Version,
) -> Result<MigrationPlan, PlanError> {
    if !crate::core::detect::is_msi_product_code(msi_product_code) {
        return Err(PlanError::InvalidMsiProductCode {
            code: msi_product_code.to_string(),
        });
    }
    let cmd = |program: &str, args: &[&str]| PlanCommand {
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    };
    Ok(MigrationPlan {
        from,
        to,
        uninstall: cmd("msiexec", &["/x", msi_product_code, "/qn", "/norestart"]),
        install: cmd(
            "winget",
            &[
                "install",
                "--id",
                "Microsoft.PowerShell",
                "--exact",
                "--source",
                "winget",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
        ),
        requires_elevation: true,
    })
}

/// Build the uninstall plan for the selected method on the given OS (ADR-0007).
///
/// The same command table discipline as [`build_plan`]: the exact `program` +
/// `args` are fixed here so unit tests assert them and the preview shows
/// exactly what would run (FR-9 agreement). Returns
/// [`PlanError::UnsupportedCombination`] for a `(method, os)` pair with no
/// defined removal procedure.
pub fn build_uninstall_plan(method: InstallMethod, os: Os) -> Result<UninstallPlan, PlanError> {
    // NOTE: `winget uninstall` accepts `--accept-source-agreements` but NOT
    // `--accept-package-agreements` (an install/upgrade-only flag) — do not
    // copy the upgrade rows' flag set verbatim.
    let (program, args, requires_elevation): (&str, &[&str], bool) = match (os, method) {
        // --- Windows ---------------------------------------------------------
        (Os::Windows, InstallMethod::Winget) => (
            "winget",
            &[
                "uninstall",
                "--id",
                "Microsoft.PowerShell",
                "--silent",
                "--accept-source-agreements",
            ],
            false, // per-user winget scope
        ),
        (Os::Windows, InstallMethod::Msix) => (
            "winget",
            &[
                "uninstall",
                "--id",
                "Microsoft.PowerShell",
                "--silent",
                "--accept-source-agreements",
            ],
            false,
        ),
        (Os::Windows, InstallMethod::Msi) => (
            "winget",
            &[
                "uninstall",
                "--id",
                "Microsoft.PowerShell",
                "--silent",
                "--accept-source-agreements",
            ],
            true, // MSI removal touches machine-scope system paths
        ),
        (Os::Windows, InstallMethod::Chocolatey) => {
            ("choco", &["uninstall", "powershell-core", "-y"], true)
        }

        // --- macOS -----------------------------------------------------------
        (Os::Macos, InstallMethod::Homebrew) => ("brew", &["uninstall", "powershell"], false),
        (Os::Macos, InstallMethod::MacPkg) => (
            // Fixed procedure: no package manager exists for a `.pkg` install.
            // Microsoft's documented removal is deleting these two fixed paths;
            // the receipt cleanup (`pkgutil --forget`) is a post-step.
            "rm",
            &[
                "-rf",
                "/usr/local/microsoft/powershell",
                "/usr/local/bin/pwsh",
            ],
            true,
        ),

        // --- Linux -----------------------------------------------------------
        (Os::Linux, InstallMethod::AptDpkg) => {
            // `remove`, not `purge`: keep system config under /etc recoverable.
            ("apt-get", &["remove", "-y", "powershell"], true)
        }
        (Os::Linux, InstallMethod::DnfRpm) => ("dnf", &["remove", "-y", "powershell"], true),
        (Os::Linux, InstallMethod::Snap) => ("snap", &["remove", "powershell"], true),
        (Os::Linux, InstallMethod::PortableTarGz) => (
            // Fixed procedure: delete the layout-gated portable install dir.
            // The command names the documented flag form (FR-9: running it
            // manually does exactly this), but the uninstall path executes the
            // same routine IN-PROCESS — it never PATH-spawns itself. Privilege
            // needs depend on who owns the install dir; the adapter surfaces
            // the FR-12 elevation error when the dir is not writable.
            "pwsh-autoupdate",
            &["--uninstall", "--yes"],
            false,
        ),

        // --- Cross-OS mismatches are unsupported ----------------------------
        (os, method) => {
            return Err(PlanError::UnsupportedCombination {
                method: format!("{method:?}"),
                os: format!("{os:?}"),
            })
        }
    };

    // Receipt cleanup after the .pkg payload is removed: cosmetic (the files
    // are already gone), so it is a best-effort post-step, not part of the
    // primary command.
    let post_steps: Vec<PlanCommand> = match (os, method) {
        (Os::Macos, InstallMethod::MacPkg) => vec![PlanCommand {
            program: "pkgutil".to_string(),
            args: ["--forget", "com.microsoft.powershell"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }],
        _ => Vec::new(),
    };

    Ok(UninstallPlan {
        method,
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        requires_elevation,
        post_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn plan(method: InstallMethod, os: Os) -> UpdatePlan {
        build_plan(method, v("7.4.0"), v("7.5.0"), os).unwrap()
    }

    #[test]
    fn resolve_install_windows_prefers_winget_over_choco() {
        let p = resolve_install(
            Os::Windows,
            &[InstallMethod::Chocolatey, InstallMethod::Winget],
        )
        .unwrap();
        assert_eq!(p.method, InstallMethod::Winget);
        assert_eq!(p.program, "winget");
        assert_eq!(p.args.first().map(String::as_str), Some("install"));
        assert!(p.args.iter().any(|a| a == "Microsoft.PowerShell"));
        assert!(!p.requires_elevation);
    }

    #[test]
    fn resolve_install_windows_falls_back_to_choco() {
        let p = resolve_install(Os::Windows, &[InstallMethod::Chocolatey]).unwrap();
        assert_eq!(p.method, InstallMethod::Chocolatey);
        assert_eq!(p.program, "choco");
        assert!(p.args.iter().any(|a| a == "install"));
        assert!(p.requires_elevation);
    }

    #[test]
    fn resolve_install_macos_uses_homebrew_formula() {
        let p = resolve_install(Os::Macos, &[InstallMethod::Homebrew]).unwrap();
        assert_eq!(p.program, "brew");
        // The formula, not the deprecated `--cask powershell`.
        assert_eq!(p.args, vec!["install", "powershell"]);
        assert!(!p.requires_elevation);
    }

    #[test]
    fn resolve_install_linux_uses_snap_classic_with_elevation() {
        let p = resolve_install(Os::Linux, &[InstallMethod::Snap]).unwrap();
        assert_eq!(p.program, "snap");
        assert_eq!(p.args, vec!["install", "powershell", "--classic"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn resolve_install_none_when_no_native_channel() {
        // apt/dpkg present but not a supported auto-install channel -> None
        // (host falls back to the portable installer).
        assert!(resolve_install(Os::Linux, &[InstallMethod::AptDpkg]).is_none());
        assert!(resolve_install(Os::Linux, &[]).is_none());
        assert!(resolve_install(Os::Macos, &[]).is_none());
        assert!(resolve_install(Os::Windows, &[]).is_none());
    }

    #[test]
    fn winget_plan() {
        let p = plan(InstallMethod::Winget, Os::Windows);
        assert_eq!(p.program, "winget");
        assert_eq!(
            p.args,
            vec![
                "upgrade",
                "--id",
                "Microsoft.PowerShell",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements"
            ]
        );
        assert!(!p.requires_elevation);
        assert_eq!(p.from, v("7.4.0"));
        assert_eq!(p.to, v("7.5.0"));
    }

    #[test]
    fn msix_plan_routes_through_winget_source_not_msstore() {
        // The winget-source Microsoft.PowerShell package IS the MSIX since
        // 7.7.0; the msstore source has no package with that id.
        let p = plan(InstallMethod::Msix, Os::Windows);
        assert_eq!(p.program, "winget");
        assert!(p.args.windows(2).any(|w| w == ["--source", "winget"]));
        assert!(!p.args.iter().any(|a| a == "msstore"));
        assert!(!p.requires_elevation);
    }

    #[test]
    fn msi_plan_requires_elevation() {
        let p = plan(InstallMethod::Msi, Os::Windows);
        assert_eq!(p.program, "winget");
        assert!(p.requires_elevation);
    }

    #[test]
    fn choco_plan() {
        let p = plan(InstallMethod::Chocolatey, Os::Windows);
        assert_eq!(p.program, "choco");
        assert_eq!(p.args, vec!["upgrade", "powershell-core", "-y"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn brew_plan() {
        let p = plan(InstallMethod::Homebrew, Os::Macos);
        assert_eq!(p.program, "brew");
        assert_eq!(p.args, vec!["upgrade", "powershell"]);
        assert!(!p.requires_elevation);
    }

    #[test]
    fn brew_plan_relinks_powershell_after_upgrade() {
        // `brew upgrade powershell` regularly leaves `pwsh` unlinked; the plan
        // carries a follow-up `brew link --overwrite powershell` so the upgrade
        // is self-healing (users no longer relink by hand).
        let p = plan(InstallMethod::Homebrew, Os::Macos);
        assert_eq!(p.post_steps.len(), 1);
        assert_eq!(p.post_steps[0].program, "brew");
        assert_eq!(
            p.post_steps[0].args,
            vec!["link", "--overwrite", "powershell"]
        );
    }

    #[test]
    fn non_homebrew_plans_have_no_post_steps() {
        // The relink is Homebrew-specific; no other channel should carry one.
        assert!(plan(InstallMethod::Winget, Os::Windows)
            .post_steps
            .is_empty());
        assert!(plan(InstallMethod::Chocolatey, Os::Windows)
            .post_steps
            .is_empty());
        assert!(plan(InstallMethod::MacPkg, Os::Macos).post_steps.is_empty());
        assert!(plan(InstallMethod::AptDpkg, Os::Linux)
            .post_steps
            .is_empty());
        assert!(plan(InstallMethod::Snap, Os::Linux).post_steps.is_empty());
    }

    #[test]
    fn macpkg_plan_uses_installer_and_requires_elevation() {
        let p = plan(InstallMethod::MacPkg, Os::Macos);
        assert_eq!(p.program, "installer");
        assert!(p.args.contains(&"-target".to_string()));
        assert!(p.args.contains(&"/".to_string()));
        assert!(p.requires_elevation);
    }

    #[test]
    fn apt_plan() {
        let p = plan(InstallMethod::AptDpkg, Os::Linux);
        assert_eq!(p.program, "apt-get");
        assert_eq!(
            p.args,
            vec!["install", "--only-upgrade", "-y", "powershell"]
        );
        assert!(p.requires_elevation);
    }

    #[test]
    fn dnf_plan() {
        let p = plan(InstallMethod::DnfRpm, Os::Linux);
        assert_eq!(p.program, "dnf");
        assert_eq!(p.args, vec!["upgrade", "-y", "powershell"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn snap_plan() {
        let p = plan(InstallMethod::Snap, Os::Linux);
        assert_eq!(p.program, "snap");
        assert_eq!(p.args, vec!["refresh", "powershell"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn portable_targz_plan() {
        let p = plan(InstallMethod::PortableTarGz, Os::Linux);
        // Modeled as a self-invoked download-and-replace step.
        assert_eq!(p.program, "pwsh-autoupdate");
        assert!(p.args.iter().any(|a| a == "--replace-portable"));
    }

    #[test]
    fn cross_os_mismatch_is_unsupported() {
        let err = build_plan(InstallMethod::Winget, v("7.4.0"), v("7.5.0"), Os::Linux).unwrap_err();
        match err {
            PlanError::UnsupportedCombination { method, os } => {
                assert_eq!(method, "Winget");
                assert_eq!(os, "Linux");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // --- uninstall plans (ADR-0007) ------------------------------------------

    fn uplan(method: InstallMethod, os: Os) -> UninstallPlan {
        build_uninstall_plan(method, os).unwrap()
    }

    #[test]
    fn winget_uninstall_plan() {
        let p = uplan(InstallMethod::Winget, Os::Windows);
        assert_eq!(p.program, "winget");
        assert_eq!(
            p.args,
            vec![
                "uninstall",
                "--id",
                "Microsoft.PowerShell",
                "--silent",
                "--accept-source-agreements"
            ]
        );
        assert!(!p.requires_elevation);
    }

    #[test]
    fn msix_uninstall_plan_does_not_use_msstore() {
        let p = uplan(InstallMethod::Msix, Os::Windows);
        assert_eq!(p.program, "winget");
        assert!(!p.args.iter().any(|a| a == "msstore"));
        assert!(!p.requires_elevation);
    }

    // --- MSI -> MSIX migration ------------------------------------------------

    const CODE: &str = "{5417B253-1A6D-4638-BC94-743A6EC2E6F4}";

    #[test]
    fn msi_migration_removes_msi_then_installs_msix() {
        let m = build_msi_to_msix_migration(CODE, v("7.6.9"), v("7.7.0")).unwrap();
        assert_eq!(m.uninstall.program, "msiexec");
        assert_eq!(m.uninstall.args, vec!["/x", CODE, "/qn", "/norestart"]);
        assert_eq!(m.install.program, "winget");
        assert_eq!(
            m.install.args,
            vec![
                "install",
                "--id",
                "Microsoft.PowerShell",
                "--exact",
                "--source",
                "winget",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements"
            ]
        );
        assert!(m.requires_elevation);
        assert_eq!(m.from, v("7.6.9"));
        assert_eq!(m.to, v("7.7.0"));
    }

    #[test]
    fn msi_migration_rejects_malformed_product_code() {
        assert!(build_msi_to_msix_migration("{nope}", v("7.6.9"), v("7.7.0")).is_err());
        assert!(build_msi_to_msix_migration("", v("7.6.9"), v("7.7.0")).is_err());
    }

    #[test]
    fn msi_uninstall_plan_requires_elevation() {
        let p = uplan(InstallMethod::Msi, Os::Windows);
        assert_eq!(p.program, "winget");
        assert!(p.requires_elevation);
    }

    #[test]
    fn winget_uninstall_plans_omit_the_package_agreements_flag() {
        // `winget uninstall` rejects `--accept-package-agreements` (that flag is
        // install/upgrade-only); passing it makes every uninstall fail.
        for method in [
            InstallMethod::Winget,
            InstallMethod::Msix,
            InstallMethod::Msi,
        ] {
            let p = uplan(method, Os::Windows);
            assert!(
                !p.args.iter().any(|a| a == "--accept-package-agreements"),
                "{method:?} uninstall must not carry --accept-package-agreements"
            );
        }
    }

    #[test]
    fn choco_uninstall_plan() {
        let p = uplan(InstallMethod::Chocolatey, Os::Windows);
        assert_eq!(p.program, "choco");
        assert_eq!(p.args, vec!["uninstall", "powershell-core", "-y"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn brew_uninstall_plan() {
        let p = uplan(InstallMethod::Homebrew, Os::Macos);
        assert_eq!(p.program, "brew");
        assert_eq!(p.args, vec!["uninstall", "powershell"]);
        assert!(!p.requires_elevation);
    }

    #[test]
    fn macpkg_uninstall_removes_documented_paths_and_forgets_receipt() {
        // No manager exists for a .pkg install; the plan encodes Microsoft's
        // documented removal (fixed paths) plus receipt cleanup as a post-step.
        let p = uplan(InstallMethod::MacPkg, Os::Macos);
        assert_eq!(p.program, "rm");
        assert_eq!(
            p.args,
            vec![
                "-rf",
                "/usr/local/microsoft/powershell",
                "/usr/local/bin/pwsh"
            ]
        );
        assert!(p.requires_elevation);
        assert_eq!(p.post_steps.len(), 1);
        assert_eq!(p.post_steps[0].program, "pkgutil");
        assert_eq!(
            p.post_steps[0].args,
            vec!["--forget", "com.microsoft.powershell"]
        );
    }

    #[test]
    fn apt_uninstall_plan_removes_not_purges() {
        let p = uplan(InstallMethod::AptDpkg, Os::Linux);
        assert_eq!(p.program, "apt-get");
        // `remove` keeps /etc config; `purge` is deliberately not used.
        assert_eq!(p.args, vec!["remove", "-y", "powershell"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn dnf_uninstall_plan() {
        let p = uplan(InstallMethod::DnfRpm, Os::Linux);
        assert_eq!(p.program, "dnf");
        assert_eq!(p.args, vec!["remove", "-y", "powershell"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn snap_uninstall_plan() {
        let p = uplan(InstallMethod::Snap, Os::Linux);
        assert_eq!(p.program, "snap");
        assert_eq!(p.args, vec!["remove", "powershell"]);
        assert!(p.requires_elevation);
    }

    #[test]
    fn portable_targz_uninstall_plan() {
        let p = uplan(InstallMethod::PortableTarGz, Os::Linux);
        // Modeled as the documented self-invoked flag form; executed in-process.
        assert_eq!(p.program, "pwsh-autoupdate");
        assert_eq!(p.args, vec!["--uninstall", "--yes"]);
        assert!(!p.requires_elevation);
    }

    #[test]
    fn non_macpkg_uninstall_plans_have_no_post_steps() {
        assert!(uplan(InstallMethod::Winget, Os::Windows)
            .post_steps
            .is_empty());
        assert!(uplan(InstallMethod::Homebrew, Os::Macos)
            .post_steps
            .is_empty());
        assert!(uplan(InstallMethod::AptDpkg, Os::Linux)
            .post_steps
            .is_empty());
        assert!(uplan(InstallMethod::Snap, Os::Linux).post_steps.is_empty());
        assert!(uplan(InstallMethod::PortableTarGz, Os::Linux)
            .post_steps
            .is_empty());
    }

    #[test]
    fn uninstall_cross_os_mismatch_is_unsupported() {
        let err = build_uninstall_plan(InstallMethod::Homebrew, Os::Windows).unwrap_err();
        match err {
            PlanError::UnsupportedCombination { method, os } => {
                assert_eq!(method, "Homebrew");
                assert_eq!(os, "Windows");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
