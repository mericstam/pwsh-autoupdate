# System Design — pwsh-autoupdate (increment 001)

> Authored by the architect during ARCHITECT, from `prd.md` (FR-1..FR-13) and `tasks.yaml`,
> conforming to the `rust-cli` archetype. This document is implementation-guiding: it fixes the
> module map, the trait seams, the cross-boundary data types, the per-OS detection and upgrade
> tables, the exit-code contract, and the test-seam strategy. It is not the source code; the coder
> implements from here. Stack and pinned dependency versions are in ADR-0005.

## 1. Architectural shape — core / adapter / host

The crate is a single Cargo **binary** (edition 2021, stable). The load-bearing rule (from the
archetype) is a three-layer split where the dependency arrows point **inward**: the host depends on
adapters and core; adapters depend on core; the **core depends on nothing outside `std` + a few
pure crates** (`serde`, `semver`, `thiserror`).

```
            +-------------------- HOST (cli.rs, main.rs) --------------------+
            |  clap parse -> build real adapters -> orchestrate -> exit code  |
            +------------------------------+--------------------------------+
                                           | calls (via &dyn traits)
            +--------------- ADAPTERS (adapters/*.rs) ----------------------+
            |  HttpClient (ureq)   CommandRunner (Command)   probe (fs/cfg) |
            |  + version-resolve orchestration (HTTP -> semver)             |
            +------------------------------+--------------------------------+
                                           | passes PLAIN VALUES
            +----------------- CORE (core/*.rs) — PURE, NO IO --------------+
            |  version | detect | plan | report | error                     |
            |  takes structs/strings, returns structs. No HTTP/Command/env. |
            +---------------------------------------------------------------+
```

**Boundary invariant (enforced, CI-checkable):** no file under `src/core/` may import `ureq`,
`std::process::Command`, `std::env`, `std::fs`, or perform any network / subprocess / environment
probing on its public surface. The core receives already-probed/already-fetched plain values and
returns plain values. All real IO lives in `adapters/` and `main.rs`. This is what makes every rule
unit-testable with zero network and zero subprocesses (FR-10, G-5).

## 2. Module map (concrete files under `src/`)

Matches the `tasks.yaml` task→path coverage exactly.

| File | Layer | Responsibility | Task |
|------|-------|----------------|------|
| `src/main.rs` | host | thin entry: parse CLI, build real adapters, run the chosen flow, map `Result`→exit code. No domain logic. | scaffold, cli-wiring-check, cli-wiring-update |
| `src/cli.rs` | host | `clap` `Parser` derive: default-run path, `--check`, `--verbose`. Parsing only; hands typed args to the app layer. | cli-args |
| `src/lib.rs` | host | library root: `pub mod core; pub mod adapters; pub mod cli;` + the orchestration entry points (`run_check`, `run_update`) so the binary stays thin and tests drive the library with fakes. | scaffold |
| `src/core/mod.rs` | core | re-exports the core modules and the shared cross-boundary types (`InstallMethod`, `DetectionSignals`, `Detection`, `UpdatePlan`, `ReleaseInfo`, `VersionState`, `Os`). | every core task |
| `src/core/error.rs` | core | `thiserror` typed errors: `SourceError`, `DetectError`, `PlanError`, and a top `CoreError`. Carries the "never fabricate a version" semantics (a source failure is representable with no latest-version value). | error-core |
| `src/core/version.rs` | core | semver parse + the pure `classify(current, latest) -> VersionState` decision incl. ADR-0003 prerelease rule. | version-core |
| `src/core/detect.rs` | core | pure detection rules over `DetectionSignals` + ADR-0004 precedence → `Detection { selected, also_detected }`. | detect-rules-core |
| `src/core/plan.rs` | core | pure `build_plan(method, from, to, os) -> UpdatePlan` incl. the per-OS+method command table and the `requires_elevation` flag (FR-12). | plan-core |
| `src/core/report.rs` | core | `--check` report model + `Display`: current, method (+ also-detected), latest, would-run command; undetermined-method case. | report-core |
| `src/adapters/mod.rs` | adapter | re-exports adapters + the `version-resolve` orchestration (`resolve_latest_stable(&dyn HttpClient) -> Result<ReleaseInfo, SourceError>`). | scaffold, version-resolve |
| `src/adapters/http.rs` | adapter | `HttpClient` trait + `RealHttp` (ureq) + serde structs for GitHub Releases / build-info. | http-adapter |
| `src/adapters/runner.rs` | adapter | `CommandRunner` trait + `RealRunner` (`std::process::Command`) + `CmdOutput`. | runner-adapter |
| `src/adapters/probe.rs` | adapter | OS/fs probing (cfg-gated) → emits plain `DetectionSignals` + locates/queries `pwsh`. | probe-adapter |
| `tests/cli.rs` | test | integration tests: `assert_cmd` drives the binary; library-level tests with fakes + `mockito`. | scaffold, hermetic-tests |

## 3. The two trait seams

These are the only two outside-world dependencies the orchestration touches; both sit behind traits
so tests inject fakes. (A third seam, the OS probe, is cfg-gated and produces plain
`DetectionSignals` consumed by the pure core; it is exercised through the real binary and, where
practical, via temp-dir fixtures.)

### 3.1 `HttpClient` (`src/adapters/http.rs`)

```rust
pub trait HttpClient {
    /// GET the URL and parse the body as JSON. Network/HTTP/parse failures map to SourceError.
    fn get_json(&self, url: &str) -> Result<serde_json::Value, SourceError>;
    /// GET the URL as text (build-info feed). Same failure mapping.
    fn get_text(&self, url: &str) -> Result<String, SourceError>;
}
pub struct RealHttp { /* ureq::Agent with a set User-Agent (GitHub requires one) */ }
impl HttpClient for RealHttp { /* ... */ }
```

- Real impl: `ureq` blocking agent. Sets a `User-Agent` header (GitHub API rejects requests
  without one). No retry/async.
- No hardcoded latest-version fallback anywhere — a failure surfaces as `SourceError` and the run
  exits with code 2 (FR-2, FR-11).
- Test impl: `FakeHttp` holding a `url -> canned response/error` map (or `mockito` server) returning
  recorded GitHub-Releases / build-info fixtures.

### 3.2 `CommandRunner` (`src/adapters/runner.rs`)

```rust
pub struct CmdOutput { pub status: i32, pub stdout: String, pub stderr: String }

pub trait CommandRunner {
    /// Run a program with args; capture status+stdout+stderr. IO failure -> PlanError/anyhow context.
    fn run(&self, program: &str, args: &[&str]) -> Result<CmdOutput, std::io::Error>;
    /// Is the program resolvable on PATH? Used before running a plan's command (FR-6).
    fn exists(&self, program: &str) -> bool;
}
pub struct RealRunner;
impl CommandRunner for RealRunner { /* std::process::Command */ }
```

- Real impl: `std::process::Command`, confined to the adapter layer — never called from core.
- A non-zero `status` is a surfaced failure (host maps to non-zero exit; never reports success).
- Test impl: `FakeRunner` that **records every `(program, args)` invocation** and returns canned
  `CmdOutput`. The critical `--check` assertion is "zero mutating runner calls recorded" (G-3).
  `exists()` is backed by a configurable set so tests can model "manager off PATH" (FR-6, Section 7).

## 4. Data types that cross the boundary

All defined in `core/` (re-exported from `core/mod.rs`) so they are pure and OS-agnostic, then
populated by adapters and consumed by host/report.

```rust
// Which OS we are running on (resolved by the host; passed into pure code so rules unit-test
// identically on every platform).
pub enum Os { Windows, Macos, Linux }

// Every supported install channel across all OSes (FR-3/4/5).
pub enum InstallMethod {
    Winget, Msix, Msi, Chocolatey,          // Windows
    Homebrew, MacPkg,                        // macOS
    AptDpkg, DnfRpm, Snap, PortableTarGz,    // Linux
}

// Plain signals gathered by probe.rs; the pure detect rules consume ONLY this (no env access).
pub struct DetectionSignals {
    pub os: Os,
    pub pwsh_present: bool,
    pub pwsh_path: Option<String>,           // e.g. /usr/bin/pwsh, C:\...\pwsh.exe
    pub current_version: Option<String>,     // raw `pwsh --version` output, parsed later
    // per-channel ownership hints, each "did this manager report owning pwsh?" :
    pub winget_lists_pwsh: bool,
    pub msix_registered: bool,
    pub msi_registered: bool,
    pub choco_lists_pwsh: bool,
    pub brew_lists_pwsh: bool,
    pub pkg_receipt_present: bool,
    pub dpkg_owns_pwsh: bool,
    pub rpm_owns_pwsh: bool,
    pub snap_lists_pwsh: bool,
    pub portable_install_dir: Option<String>, // set when pwsh is a portable tar.gz unpack
    // which manager executables exist on PATH (for the exists()-gating + reporting):
    pub available_managers: Vec<InstallMethod>,
}

// Result of pure detection.
pub struct Detection {
    pub selected: Option<InstallMethod>,     // None == undetermined (FR-3/4/5 undetermined branch)
    pub also_detected: Vec<InstallMethod>,   // others present, for reporting (FR-9)
}

// Resolved latest release (from version-resolve; never fabricated on failure — FR-11).
pub struct ReleaseInfo {
    pub version: semver::Version,            // latest STABLE, parsed
    pub tag: String,                         // raw GitHub tag, e.g. "v7.4.6"
}

// Pure semver decision (FR-8, ADR-0003).
pub enum VersionState { UpToDate, UpdateAvailable }

// The plan — built once, drives BOTH --check report and the real update (FR-9 agreement).
pub struct UpdatePlan {
    pub method: InstallMethod,
    pub from: semver::Version,
    pub to: semver::Version,
    pub program: String,                     // e.g. "winget"
    pub args: Vec<String>,                   // e.g. ["upgrade","--id","Microsoft.PowerShell", ...]
    pub requires_elevation: bool,            // FR-12: host surfaces this before/at execution
}
```

## 5. Per-OS detection signal sources (probe.rs → DetectionSignals)

`probe.rs` is `cfg`-gated per OS; it emits the plain `DetectionSignals` above. The pure rules in
`detect.rs` never see the environment.

| OS | Channel | Signal source the probe collects |
|----|---------|----------------------------------|
| Windows | winget | `winget list --id Microsoft.PowerShell` returns the package (and `pwsh` path under winget links / `%LOCALAPPDATA%\Microsoft\WindowsApps`). |
| Windows | MSIX / Store | `pwsh_path` under `%ProgramFiles%\WindowsApps\Microsoft.PowerShell_*` or an MSIX/AppX package registration. |
| Windows | MSI | MSI product registration for PowerShell (uninstall registry key / `pwsh_path` under `%ProgramFiles%\PowerShell\7`). |
| Windows | Chocolatey | `choco list --local-only powershell-core` lists it (and `pwsh` under the Chocolatey lib/shim path). |
| macOS | Homebrew | `brew list --formula` (or `brew --prefix`) shows `powershell`, or `pwsh_path` resolves under the brew prefix (`/opt/homebrew` or `/usr/local`). |
| macOS | `.pkg` | a PowerShell pkg receipt (`pkgutil --pkgs` matching `com.microsoft.powershell`), or `pwsh_path` under `/usr/local/microsoft/powershell`. |
| Linux | apt/dpkg | `dpkg -S $(which pwsh)` / `dpkg -l powershell` reports ownership. |
| Linux | dnf/rpm | `rpm -qf $(which pwsh)` / `rpm -q powershell` reports ownership. |
| Linux | snap | `snap list powershell` lists it (and `pwsh_path` under `/snap/`). |
| Linux | portable tar.gz | `pwsh` is NOT owned by any system pkg manager and lives in a portable dir (e.g. `/opt/microsoft/powershell/7` unpacked by hand) → `portable_install_dir` set. Lowest precedence (ADR-0004). |

Probe also sets `pwsh_present=false` when no `pwsh`/`pwsh.exe` is found, so the host can report
"not installed" and exit non-zero without installing (ADR-0001, Section 7). Executable name is
`pwsh.exe` on Windows, `pwsh` elsewhere — use `std::env::consts::EXE_SUFFIX` / cfg, never hardcode.

## 6. Per-OS + method upgrade command table (plan.rs)

`plan.rs` is pure: it maps `(method, os)` to the exact `program` + `args` and the elevation flag.
The command string is built here so it is asserted in unit tests; the adapter only *executes* it.

| OS | Method | program | args | requires_elevation |
|----|--------|---------|------|--------------------|
| Windows | Winget | `winget` | `upgrade --id Microsoft.PowerShell --silent --accept-source-agreements --accept-package-agreements` | no (per-user) — true if machine-scope |
| Windows | Chocolatey | `choco` | `upgrade powershell-core -y` | yes (admin shell) |
| Windows | MSIX / Store | `winget` | `upgrade --id Microsoft.PowerShell --source msstore --silent ...` (Store updates flow through winget/Store) | no |
| Windows | MSI | `winget` | `upgrade --id Microsoft.PowerShell ...` (MSI channel is upgraded via the MSI/winget package; documented as the supported MSI upgrade) | yes |
| macOS | Homebrew | `brew` | `upgrade powershell` | no |
| macOS | `.pkg` | (download + `installer`) | the `.pkg` replace procedure: fetch the latest `.pkg` asset, then `installer -pkg <file> -target /` | yes (`installer -target /`) |
| Linux | apt/dpkg | `apt-get` | `install --only-upgrade -y powershell` | yes (root) |
| Linux | dnf/rpm | `dnf` | `upgrade -y powershell` | yes (root) |
| Linux | snap | `snap` | `refresh powershell` | yes (root) |
| Linux | portable tar.gz | (download + replace) | download the latest `tar.gz` asset and replace the contents of `portable_install_dir` (download-and-replace per A-6) | depends on dir ownership |

Notes for the coder:
- The MSIX and MSI rows route their upgrade through the supported package channel; the precedence in
  ADR-0004 still selects exactly one method, and the plan's command must match the selected method.
- For `.pkg` and portable tar.gz, the "command" is a small fixed procedure, not a single manager
  invocation; model it as a `program`+`args` where the program is the OS tool that performs the step
  (`installer`, or a replace step), and keep the *asset URL* resolution in the adapter, not core.
- `requires_elevation` is a static property of the (method, os) pair (e.g. anything writing system
  paths or running root package managers). The host checks it and surfaces the requirement (FR-12);
  the tool never self-elevates.

## 7. Orchestration flow (host, in `lib.rs` / `main.rs`)

Two entry points, both taking `&dyn HttpClient` + `&dyn CommandRunner` so tests inject fakes:

```
run_check(http, runner, os):
  signals   = probe(runner, os)            // adapter
  if !signals.pwsh_present -> error "not installed", exit 2
  current   = parse_version(signals.current_version)?   // core
  detection = detect::resolve(signals)     // core (ADR-0004 precedence)
  latest    = resolve_latest_stable(http)? // adapter; FR-11: error -> exit 2, no version printed
  state     = version::classify(current, latest.version)   // core
  plan      = detection.selected.map(|m| plan::build_plan(m, current, latest.version, os)) // core
  report    = report::build(current, detection, latest, plan)  // core
  print(report)                            // NO runner.run() — zero side effects (G-3)
  exit code: UpToDate -> 0, UpdateAvailable -> 1, (any error above) -> 2   // ADR-0002

run_update(http, runner, os):
  ... same probe/detect/resolve/classify ...
  if OS unsupported -> exit non-zero naming it
  if state == UpToDate -> exit 0 (no-op)
  selected method? else report undetermined, no update
  plan = plan::build_plan(...)
  if plan.requires_elevation && !have_privileges -> surface requirement, exit non-zero  // FR-12
  if !runner.exists(plan.program) -> "manager <X> not found", exit non-zero, no other channel // FR-6
  out = runner.run(plan.program, &plan.args)?       // the ONLY mutating call
  if out.status != 0 -> surface stderr, exit non-zero (never report success)  // FR-6
  exit 0
```

## 8. Exit-code contract (ADR-0002)

| Mode | Code | Meaning |
|------|------|---------|
| `--check` | 0 | up to date |
| `--check` | 1 | update available |
| `--check` | 2 | error (network/API/parse failure, or undetermined-state error) |
| full run | 0 | success (including up-to-date no-op) |
| full run | non-zero | failure (manager non-zero, manager missing, OS unsupported, pwsh absent, elevation required) |

`main.rs` is the single place that maps the orchestration `Result`/state to `std::process::exit`.
Documented in the README (FR-13). Never `unwrap`/`expect` on IO — convert to `Result`, print a
readable message to stderr, map to the non-zero code.

## 9. Test seam strategy (FR-10, G-5)

- **Unit tests (in `core/`)** — no network, no subprocess:
  - `version`: equal / greater / less / prerelease-vs-stable cases (FR-8, ADR-0003).
  - `detect`: one fixture `DetectionSignals` per channel returns exactly that method; none→undetermined;
    multi-method→ADR-0004 winner + also-detected list (FR-3/4/5/9).
  - `plan`: each (os, method) yields the exact program+args and correct `requires_elevation` (FR-6/12).
  - `report`: `Display` of the four fields incl. the undetermined-method branch (FR-7).
- **Adapter / orchestration tests with fakes:**
  - `FakeHttp` (or `mockito` server) returns recorded GitHub-Releases + build-info JSON; assert
    `resolve_latest_stable` parses the right semver, and that a failure fixture yields a `SourceError`
    with no fabricated version (FR-2, FR-11).
  - `FakeRunner` records every `(program,args)` and returns canned `CmdOutput`; assert the chosen plan
    and — critically — that `run_check` records **zero** mutating runner calls (G-3).
  - Model "manager off PATH" via `FakeRunner::exists` returning false → host names it, exits non-zero (FR-6).
- **CLI integration (`tests/cli.rs`) with `assert_cmd` + `predicates`:** `--help`/`--version`, and a
  `--check` path against a `mockito` fixture server asserting stdout fields + exit code. This catches
  "feature implemented but never wired into the CLI."
- **Never** hit the live GitHub API or spawn a real package manager in tests. Coverage measured with
  `cargo llvm-cov` (target ≥ 70% line coverage on `core/`); if unavailable, report the gap.

## 10. Cross-OS notes

- `pwsh.exe` (Windows) vs `pwsh`; build paths with `Path`/`PathBuf::join`, never literal separators.
- All OS-specific probing gated behind `cfg(target_os = ...)`; the **rules** in `detect.rs`/`plan.rs`
  stay pure and OS-agnostic (they take `Os` + `DetectionSignals` as data) so the same core compiles
  and unit-tests identically on all three platforms.
- CI matrix: ubuntu / macos / windows, running `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings`, `cargo build --release`, `cargo test` (see ADR-0005 + `packaging-ci`).

## 11. ADR references

- ADR-0001 — update-existing-only (absent pwsh → report + exit non-zero; no bundled binaries).
- ADR-0002 — exit-code contract (§8).
- ADR-0003 — stable-only prerelease policy (`version.rs` classify).
- ADR-0004 — multi-method precedence (`detect.rs` resolution; §6 selection).
- ADR-0005 — technology stack & pinned dependencies (this increment; see `docs/adr/0005-*`).
