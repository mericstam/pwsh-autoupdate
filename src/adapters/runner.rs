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
    /// Does a directory exist on the host filesystem? The only filesystem stat
    /// the probe needs (portable-install prefix detection). Defaults to `false`
    /// so test fakes are hermetic by construction — only `RealRunner` reaches
    /// the disk. Fakes that need to model a portable install override this.
    fn dir_exists(&self, _path: &str) -> bool {
        false
    }
    /// Resolve a program on PATH to its canonical, symlink-followed absolute
    /// path (e.g. `~/.local/bin/pwsh` -> `~/.local/share/powershell/7.6.2/pwsh`),
    /// or `None` if not found. Lets the probe recognize a user-local / manual
    /// portable install from where the real binary lives. Defaults to `None` so
    /// fakes are hermetic by construction; only `RealRunner` touches the disk.
    fn resolve_program_path(&self, _program: &str) -> Option<String> {
        None
    }
}

/// Platform executable filename for a program name: append `EXE_SUFFIX` only
/// when it is not already present, so "pwsh.exe" does not become "pwsh.exe.exe"
/// on Windows while a bare "winget" still becomes "winget.exe".
fn exe_filename(program: &str) -> String {
    let suffix = std::env::consts::EXE_SUFFIX;
    if !suffix.is_empty() && program.to_ascii_lowercase().ends_with(suffix) {
        program.to_string()
    } else {
        format!("{program}{suffix}")
    }
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
        // for an executable file with the program's name (+ EXE suffix). Using
        // the platform `EXE_SUFFIX` keeps lookup correct on Windows (`.exe`)
        // vs Unix (empty) by construction.
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        // `exe_filename` appends the platform suffix only when absent, so a
        // resolved "pwsh.exe" is not looked up as "pwsh.exe.exe" on Windows.
        let filename = exe_filename(program);
        std::env::split_paths(&path).any(|dir| dir.join(&filename).is_file())
    }

    fn dir_exists(&self, path: &str) -> bool {
        std::path::Path::new(path).is_dir()
    }

    fn resolve_program_path(&self, program: &str) -> Option<String> {
        let path = std::env::var_os("PATH")?;
        let filename = exe_filename(program);
        std::env::split_paths(&path)
            .map(|dir| dir.join(&filename))
            .find(|candidate| candidate.is_file())
            .and_then(|candidate| std::fs::canonicalize(candidate).ok())
            .map(|resolved| resolved.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Recording fake: returns canned `CmdOutput` per program and records every
    /// invocation. Models a non-zero manager exit and "off PATH" via `present`.
    #[derive(Default)]
    pub struct FakeRunner {
        pub canned: std::collections::HashMap<String, CmdOutput>,
        pub present: std::collections::HashSet<String>,
        pub calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn with_output(program: &str, out: CmdOutput) -> Self {
            let mut canned = std::collections::HashMap::new();
            canned.insert(program.to_string(), out);
            let mut present = std::collections::HashSet::new();
            present.insert(program.to_string());
            Self {
                canned,
                present,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CmdOutput> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            self.canned.get(program).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, format!("no fake: {program}"))
            })
        }
        fn exists(&self, program: &str) -> bool {
            self.present.contains(program)
        }
    }

    #[test]
    fn records_invocation_and_returns_canned_output() {
        let runner = FakeRunner::with_output(
            "winget",
            CmdOutput {
                status: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            },
        );
        let out = runner
            .run("winget", &["upgrade", "--id", "Microsoft.PowerShell"])
            .unwrap();
        assert_eq!(out.status, 0);
        assert_eq!(out.stdout, "ok");
        assert_eq!(
            runner.calls.borrow().as_slice(),
            &[(
                "winget".to_string(),
                vec![
                    "upgrade".to_string(),
                    "--id".to_string(),
                    "Microsoft.PowerShell".to_string()
                ]
            )]
        );
    }

    #[test]
    fn nonzero_exit_is_representable_as_surfaced_failure_not_success() {
        // A manager that exits non-zero is captured (status != 0) so the host
        // can surface a failure and never report success (FR-6/FR-12).
        let runner = FakeRunner::with_output(
            "apt-get",
            CmdOutput {
                status: 100,
                stdout: String::new(),
                stderr: "E: Unable to locate package".into(),
            },
        );
        let out = runner.run("apt-get", &["install", "powershell"]).unwrap();
        assert_ne!(out.status, 0);
        assert!(out.stderr.contains("Unable to locate package"));
    }

    #[test]
    fn exists_models_manager_off_path() {
        let runner = FakeRunner::default();
        assert!(!runner.exists("brew"));
    }

    // A single test owns the global-PATH critical section. `exists()` reads the
    // process-wide PATH env var, so two tests mutating it run as a data race
    // under cargo's multi-threaded runner (flaky, worst on Windows). Keeping
    // exactly one PATH-mutating test makes the suite deterministic.
    #[test]
    fn real_runner_exists_path_resolution() {
        // One temp dir on PATH holds a file named "<name><suffix>". No subprocess.
        let dir = tempfile::tempdir().unwrap();
        let name = "psautoupdater-fake-mgr";
        let suffix = std::env::consts::EXE_SUFFIX;
        let on_disk = format!("{name}{suffix}");
        std::fs::write(dir.path().join(&on_disk), b"#!/bin/sh\n").unwrap();

        let prev = std::env::var_os("PATH");
        let mut paths: Vec<std::path::PathBuf> = prev
            .as_ref()
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default();
        paths.insert(0, dir.path().to_path_buf());
        let joined = std::env::join_paths(&paths).unwrap();
        // SAFETY: single-threaded critical section; PATH is restored before returning.
        std::env::set_var("PATH", &joined);

        let runner = RealRunner;
        // Bare name resolves via the platform suffix.
        let found = runner.exists(name);
        let missing = runner.exists("psautoupdater-definitely-absent-xyz");
        // Already-suffixed name (as the probe passes "pwsh.exe" on Windows) must
        // NOT be double-suffixed into "pwsh.exe.exe". On Unix EXE_SUFFIX is empty
        // so this coincides with the bare lookup.
        let suffixed = runner.exists(&on_disk);
        // resolve_program_path returns the canonical on-disk path (PATH lookup +
        // canonicalize); compare against the canonicalized expected file.
        let resolved = runner.resolve_program_path(name);
        let resolved_missing = runner.resolve_program_path("psautoupdater-definitely-absent-xyz");
        let expected = std::fs::canonicalize(dir.path().join(&on_disk)).unwrap();

        match prev {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert!(found, "real exists() should locate a file on PATH");
        assert!(!missing, "real exists() should not find an absent program");
        assert!(
            suffixed,
            "already-suffixed name must not be double-suffixed (pwsh.exe.exe bug)"
        );
        assert_eq!(
            resolved.as_deref(),
            Some(expected.to_string_lossy().as_ref()),
            "resolve_program_path should return the canonical on-disk path"
        );
        assert!(
            resolved_missing.is_none(),
            "resolve_program_path returns None for an absent program"
        );
    }
}
