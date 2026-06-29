//! CLI surface (`clap` derive). Parsing only — no domain logic here. The host
//! hands the typed args to the orchestration layer.

use clap::Parser;

/// Detect how PowerShell was installed and update it via the owning package
/// manager.
#[derive(Debug, Parser)]
#[command(name = "pwsh-autoupdate", version, about)]
pub struct Cli {
    /// Report current/latest version, detected method, and the exact command
    /// that would run — without performing any update (dry run).
    #[arg(long)]
    pub check: bool,

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
