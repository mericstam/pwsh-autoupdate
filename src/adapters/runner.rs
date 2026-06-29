//! Process-runner seam.
//!
//! The real implementation is the only place in the crate that touches
//! `std::process::Command`. The pure core never runs subprocesses; it only
//! builds the `(program, args)` that this adapter executes.

/// Captured result of running an external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run host package-manager commands; check availability on PATH. Tests inject
/// a fake that records invocations and returns canned output.
pub trait CommandRunner {
    /// Run a program with args; capture status + stdout + stderr.
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CmdOutput>;
    /// Is the program resolvable on PATH? Used before running a plan's command.
    fn exists(&self, program: &str) -> bool;
}

/// Real runner over `std::process::Command`, confined to the adapter layer.
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CmdOutput> {
        let output = std::process::Command::new(program).args(args).output()?;
        Ok(CmdOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn exists(&self, program: &str) -> bool {
        // Resolve on PATH without running the program: probe each PATH entry
        // for an executable file with the program's name (+ EXE suffix).
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(format!("{program}{}", std::env::consts::EXE_SUFFIX));
            candidate.is_file()
        })
    }
}
