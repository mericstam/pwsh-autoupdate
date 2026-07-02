//! Thin host entry point.
//!
//! Responsibilities (kept deliberately minimal): parse the CLI, build the real
//! adapters, run the chosen flow via the shared orchestration in `app`, and map
//! the returned code to a process exit. No domain logic lives here.
//!
//! The real adapters (`RealHttp`, `RealRunner`) are wired here and passed as the
//! same trait objects the hermetic tests use, so the shipping binary exercises
//! the production code path — never a divergent one.

use pwsh_autoupdate::adapters::http::RealHttp;
use pwsh_autoupdate::adapters::runner::RealRunner;
use pwsh_autoupdate::app::{self, EXIT_FAILURE};
use pwsh_autoupdate::cli::Cli;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse_args();

    let os = match app::host_os() {
        Some(os) => os,
        None => {
            eprintln!(
                "error: unsupported operating system ({}); this tool supports Windows, macOS, and Linux only",
                std::env::consts::OS
            );
            return ExitCode::from(EXIT_FAILURE as u8);
        }
    };

    let http = RealHttp::new(app::USER_AGENT);
    let runner = RealRunner;

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();

    let code = if cli.check {
        app::run_check(&http, &runner, os, &mut stdout, &mut stderr)
    } else if cli.replace_portable {
        app::run_replace_portable(&http, &runner, os, &mut stdout, &mut stderr)
    } else {
        app::run_update(&http, &runner, os, &mut stdout, &mut stderr)
    };

    ExitCode::from(code as u8)
}
