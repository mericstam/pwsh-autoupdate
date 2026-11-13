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
        // The concrete install TECHNOLOGY wins: winget correlates every
        // installed PowerShell (MSI, MSIX, ...) with `Microsoft.PowerShell`, so
        // `winget list` matching proves nothing about how it was installed and
        // is only a last-resort fallback. Chocolatey's `powershell-core` wraps
        // the MSI, so it must outrank the bare MSI registration.
        Os::Windows => vec![
            (InstallMethod::Msix, signals.msix_registered),
            (InstallMethod::Chocolatey, signals.choco_lists_pwsh),
            (InstallMethod::Msi, signals.msi_registered),
            (InstallMethod::Winget, signals.winget_lists_pwsh),
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

// --- Windows install-technology rules ---------------------------------------

/// Parsed output of the Windows install-technology query (see
/// `adapters::probe::WINDOWS_INSTALL_QUERY`): lines `MSI <product-code>` and
/// `MSIX <package-full-name>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsInstallQuery {
    /// First well-formed MSI product code found (`{GUID}`), if any.
    pub msi_product_code: Option<String>,
    /// An AppX/MSIX `Microsoft.PowerShell` package is registered.
    pub appx_registered: bool,
}

/// Parse the Windows install-technology query output. Malformed product codes
/// are dropped (fail-closed): the code is later passed to `msiexec`, so only a
/// strict `{8-4-4-4-12}` hex GUID is accepted.
pub fn parse_windows_install_query(stdout: &str) -> WindowsInstallQuery {
    let mut q = WindowsInstallQuery::default();
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(code) = line.strip_prefix("MSI ") {
            let code = code.trim();
            if q.msi_product_code.is_none() && is_msi_product_code(code) {
                q.msi_product_code = Some(code.to_ascii_uppercase());
            }
        } else if line.starts_with("MSIX ") {
            q.appx_registered = true;
        }
    }
    q
}

/// Strict MSI product-code shape: `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
pub fn is_msi_product_code(s: &str) -> bool {
    let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) else {
        return false;
    };
    let groups: Vec<&str> = inner.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    groups.len() == lens.len()
        && groups
            .iter()
            .zip(lens)
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Is this resolved pwsh path inside an MSIX/AppX package (or its App
/// Execution Alias)? Both live under a `WindowsApps` directory.
pub fn is_windowsapps_path(path: &str) -> bool {
    path.to_ascii_lowercase()
        .replace('/', "\\")
        .contains("\\windowsapps\\")
}

/// Extract the installer type from `winget show` output, lowercase.
///
/// winget localizes its labels ("Installer Type", "Installertyp", ...), but
/// not the type values, so match a `label: value` line whose value is a known
/// winget installer type rather than matching the label text.
pub fn parse_winget_installer_type(stdout: &str) -> Option<String> {
    const KNOWN: &[&str] = &[
        "msix", "appx", "msi", "wix", "msstore", "exe", "burn", "inno", "nullsoft", "zip",
        "portable",
    ];
    stdout.lines().find_map(|line| {
        let (_, value) = line.rsplit_once(':')?;
        let value = value.trim().to_ascii_lowercase();
        KNOWN.contains(&value.as_str()).then_some(value)
    })
}

/// True when an MSI install can no longer be upgraded in place because winget's
/// latest `Microsoft.PowerShell` installer is MSIX (PowerShell 7.7.0+ ships no
/// MSI). The host then requires the opt-in `--migrate` path.
pub fn msi_needs_msix_migration(
    selected: Option<InstallMethod>,
    signals: &DetectionSignals,
) -> bool {
    selected == Some(InstallMethod::Msi)
        && matches!(
            signals.winget_installer_type.as_deref(),
            Some("msix") | Some("appx")
        )
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
    fn windows_precedence_choco_over_msi_over_winget() {
        let mut s = signals();
        s.winget_lists_pwsh = true;
        s.msi_registered = true;
        s.choco_lists_pwsh = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Chocolatey));
        assert_eq!(
            d.also_detected,
            vec![InstallMethod::Msi, InstallMethod::Winget]
        );
    }

    #[test]
    fn windows_msi_install_seen_by_winget_resolves_to_msi() {
        // Regression: winget lists an MSI-installed pwsh as Microsoft.PowerShell;
        // that must NOT make winget the owning channel (0x8A15008E on 7.7.0).
        let mut s = signals();
        s.winget_lists_pwsh = true;
        s.msi_registered = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Msi));
        assert_eq!(d.also_detected, vec![InstallMethod::Winget]);
    }

    #[test]
    fn windows_msix_outranks_everything() {
        let mut s = signals();
        s.winget_lists_pwsh = true;
        s.msi_registered = true;
        s.msix_registered = true;
        let d = resolve(Os::Windows, &s);
        assert_eq!(d.selected, Some(InstallMethod::Msix));
    }

    // --- Windows install-technology parsers ---------------------------------

    #[test]
    fn parses_msi_and_msix_query_lines() {
        let q = parse_windows_install_query(
            "MSI {5417b253-1a6d-4638-bc94-743a6ec2e6f4}\r\nMSIX Microsoft.PowerShell_7.7.0.0_x64__8wekyb3d8bbwe\r\n",
        );
        assert_eq!(
            q.msi_product_code.as_deref(),
            Some("{5417B253-1A6D-4638-BC94-743A6EC2E6F4}")
        );
        assert!(q.appx_registered);
    }

    #[test]
    fn rejects_malformed_product_codes() {
        for bad in [
            "MSI 5417B253-1A6D-4638-BC94-743A6EC2E6F4",
            "MSI {5417B253-1A6D-4638-BC94-743A6EC2E6F}",
            "MSI {5417B253-1A6D-4638-BC94-743A6EC2E6FZ}",
            "MSI {x} & calc.exe",
        ] {
            assert_eq!(
                parse_windows_install_query(bad).msi_product_code,
                None,
                "{bad}"
            );
        }
        assert_eq!(
            parse_windows_install_query(""),
            WindowsInstallQuery::default()
        );
    }

    #[test]
    fn windowsapps_path_detection() {
        assert!(is_windowsapps_path(
            r"\\?\C:\Program Files\WindowsApps\Microsoft.PowerShell_7.7.0.0_x64__8wekyb3d8bbwe\pwsh.exe"
        ));
        assert!(is_windowsapps_path(
            r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\pwsh.exe"
        ));
        assert!(!is_windowsapps_path(
            r"\\?\C:\Program Files\PowerShell\7\pwsh.exe"
        ));
    }

    #[test]
    fn parses_winget_installer_type_any_locale() {
        let en = "Found PowerShell [Microsoft.PowerShell]\nVersion: 7.7.0.0\nInstaller:\n  Installer Type: msix\n  Installer Url: https://github.com/x.msixbundle\n  Offline Distribution Supported: true\n";
        assert_eq!(parse_winget_installer_type(en).as_deref(), Some("msix"));
        let de = "Installer:\n  Installertyp: wix\n  Installer-URL: https://github.com/x.msi\n";
        assert_eq!(parse_winget_installer_type(de).as_deref(), Some("wix"));
        assert_eq!(parse_winget_installer_type("No package found."), None);
    }

    #[test]
    fn migration_needed_only_for_msi_with_msix_manifest() {
        let mut s = signals();
        s.winget_installer_type = Some("msix".into());
        assert!(msi_needs_msix_migration(Some(InstallMethod::Msi), &s));
        assert!(!msi_needs_msix_migration(Some(InstallMethod::Msix), &s));
        s.winget_installer_type = Some("wix".into());
        assert!(!msi_needs_msix_migration(Some(InstallMethod::Msi), &s));
        s.winget_installer_type = None;
        assert!(!msi_needs_msix_migration(Some(InstallMethod::Msi), &s));
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
