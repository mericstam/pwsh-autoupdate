//! CLI surface (`clap` derive). Parsing only — no domain logic here. The host
//! hands the typed args to the orchestration layer (`run_check` / `run_update`).
//!
//! Three mutually-distinct run modes are exposed:
//! * default (no flag): the update path — detect the owning manager and run
//!   the upgrade command (a mutating path).
//! * `--check`: a read-only dry run — report current/latest version, detected
//!   method, and the exact command that *would* run, performing no update.
//! * `--replace-portable`: the portable tar.gz download-and-replace (Linux) —
//!   the command the update path reports for a portable install, runnable
//!   directly (a mutating path).
//!
//! `--verbose` only toggles extra diagnostic output; it carries no domain logic.

use clap::Parser;

/// Detect how PowerShell was installed and update it via the owning package
/// manager.
#[derive(Debug, Parser)]
#[command(name = "pwsh-autoupdate", version, about, long_about = None)]
pub struct Cli {
    /// Report current/latest version, detected method, and the exact command
    /// that would run — without performing any update (dry run, no side
    /// effects). Exit code: 0 up-to-date, 1 update-available, 2 error.
    #[arg(long)]
    pub check: bool,

    /// Update a portable tar.gz install of PowerShell (Linux only): download
    /// the latest release tarball, verify its SHA-256 against the release hash
    /// manifest, and atomically replace the existing portable install
    /// directory. This is the command the default update path runs for a
    /// portable install; refuses to touch an install owned by a package
    /// manager.
    #[arg(long, conflicts_with = "check")]
    pub replace_portable: bool,

    /// Print additional diagnostic detail to stderr.
    #[arg(long, short)]
    pub verbose: bool,
}

impl Cli {
    /// Parse the process arguments into a typed `Cli`.
    pub fn parse_args() -> Self {
        Self::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn defaults_to_update_path() {
        let cli = Cli::parse_from(["pwsh-autoupdate"]);
        assert!(!cli.check);
        assert!(!cli.replace_portable);
        assert!(!cli.verbose);
    }

    #[test]
    fn parses_replace_portable_flag() {
        let cli = Cli::parse_from(["pwsh-autoupdate", "--replace-portable"]);
        assert!(cli.replace_portable);
        assert!(!cli.check);
    }

    #[test]
    fn replace_portable_conflicts_with_check() {
        // A dry run that mutates is a contradiction; the two flags never combine.
        assert!(Cli::try_parse_from(["pwsh-autoupdate", "--check", "--replace-portable"]).is_err());
    }

    #[test]
    fn parses_check_flag() {
        let cli = Cli::parse_from(["pwsh-autoupdate", "--check"]);
        assert!(cli.check);
        assert!(!cli.verbose);
    }

    #[test]
    fn parses_verbose_flag_long_and_short() {
        assert!(Cli::parse_from(["pwsh-autoupdate", "--verbose"]).verbose);
        assert!(Cli::parse_from(["pwsh-autoupdate", "-v"]).verbose);
    }

    #[test]
    fn parses_check_and_verbose_together() {
        let cli = Cli::parse_from(["pwsh-autoupdate", "--check", "--verbose"]);
        assert!(cli.check);
        assert!(cli.verbose);
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(Cli::try_parse_from(["pwsh-autoupdate", "--definitely-not-a-flag"]).is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        // Catches conflicting arg definitions at test time.
        Cli::command().debug_assert();
    }
}
