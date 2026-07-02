//! `--check` report model + `Display` (FR-7).
//!
//! Pure: the report is built from already-computed core values (current
//! version, detection, resolved latest, and the optional plan) and rendered via
//! `Display`. It shows the current version, the detected method plus any
//! also-detected channels, the latest version, the [`VersionState`], and the
//! exact command that *would* run. When the method is undetermined it still
//! shows current + latest and states that no command can be run.
//!
//! The rendered command is taken verbatim from the [`UpdatePlan`] so the
//! reported command equals the executed one (FR-9 / ADR-0004 agreement).

use crate::core::{Detection, InstallMethod, UninstallPlan, UpdatePlan, VersionState};
use semver::Version;
use std::fmt;

/// The `--check` report model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// The installed PowerShell version.
    pub current: Version,
    /// The latest resolved stable version.
    pub latest: Version,
    /// Up-to-date vs update-available decision.
    pub state: VersionState,
    /// Detected install method(s): the winner (if any) + also-detected.
    pub detection: Detection,
    /// The plan whose command would run, when a method was determined and an
    /// update is available. `None` when undetermined or already up to date.
    pub plan: Option<UpdatePlan>,
}

impl CheckReport {
    /// Build a report from the computed core values.
    pub fn build(
        current: Version,
        latest: Version,
        state: VersionState,
        detection: Detection,
        plan: Option<UpdatePlan>,
    ) -> Self {
        Self {
            current,
            latest,
            state,
            detection,
            plan,
        }
    }
}

fn render_methods(selected: Option<InstallMethod>, also: &[InstallMethod]) -> String {
    match selected {
        None => "undetermined".to_string(),
        Some(method) => {
            let mut s = method.label().to_string();
            if !also.is_empty() {
                let others: Vec<&str> = also.iter().map(|m| m.label()).collect();
                s.push_str(&format!(" (also detected: {})", others.join(", ")));
            }
            s
        }
    }
}

impl fmt::Display for CheckReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Current version: {}", self.current)?;
        writeln!(
            f,
            "Detected method: {}",
            render_methods(self.detection.selected, &self.detection.also_detected)
        )?;
        writeln!(f, "Latest version:  {}", self.latest)?;

        let status = match self.state {
            VersionState::UpToDate => "up to date",
            VersionState::UpdateAvailable => "update available",
        };
        writeln!(f, "Status:          {status}")?;

        match (&self.plan, self.detection.selected) {
            (Some(plan), _) => {
                let render = |program: &str, args: &[String]| {
                    if args.is_empty() {
                        program.to_string()
                    } else {
                        format!("{} {}", program, args.join(" "))
                    }
                };
                // Primary command plus any follow-up steps, joined with `&&` to
                // convey "the next runs only if the previous succeeded" — the
                // exact execution semantics (FR-9 agreement).
                let mut cmd = render(&plan.program, &plan.args);
                for step in &plan.post_steps {
                    cmd.push_str(" && ");
                    cmd.push_str(&render(&step.program, &step.args));
                }
                write!(f, "Would run:       {cmd}")?;
            }
            (None, None) => {
                write!(
                    f,
                    "Would run:       (none -- install method undetermined; cannot determine an upgrade command)"
                )?;
            }
            (None, Some(_)) => {
                // Method known but no plan: already up to date.
                write!(f, "Would run:       (none -- already up to date)")?;
            }
        }
        Ok(())
    }
}

/// The `--uninstall` report model (ADR-0007). Printed both as the preview
/// (without `--yes`) and as the pre-execution echo (with `--yes`), so the shown
/// command always equals the executed one (FR-9 agreement).
///
/// Unlike [`CheckReport`] it carries no latest version or [`VersionState`] —
/// uninstalling needs neither (and performs no network read at all). The
/// current version is optional: an unparseable `pwsh --version` must not block
/// an uninstall that does not depend on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallReport {
    /// The installed PowerShell version, when it parsed.
    pub current: Option<Version>,
    /// Detected install method(s): the winner + also-detected.
    pub detection: Detection,
    /// The plan whose command would run / is about to run.
    pub plan: UninstallPlan,
}

impl fmt::Display for UninstallReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.current {
            Some(v) => writeln!(f, "Current version: {v}")?,
            None => writeln!(f, "Current version: unknown")?,
        }
        writeln!(
            f,
            "Detected method: {}",
            render_methods(self.detection.selected, &self.detection.also_detected)
        )?;
        let render = |program: &str, args: &[String]| {
            if args.is_empty() {
                program.to_string()
            } else {
                format!("{} {}", program, args.join(" "))
            }
        };
        // Primary command plus follow-up steps, `&&`-joined like CheckReport.
        let mut cmd = render(&self.plan.program, &self.plan.args);
        for step in &self.plan.post_steps {
            cmd.push_str(" && ");
            cmd.push_str(&render(&step.program, &step.args));
        }
        write!(f, "Would run:       {cmd}")?;
        if self.plan.requires_elevation {
            write!(f, "\n                 (requires elevation)")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Os;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn winget_plan() -> UpdatePlan {
        crate::core::plan::build_plan(InstallMethod::Winget, v("7.4.0"), v("7.5.0"), Os::Windows)
            .unwrap()
    }

    #[test]
    fn update_available_renders_all_fields_and_command() {
        let report = CheckReport::build(
            v("7.4.0"),
            v("7.5.0"),
            VersionState::UpdateAvailable,
            Detection {
                selected: Some(InstallMethod::Winget),
                also_detected: vec![],
            },
            Some(winget_plan()),
        );
        let out = report.to_string();
        assert!(out.contains("Current version: 7.4.0"));
        assert!(out.contains("Latest version:  7.5.0"));
        assert!(out.contains("Detected method: winget"));
        assert!(out.contains("Status:          update available"));
        assert!(out.contains(
            "Would run:       winget upgrade --id Microsoft.PowerShell --silent --accept-source-agreements --accept-package-agreements"
        ));
    }

    #[test]
    fn homebrew_report_shows_relink_follow_up_step() {
        let plan = crate::core::plan::build_plan(
            InstallMethod::Homebrew,
            v("7.4.0"),
            v("7.5.0"),
            Os::Macos,
        )
        .unwrap();
        let report = CheckReport::build(
            v("7.4.0"),
            v("7.5.0"),
            VersionState::UpdateAvailable,
            Detection {
                selected: Some(InstallMethod::Homebrew),
                also_detected: vec![],
            },
            Some(plan),
        );
        let out = report.to_string();
        assert!(out.contains(
            "Would run:       brew upgrade powershell && brew link --overwrite powershell"
        ));
    }

    #[test]
    fn also_detected_methods_are_listed() {
        let report = CheckReport::build(
            v("7.4.0"),
            v("7.5.0"),
            VersionState::UpdateAvailable,
            Detection {
                selected: Some(InstallMethod::Winget),
                also_detected: vec![InstallMethod::Msi, InstallMethod::Chocolatey],
            },
            Some(winget_plan()),
        );
        let out = report.to_string();
        assert!(out.contains("Detected method: winget (also detected: MSI, Chocolatey)"));
    }

    #[test]
    fn up_to_date_states_no_command() {
        let report = CheckReport::build(
            v("7.5.0"),
            v("7.5.0"),
            VersionState::UpToDate,
            Detection {
                selected: Some(InstallMethod::Winget),
                also_detected: vec![],
            },
            None,
        );
        let out = report.to_string();
        assert!(out.contains("Status:          up to date"));
        assert!(out.contains("already up to date"));
    }

    // --- UninstallReport (ADR-0007) -------------------------------------------

    fn uninstall_report(
        current: Option<&str>,
        method: InstallMethod,
        os: Os,
        also: Vec<InstallMethod>,
    ) -> UninstallReport {
        UninstallReport {
            current: current.map(v),
            detection: Detection {
                selected: Some(method),
                also_detected: also,
            },
            plan: crate::core::plan::build_uninstall_plan(method, os).unwrap(),
        }
    }

    #[test]
    fn uninstall_report_renders_version_method_and_command() {
        let out =
            uninstall_report(Some("7.4.0"), InstallMethod::AptDpkg, Os::Linux, vec![]).to_string();
        assert!(out.contains("Current version: 7.4.0"));
        assert!(out.contains("Detected method: apt/dpkg"));
        assert!(out.contains("Would run:       apt-get remove -y powershell"));
        assert!(out.contains("(requires elevation)"));
    }

    #[test]
    fn uninstall_report_unknown_version_still_renders() {
        let out = uninstall_report(None, InstallMethod::Homebrew, Os::Macos, vec![]).to_string();
        assert!(out.contains("Current version: unknown"));
        assert!(out.contains("Would run:       brew uninstall powershell"));
        // Homebrew needs no elevation; the line must be absent.
        assert!(!out.contains("requires elevation"));
    }

    #[test]
    fn uninstall_report_macpkg_joins_receipt_cleanup_with_and_and() {
        let out =
            uninstall_report(Some("7.4.0"), InstallMethod::MacPkg, Os::Macos, vec![]).to_string();
        assert!(out.contains(
            "Would run:       rm -rf /usr/local/microsoft/powershell /usr/local/bin/pwsh && pkgutil --forget com.microsoft.powershell"
        ));
        assert!(out.contains("(requires elevation)"));
    }

    #[test]
    fn uninstall_report_lists_also_detected_methods() {
        let out = uninstall_report(
            Some("7.4.0"),
            InstallMethod::AptDpkg,
            Os::Linux,
            vec![InstallMethod::Snap],
        )
        .to_string();
        assert!(out.contains("Detected method: apt/dpkg (also detected: snap)"));
    }

    #[test]
    fn undetermined_method_shows_versions_and_no_command() {
        let report = CheckReport::build(
            v("7.4.0"),
            v("7.5.0"),
            VersionState::UpdateAvailable,
            Detection {
                selected: None,
                also_detected: vec![],
            },
            None,
        );
        let out = report.to_string();
        assert!(out.contains("Current version: 7.4.0"));
        assert!(out.contains("Latest version:  7.5.0"));
        assert!(out.contains("Detected method: undetermined"));
        assert!(out.contains("undetermined; cannot determine an upgrade command"));
    }
}
