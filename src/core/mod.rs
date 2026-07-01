//! Pure domain core — no IO.
//!
//! Boundary invariant (enforced): no file under `src/core/` may import `ureq`,
//! `std::process::Command`, `std::env`, or perform any network / subprocess /
//! environment probing on its public surface. The core receives already-probed
//! / already-fetched plain values and returns plain values. This is what makes
//! every rule unit-testable with zero network and zero subprocesses.

pub mod detect;
pub mod error;
pub mod plan;
pub mod report;
pub mod version;

/// Which OS we are running on. Resolved by the host and passed into pure code
/// as data, so detection/plan rules unit-test identically on every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

/// Every supported install channel across all OSes (FR-3/4/5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallMethod {
    // Windows
    Winget,
    Msix,
    Msi,
    Chocolatey,
    // macOS
    Homebrew,
    MacPkg,
    // Linux
    AptDpkg,
    DnfRpm,
    Snap,
    PortableTarGz,
}

impl InstallMethod {
    /// Stable human-readable label used in reports.
    pub fn label(self) -> &'static str {
        match self {
            InstallMethod::Winget => "winget",
            InstallMethod::Msix => "MSIX/Microsoft Store",
            InstallMethod::Msi => "MSI",
            InstallMethod::Chocolatey => "Chocolatey",
            InstallMethod::Homebrew => "Homebrew",
            InstallMethod::MacPkg => "macOS .pkg",
            InstallMethod::AptDpkg => "apt/dpkg",
            InstallMethod::DnfRpm => "dnf/rpm",
            InstallMethod::Snap => "snap",
            InstallMethod::PortableTarGz => "portable tar.gz",
        }
    }
}

/// Plain signals gathered by the probe adapter; the pure detect rules consume
/// ONLY this struct (no environment access).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectionSignals {
    pub pwsh_present: bool,
    pub pwsh_path: Option<String>,
    /// Raw `pwsh --version` output, parsed later by `version.rs`.
    pub current_version: Option<String>,
    // Per-channel ownership hints: "did this manager report owning pwsh?"
    pub winget_lists_pwsh: bool,
    pub msix_registered: bool,
    pub msi_registered: bool,
    pub choco_lists_pwsh: bool,
    pub brew_lists_pwsh: bool,
    pub pkg_receipt_present: bool,
    pub dpkg_owns_pwsh: bool,
    pub rpm_owns_pwsh: bool,
    pub snap_lists_pwsh: bool,
    /// Set when pwsh is a portable tar.gz unpack owned by no package manager.
    pub portable_install_dir: Option<String>,
    /// Which manager executables exist on PATH (for exists()-gating + reporting).
    pub available_managers: Vec<InstallMethod>,
}

/// Result of pure detection (`detect::resolve`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// `None` == undetermined (FR-3/4/5 undetermined branch).
    pub selected: Option<InstallMethod>,
    /// Other detected channels, for reporting (FR-9).
    pub also_detected: Vec<InstallMethod>,
}

/// Resolved latest stable release. Never fabricated on failure (FR-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Latest STABLE version, parsed.
    pub version: semver::Version,
    /// Raw GitHub tag, e.g. "v7.4.6".
    pub tag: String,
}

/// Pure semver decision (FR-8, ADR-0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionState {
    UpToDate,
    UpdateAvailable,
}

/// A from-scratch install plan (ADR-0006): the native package-manager command
/// that installs PowerShell when none is present. Mirrors [`UpdatePlan`]'s
/// command shape so the host runs and reports it the same way. Resolved only
/// from the set of available managers — when none is available the host falls
/// back to the portable installer (delivered separately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    pub method: InstallMethod,
    pub program: String,
    pub args: Vec<String>,
    /// FR-12: the host surfaces this before execution; the tool never
    /// self-elevates.
    pub requires_elevation: bool,
}

/// A single `program args...` invocation. Used for a plan's optional follow-up
/// steps that must run *after* the primary upgrade command succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// The plan — built once, drives BOTH the `--check` report and the real update
/// (FR-9 agreement: reported command equals executed command).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    pub method: InstallMethod,
    pub from: semver::Version,
    pub to: semver::Version,
    pub program: String,
    pub args: Vec<String>,
    /// FR-12: the host surfaces this before/at execution; the tool never
    /// self-elevates.
    pub requires_elevation: bool,
    /// Follow-up commands run (in order) only after the primary command
    /// succeeds. Empty for most channels. Homebrew populates this with
    /// `brew link --overwrite powershell` because `brew upgrade powershell`
    /// routinely leaves the `pwsh` symlink unlinked on a file conflict, which
    /// otherwise forces users to relink by hand after every upgrade. These are
    /// best-effort self-healing steps: a failure is surfaced as a warning, not
    /// a failed update (the new version is already installed at that point).
    pub post_steps: Vec<PlanCommand>,
}
