//! Thin host entry point.
//!
//! Responsibilities (kept deliberately minimal): parse the CLI, build the real
//! adapters, run the chosen flow, and map the result to a process exit code.
//! No domain logic lives here — that is in `core/`, behind the orchestration
//! entry points in `lib.rs`.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Orchestration entry points (`run_check` / `run_update`) are wired in a
    // later cluster; until then the binary parses args and reports that the
    // host flow is not yet wired, without performing any side effects.
    let _cli = pwsh_autoupdate::cli::Cli::parse_args();
    ExitCode::SUCCESS
}
