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
use crate::core::{InstallMethod, Os, UpdatePlan};
use semver::Version;

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
                "msstore",
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
            // Fixed procedure: download the latest tar.gz asset and replace the
            // contents of the portable install dir. Elevation depends on who
            // owns that dir; treat as not-required by default (a user-owned
            // ~/powershell needs none) — the host surfaces a permission error
            // if the replace fails on a root-owned dir.
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

    Ok(UpdatePlan {
        method,
        from,
        to,
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        requires_elevation,
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
    fn msix_plan_routes_through_msstore() {
        let p = plan(InstallMethod::Msix, Os::Windows);
        assert_eq!(p.program, "winget");
        assert!(p.args.iter().any(|a| a == "msstore"));
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
        }
    }
}
