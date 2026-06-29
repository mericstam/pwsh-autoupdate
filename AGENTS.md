# AGENTS.md — pwsh-autoupdate

Guidance for agents and contributors working in this repository. Read this
first, then [README.md](README.md) for product behavior and
[docs/PRD.md](docs/PRD.md) for the product contract and Constitution.

## Project overview

`pwsh-autoupdate` is a single-binary, cross-platform Rust CLI that detects how
the existing cross-platform PowerShell (`pwsh`) was installed and updates it via
the owning package manager (winget/MSIX/MSI/Chocolatey on Windows;
Homebrew/`.pkg` on macOS; apt-dpkg/dnf-rpm/snap/portable tar.gz on Linux). It
updates an existing install only — it never installs `pwsh` from scratch and
never bundles a PowerShell payload.

## Build / test / self-verify gate

Run all four, in this order, and require each to pass before committing. This is
the self-verify gate:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

- `cargo fmt --check` — formatting must already be clean (run `cargo fmt` to fix).
- `cargo clippy --all-targets -- -D warnings` — clippy warnings are errors.
- `cargo test` — the full hermetic suite (unit + the `tests/cli.rs` integration
  tests). Must be green with no network access and no real package-manager
  invocation.
- `cargo build --release` — release binary builds; the binary is at
  `target/release/pwsh-autoupdate`.

In this Linux sandbox, set the linker first if the release build cannot find one:

```sh
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/cc
```

## Run

```sh
pwsh-autoupdate            # update via the owning channel (mutating)
pwsh-autoupdate --check    # dry run, no side effects
pwsh-autoupdate --verbose  # extra diagnostics on stderr
```

Exit codes follow ADR-0002 (see README). `--check`: 0 up-to-date, 1
update-available, 2 error. Default mode: 0 success, non-zero failure.

## Architecture boundary (core / adapters / host)

The dependency arrows point inward. Do not violate these layers.

- **core** (`src/core/`) — pure domain logic: version parsing and comparison
  (`version.rs`), install-method detection rules (`detect.rs`), update-plan
  production (`plan.rs`), the `--check` report model (`report.rs`), and error
  types (`error.rs`). Enforced invariant: no file under `src/core/` may import
  `ureq`, `std::process::Command`, or `std::env`, or perform any network /
  subprocess / environment probing. Core receives already-probed, already-fetched
  plain values and returns plain values. The `Os` and `InstallMethod` enums are
  passed in as data, so every rule unit-tests identically on every platform.
- **adapters** (`src/adapters/`) — the IO seams behind traits: `HttpClient`
  (`http.rs`, the only place that touches `ureq`), `CommandRunner` (`runner.rs`,
  the only place that spawns processes), and `probe.rs` (reads `pwsh` and
  ownership signals via the runner). Real implementations: `RealHttp`,
  `RealRunner`.
- **host** (`src/app.rs`, `src/cli.rs`, `src/main.rs`) — `cli.rs` is the clap
  parsing surface (no domain logic). `app.rs` assembles the
  probe → detect → resolve → classify → (report | run) flow and owns the
  ADR-0002 exit codes. `main.rs` is a thin entry point that wires the real
  adapters and maps the returned code to a process exit.

The orchestration entry points (`app::run_check`, `app::run_update`) take
`&dyn HttpClient` + `&dyn CommandRunner` + the resolved `Os`, so the shipping
binary and the tests drive the **same** code path — there is no divergent "test
path".

## How tests stay hermetic

- Unit tests inject **fakes** for `HttpClient` and `CommandRunner`, with canned
  HTTP bodies and canned command outputs. No real network, no real subprocess.
- The integration tests in `tests/cli.rs` drive the real `RealHttp` path against
  an in-process **`mockito`** HTTP server.
- Test-only env override seam: `PWSH_AUTOUPDATE_RELEASES_URL` and
  `PWSH_AUTOUPDATE_BUILDINFO_URL` override the GitHub-releases and build-info
  source URLs so the integration tests can point `RealHttp` at the mock server.
  These are **unset in production** (the defaults are the real pinned URLs).
  Treat unset/empty as "no override". They do not change production behavior and
  do not weaken the no-fabricated-version rule — a failed fetch against whatever
  URL is in effect still surfaces an error, never a version.

When you change tests that score an FR's acceptance criteria, reconcile the
owning increment PRD and the FR registry in [docs/PRD.md](docs/PRD.md).

## Code style and conventions

- Rust stable toolchain, 2021 edition. Keep `cargo fmt` clean and clippy
  warning-free (`-D warnings`).
- Keep domain logic in `core`; keep IO in `adapters`; keep `main.rs` thin.
- New detection rules, version logic, plan mapping, and report shaping go in
  `core` with unit tests next to them. New IO goes behind the existing traits in
  `adapters`. New end-to-end behavior gets a `tests/cli.rs` case driven through
  the real adapters against the mock server.
- Dependencies must be MIT/Apache-2.0 (or compatible permissive) only — no
  GPL-family crates.

## Safety / do-nots

- Never fabricate a "latest version" when a source fails — surface the error and
  exit non-zero.
- Never install `pwsh` from scratch, never manage Windows PowerShell 5.1, never
  bundle a PowerShell binary.
- Never self-elevate or modify the host privilege configuration; surface the
  requirement instead.
- `--check` must never run a package-manager process or perform any
  state-changing / network-write side effect.
- Do not commit secrets or credentials. Do not let README and AGENTS.md
  contradict each other.

## Where to look first

- Product behavior, usage, exit codes: [README.md](README.md)
- Product contract, Constitution, FR registry: [docs/PRD.md](docs/PRD.md)
- Design decisions: [docs/adr/](docs/adr/) (esp. ADR-0001..0005)
- Orchestration flow and exit-code mapping: `src/app.rs`
- Pure rules to extend: `src/core/`
