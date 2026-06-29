//! Install-method detection rules + precedence (FR-3/4/5/9, ADR-0004).
//!
//! Pure rules over already-probed [`DetectionSignals`] plain values. No
//! environment, filesystem, or process access — the probe adapter gathers the
//! signals; these rules only interpret them, so they unit-test identically on
//! every platform.
//!
//! Each per-channel ownership hint in [`DetectionSignals`] maps to one
//! [`InstallMethod`]. When several are present the per-OS precedence in
//! ADR-0004 selects exactly one winner; the rest become the `also_detected`
//! set used for reporting (FR-9). When none are present the outcome is an
//! explicit `selected: None` (undetermined).

use crate::core::{Detection, DetectionSignals, InstallMethod, Os};

/// Resolve the detected methods from probed signals, applying the ADR-0004
/// per-OS precedence to pick exactly one winner plus the also-detected set.
///
/// The precedence order list is OS-specific and ordered highest-first; the
/// first method whose ownership hint is set wins, the remaining set hints
/// become `also_detected` (in precedence order, for stable reporting).
pub fn resolve(os: Os, signals: &DetectionSignals) -> Detection {
    // Build the candidate list in precedence order (highest first) for this OS,
    // pairing each method with whether its ownership hint fired.
    let candidates: Vec<(InstallMethod, bool)> = match os {
        Os::Windows => vec![
            (InstallMethod::Winget, signals.winget_lists_pwsh),
            (InstallMethod::Msix, signals.msix_registered),
            (InstallMethod::Msi, signals.msi_registered),
            (InstallMethod::Chocolatey, signals.choco_lists_pwsh),
        ],
        Os::Macos => vec![
            (InstallMethod::Homebrew, signals.brew_lists_pwsh),
            (InstallMethod::MacPkg, signals.pkg_receipt_present),
        ],
        Os::Linux => vec![
            // Native system package managers that *own* the binary rank above
            // snap; portable tar.gz (owned by no manager) is lowest.
            (InstallMethod::AptDpkg, signals.dpkg_owns_pwsh),
            (InstallMethod::DnfRpm, signals.rpm_owns_pwsh),
            (InstallMethod::Snap, signals.snap_lists_pwsh),
            (
                InstallMethod::PortableTarGz,
                signals.portable_install_dir.is_some(),
            ),
        ],
    };

    let detected: Vec<InstallMethod> = candidates
        .iter()
        .filter(|(_, present)| *present)
        .map(|(method, _)| *method)
        .collect();

    let mut iter = detected.into_iter();
    let selected = iter.next();
    let also_detected = iter.collect();

    Detection {
        selected,
        also_detected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals() -> DetectionSignals {
        DetectionSignals {
            pwsh_present: true,
            ..Default::default()
        }
    }

    // --- Windows channels ---------------------------------------------------

    #[test]
    fn windows_winget_only() {
        let mut s = signals();
        s.winget_lists_pwsh = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Winget));
        assert!(d.also_detected.is_empty());
    }

    #[test]
    fn windows_msix_only() {
        let mut s = signals();
        s.msix_registered = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Msix));
    }

    #[test]
    fn windows_msi_only() {
        let mut s = signals();
        s.msi_registered = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Msi));
    }

    #[test]
    fn windows_choco_only() {
        let mut s = signals();
        s.choco_lists_pwsh = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Chocolatey));
    }

    #[test]
    fn windows_precedence_winget_over_msi_and_choco() {
        let mut s = signals();
        s.winget_lists_pwsh = true;
        s.msi_registered = true;
        s.choco_lists_pwsh = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Winget));
        assert_eq!(
            d.also_detected,
            vec![InstallMethod::Msi, InstallMethod::Chocolatey]
        );
    }

    // --- macOS channels -----------------------------------------------------

    #[test]
    fn macos_homebrew_only() {
        let mut s = signals();
        s.brew_lists_pwsh = true;
        let d = resolve(Os::Macos, &s);
        assert_eq!(d.selected, Some(InstallMethod::Homebrew));
    }

    #[test]
    fn macos_pkg_only() {
        let mut s = signals();
        s.pkg_receipt_present = true;
        let d = resolve(Os::Macos, &s);
        assert_eq!(d.selected, Some(InstallMethod::MacPkg));
    }

    #[test]
    fn macos_precedence_homebrew_over_pkg() {
        let mut s = signals();
        s.brew_lists_pwsh = true;
        s.pkg_receipt_present = true;
        let d = resolve(Os::Macos, &s);
        assert_eq!(d.selected, Some(InstallMethod::Homebrew));
        assert_eq!(d.also_detected, vec![InstallMethod::MacPkg]);
    }

    // --- Linux channels -----------------------------------------------------

    #[test]
    fn linux_apt_only() {
        let mut s = signals();
        s.dpkg_owns_pwsh = true;
        let d = resolve(Os::Linux, &s);
        assert_eq!(d.selected, Some(InstallMethod::AptDpkg));
    }

    #[test]
    fn linux_dnf_only() {
        let mut s = signals();
        s.rpm_owns_pwsh = true;
        let d = resolve(Os::Linux, &s);
        assert_eq!(d.selected, Some(InstallMethod::DnfRpm));
    }

    #[test]
    fn linux_snap_only() {
        let mut s = signals();
        s.snap_lists_pwsh = true;
        let d = resolve(Os::Linux, &s);
        assert_eq!(d.selected, Some(InstallMethod::Snap));
    }

    #[test]
    fn linux_portable_targz_only() {
        let mut s = signals();
        s.portable_install_dir = Some("/opt/microsoft/powershell/7".to_string());
        let d = resolve(Os::Linux, &s);
        assert_eq!(d.selected, Some(InstallMethod::PortableTarGz));
    }

    #[test]
    fn linux_precedence_native_over_snap_over_portable() {
        let mut s = signals();
        s.dpkg_owns_pwsh = true;
        s.snap_lists_pwsh = true;
        s.portable_install_dir = Some("/opt/microsoft/powershell/7".to_string());
        let d = resolve(Os::Linux, &s);
        assert_eq!(d.selected, Some(InstallMethod::AptDpkg));
        assert_eq!(
            d.also_detected,
            vec![InstallMethod::Snap, InstallMethod::PortableTarGz]
        );
    }

    // --- Undetermined -------------------------------------------------------

    #[test]
    fn none_detected_is_undetermined() {
        let s = signals();
        let d = resolve(Os::Linux, &s);
        assert_eq!(d.selected, None);
        assert!(d.also_detected.is_empty());
    }
}
