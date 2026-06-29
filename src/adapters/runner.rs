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
        // Only append the platform suffix when the name doesn't already carry
        // it: callers pass bare manager names ("winget") but also the resolved
        // PowerShell exe ("pwsh.exe" on Windows). Blindly appending would look
        // for "pwsh.exe.exe" and never find a real pwsh on Windows.
        let suffix = std::env::consts::EXE_SUFFIX;
        let already_suffixed = !suffix.is_empty() && program.to_ascii_lowercase().ends_with(suffix);
        let filename = if already_suffixed {
            program.to_string()
        } else {
            format!("{program}{suffix}")
        };
        std::env::split_paths(&path).any(|dir| dir.join(&filename).is_file())
    }

    fn dir_exists(&self, path: &str) -> bool {
        std::path::Path::new(path).is_dir()
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

    #[test]
    fn real_runner_exists_finds_executable_on_path() {
        // Drive the real PATH-lookup logic with a temp dir on PATH holding a
        // file named like the program (+ platform EXE suffix). No subprocess.
        let dir = tempfile::tempdir().unwrap();
        let name = "psautoupdater-fake-mgr";
        let file = dir
            .path()
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&file, b"#!/bin/sh\n").unwrap();

        let prev = std::env::var_os("PATH");
        let mut paths: Vec<std::path::PathBuf> = prev
            .as_ref()
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default();
        paths.insert(0, dir.path().to_path_buf());
        let joined = std::env::join_paths(&paths).unwrap();
        // SAFETY: single-threaded test; PATH is restored before returning.
        std::env::set_var("PATH", &joined);

        let runner = RealRunner;
        let found = runner.exists(name);
        let missing = runner.exists("psautoupdater-definitely-absent-xyz");

        match prev {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert!(found, "real exists() should locate a file on PATH");
        assert!(!missing, "real exists() should not find an absent program");
    }

    #[test]
    fn real_runner_exists_accepts_already_suffixed_name() {
        // On Windows the probe asks for the resolved exe ("pwsh.exe"); exists()
        // must not append a second ".exe". The on-disk file is "<name><suffix>",
        // and BOTH the bare and the already-suffixed lookups must find it. On
        // Unix EXE_SUFFIX is empty, so the two forms coincide and still pass.
        let dir = tempfile::tempdir().unwrap();
        let name = "psautoupdater-fake-pwsh";
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
        // SAFETY: single-threaded test; PATH is restored before returning.
        std::env::set_var("PATH", &joined);

        let runner = RealRunner;
        let bare = runner.exists(name);
        let suffixed = runner.exists(&on_disk);

        match prev {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert!(bare, "bare name should resolve with the platform suffix");
        assert!(
            suffixed,
            "already-suffixed name must not be double-suffixed (pwsh.exe.exe bug)"
        );
    }
}
