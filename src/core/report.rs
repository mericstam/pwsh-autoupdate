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

use crate::core::{Detection, InstallMethod, UpdatePlan, VersionState};
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
                let cmd = if plan.args.is_empty() {
                    plan.program.clone()
                } else {
                    format!("{} {}", plan.program, plan.args.join(" "))
                };
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
