# pwsh-autoupdate — Product Requirements

This document is the materialized product contract for pwsh-autoupdate. It opens
with a durable, increment-independent **Constitution** (product principles,
boundaries, and the FR registry), followed by the full PRD for the first
increment (increment 001).

---

# Constitution

The Constitution captures the durable product principles that hold across every
increment. Individual increment PRDs may add functional requirements but must not
contradict these principles.

## Product vision

One binary, run anywhere PowerShell runs, that tells you whether your `pwsh` is
out of date and upgrades it correctly through the channel that installed it —
without you having to remember the right per-channel incantation.

## Durable principles

1. **Scope = update-existing-only.** pwsh-autoupdate updates an already-installed
   `pwsh`. It never installs PowerShell from scratch; an absent `pwsh` is
   reported, not installed. (ADR-0001)
2. **Honest failure / no fabricated version.** When a source cannot be fetched or
   parsed, the tool surfaces the error and exits non-zero. It never prints or
   invents a "latest version", and never reports an update as successful when it
   was not. (FR-2, FR-11)
3. **Cross-platform by construction.** A single Rust codebase compiles to a
   standalone binary for Windows, macOS, and Linux. OS-specific behavior is
   internal `#[cfg]`/data branching — never separate source trees.
4. **`--check` has no side effects.** The dry-run path executes no
   package-manager process and performs no state-changing or network-write side
   effect. Its exit code is a machine contract (ADR-0002): 0 up-to-date, 1
   update-available, 2 error.
5. **MIT/Apache-only dependencies.** All dependencies are MIT or Apache-2.0 (or
   compatibly permissive). No GPL-family libraries. The product itself is dual
   licensed `MIT OR Apache-2.0`.

## Product-wide boundaries & Non-Goals

- Not a PowerShell module or `.ps1` script — a standalone Rust binary. (ADR-0001)
- Does not bundle, vendor, or embed any PowerShell binary.
- Does not install PowerShell from scratch.
- Does not manage Windows PowerShell 5.1 (Desktop edition); only cross-platform
  `pwsh` (6+/Core).
- No npm-based update path.
- No GUI / TUI / web interface — CLI only.
- No self-update, no scheduling/daemon mode, no telemetry, no rollback/downgrade.
- Never self-elevates; surfaces a privilege requirement instead.
- Never touches a non-detected channel; never silently falls back to another
  channel.

## FR registry

High-water mark: **FR-13**. All requirements below are `active` (first
increment, no supersessions or removals).

| FR | Title | Status | Increment |
|----|-------|--------|-----------|
| FR-1 | Single cross-OS binary | active | 001 |
| FR-2 | Resolve latest stable PowerShell version | active | 001 |
| FR-3 | Detect Windows install method | active | 001 |
| FR-4 | Detect macOS install method | active | 001 |
| FR-5 | Detect Linux install method | active | 001 |
| FR-6 | Perform update via the detected manager | active | 001 |
| FR-7 | `--check` / dry-run reporting | active | 001 |
| FR-8 | Semantic version comparison | active | 001 |
| FR-9 | Multiple-install-method resolution | active | 001 |
| FR-10 | Test suite with mocked HTTP and process execution | active | 001 |
| FR-11 | Never fabricate a version on source failure | active | 001 |
| FR-12 | Privilege / permission handling for upgrades | active | 001 |
| FR-13 | Ship AGENTS.md and README | active | 001 |

## Increment log

| Increment | Slug | FRs added | High-water |
|-----------|------|-----------|------------|
| 001 | autoupdate-detect-and-update | FR-1 … FR-13 | 13 |

## Requirement ADRs

- ADR-0001 — Update-existing-only scope (no install-from-scratch)
- ADR-0002 — `--check` exit-code contract (0/1/2)
- ADR-0003 — Prerelease handling policy: stable-only by default
- ADR-0004 — Multiple-install-method resolution precedence
- ADR-0005 — Technology stack & dependencies

---

# PRD — pwsh-autoupdate (increment 001)

> Product Requirements Document — the contract for the first increment of pwsh-autoupdate. Greenfield: FR ids start at FR-1.

## 1. Overview *(core)*

pwsh-autoupdate is a single-binary command-line tool, written in Rust, for developers and operators who keep PowerShell (the cross-platform `pwsh`) current across Windows, macOS, and Linux. It detects how the existing `pwsh` was installed (the owning package manager or installer), resolves the latest stable PowerShell release for the current OS from official sources, compares versions with semantic-versioning rules, and updates PowerShell by delegating to the same package manager that installed it. A `--check` dry-run reports the situation and the exact command it would run without changing anything. It exists now because PowerShell ships through many channels per OS and users currently must remember the right per-channel upgrade incantation by hand.

## 2. Problem Statement *(core)*

A user with PowerShell already installed cannot tell, from one command, whether their `pwsh` is out of date or how to upgrade it correctly. PowerShell is distributed through at least nine channels across three operating systems (winget, Microsoft Store/MSIX, MSI, Chocolatey on Windows; Homebrew and `.pkg` on macOS; apt/dpkg, dnf/rpm, snap, and portable tar.gz on Linux), and each channel has its own upgrade procedure. Using the wrong procedure (e.g. running an MSI over a winget install, or `brew upgrade` on a `.pkg` install) leaves duplicate or broken installs. Today this requires the user to know which channel provisioned their `pwsh` and to recall that channel's exact upgrade command, per machine and per OS.

## 3. Goals & Success Metrics *(core)*

| # | Goal | Metric / target | How verified |
|---|---|---|---|
| G-1 | One binary runs the full detect→check→update flow on all three OSes from one codebase | Crate builds and the CLI runs on Windows, macOS, and Linux targets with no per-OS source forks (`#[cfg]` branching allowed) | `cargo build` succeeds for each target triple; CI matrix or manual run on each OS |
| G-2 | Correctly identify the install method that owns the current `pwsh` | For each supported channel, given a fixture representing that channel's environment, detection returns exactly that channel | `cargo test` detection cases, one per channel in FR-3..FR-5, all pass |
| G-3 | Report accurately in `--check` with zero side effects | For ≥1 fixture per OS, `--check` prints current version, detected method, latest version, and the exact upgrade command, and executes no process and no network write | `cargo test` asserts no upgrade command was invoked via the mocked process executor; output fields asserted |
| G-4 | Never invent a version when sources fail | On simulated GitHub API / build-info failure, the tool exits non-zero with an error and prints no "latest version" value | `cargo test` API-failure case asserts non-zero exit and no fabricated version string |
| G-5 | Tests are hermetic | The whole `cargo test` suite passes with no real network access and no real package-manager invocation | Run `cargo test` with networking disabled (mocked HTTP, mocked process execution); suite is green |

## 4. Non-Goals / Out of Scope *(core)*

- **Not a PowerShell module or `.ps1` script.** This is a standalone Rust binary; it will not be delivered as a PowerShell module, function, or script. (ADR-0001)
- **Does not bundle, vendor, or embed PowerShell binaries.** The actual upgrade is delegated to the host package manager; the tool ships no `pwsh` payload. (ADR-0001)
- **Does not install PowerShell from scratch.** v1 only updates an existing `pwsh`; a machine with no PowerShell installed is out of scope and is reported as such, not installed. (ADR-0001)
- **No GUI.** CLI only for v1 — no graphical, TUI, or web interface.
- **Does not manage Windows PowerShell 5.1** (the in-box `powershell.exe` / Desktop edition). Only the cross-platform `pwsh` (PowerShell 6+/Core) is in scope.
- **No npm-based update path.** npm is not an official PowerShell distribution channel and is not detected or used.
- **No self-update of pwsh-autoupdate itself**, no scheduling/daemon mode, no telemetry, and no rollback/downgrade — none are requested for v1.

## 5. Users & Personas *(optional)*

Single persona: a developer or operator on their own workstation or a server, comfortable with a terminal, with `pwsh` already installed and the relevant package manager available. Primary journey: run the tool (optionally with `--check`), read whether an update is available and how it would be applied, then either let the tool apply it or run `--check` first and apply later.

## 6. Functional Requirements *(core)*

### FR-1: Single cross-OS binary — `must`

**Story:** As an operator managing mixed fleets, I want one tool that works the same on Windows, macOS, and Linux so that I do not learn three tools.

**Acceptance criteria:**
- The system SHALL be built from a single Rust codebase that compiles to a standalone executable for Windows, macOS, and Linux.
- WHEN the binary is run on any supported OS, the system SHALL execute the detect, check, and (unless `--check`) update flow appropriate to that OS without requiring a separate per-OS build of the source.
- WHERE OS-specific behavior is required, the system SHALL branch internally (e.g. `#[cfg(target_os)]`) rather than ship separate source trees.
- IF the tool is run on an OS it does not support, THEN the system SHALL exit non-zero with a message naming the unsupported OS and SHALL take no further action.

### FR-2: Resolve latest stable PowerShell version — `must`

**Story:** As a user, I want the tool to learn the current latest stable PowerShell release so that it can tell me whether I am behind.

**Acceptance criteria:**
- WHEN resolving the latest version, the system SHALL query the GitHub Releases API at `https://api.github.com/repos/PowerShell/PowerShell/releases/latest` and SHALL parse the release tag into a semantic version.
- The system SHALL also consult the official build-info metadata at `https://aka.ms/pwsh-buildinfo-stable` as the stable-channel reference.
- The system SHALL treat the resolved latest as the stable release only (prerelease policy per FR-8 / ADR-0003).
- IF a network or HTTP error occurs while resolving the latest version, THEN the system SHALL surface the error and exit non-zero, and SHALL NOT report or fabricate any "latest version" value (see FR-11).
- IF the responses cannot be parsed into a version, THEN the system SHALL surface a parse error and exit non-zero.

### FR-3: Detect Windows install method — `must`

**Story:** As a Windows user, I want the tool to know which channel installed my `pwsh` so that it upgrades through the right one.

**Acceptance criteria:**
- WHILE running on Windows, the system SHALL attempt to detect the install method that owns the current `pwsh` among: winget (`Microsoft.PowerShell`), MSIX / Microsoft Store, MSI, and Chocolatey.
- WHEN exactly one method is detected, the system SHALL select that method as the upgrade channel.
- WHEN more than one method is detected, the system SHALL resolve to a single method per the precedence policy in FR-9 (ADR-0004).
- IF no supported install method can be determined for an existing `pwsh`, THEN the system SHALL report the install method as undetermined and SHALL NOT attempt an update (it MAY still report current and latest version).

### FR-4: Detect macOS install method — `must`

**Story:** As a macOS user, I want the tool to know whether Homebrew or the `.pkg` installer provisioned my `pwsh`.

**Acceptance criteria:**
- WHILE running on macOS, the system SHALL attempt to detect the install method among: Homebrew (`brew`) and the direct `.pkg` installer.
- WHEN exactly one method is detected, the system SHALL select that method as the upgrade channel.
- WHEN more than one method is detected, the system SHALL resolve to a single method per the precedence policy in FR-9 (ADR-0004).
- IF no supported install method can be determined, THEN the system SHALL report it as undetermined and SHALL NOT attempt an update.

### FR-5: Detect Linux install method — `must`

**Story:** As a Linux user, I want the tool to identify the package manager that owns my `pwsh` so that it upgrades cleanly.

**Acceptance criteria:**
- WHILE running on Linux, the system SHALL attempt to detect the install method that owns the `pwsh` binary among: apt/dpkg, dnf/rpm, snap, and a portable tar.gz install.
- WHEN exactly one method is detected, the system SHALL select that method as the upgrade channel.
- WHEN more than one method is detected, the system SHALL resolve to a single method per the precedence policy in FR-9 (ADR-0004).
- IF no supported install method can be determined, THEN the system SHALL report it as undetermined and SHALL NOT attempt an update.

### FR-6: Perform update via the detected manager — `must`

**Story:** As a user, I want the tool to upgrade PowerShell using the channel that installed it so that I do not end up with duplicate or broken installs.

**Acceptance criteria:**
- WHEN an update is requested (not `--check`), a supported install method is detected, and the latest version is greater than the current version, the system SHALL perform the update by invoking that method's upgrade command (e.g. `winget upgrade Microsoft.PowerShell`, `choco upgrade powershell-core`, `brew upgrade powershell`, `apt-get install --only-upgrade powershell`, `dnf upgrade powershell`, `snap refresh powershell`, or the portable-archive replacement procedure for tar.gz).
- WHEN the update command completes successfully, the system SHALL exit zero.
- IF the upgrade command exits non-zero, THEN the system SHALL surface the manager's failure and exit non-zero, and SHALL NOT report the update as successful.
- IF the detected manager's executable is not found on `PATH`, THEN the system SHALL exit non-zero with a message naming the missing manager and SHALL NOT attempt any other channel.
- WHERE the upgrade requires elevated privileges (FR-12), the system SHALL surface the privilege requirement rather than silently failing.

### FR-7: `--check` / dry-run reporting — `must`

**Story:** As a cautious user, I want to see what would happen before anything changes so that I can decide whether to proceed.

**Acceptance criteria:**
- WHEN invoked with `--check` (dry-run), the system SHALL report: the current installed `pwsh` version, the detected install method, the latest available version, and the exact command it would run to update.
- WHILE in `--check` mode, the system SHALL NOT execute any package-manager process and SHALL NOT perform any state-changing or network-write side effect.
- The system SHALL signal whether an update is available via its exit code per the contract in ADR-0002 (update-available is a distinct, documented non-zero code; up-to-date is zero).
- IF the install method is undetermined in `--check` mode, THEN the system SHALL still report current and latest version and SHALL state that no upgrade command can be produced.

### FR-8: Semantic version comparison — `must`

**Story:** As a user, I want the tool to compare versions correctly so that it never offers a downgrade or a false update.

**Acceptance criteria:**
- WHEN comparing the current and latest versions, the system SHALL use semantic-versioning (semver) precedence rules.
- WHEN the latest stable version is greater than the current version, the system SHALL classify the state as "update available".
- WHEN the current version is greater than or equal to the latest stable version, the system SHALL classify the state as "up to date" and SHALL NOT attempt an update.
- WHERE the current installed version is a prerelease and the latest stable equals its release version, the system SHALL apply the prerelease policy of ADR-0003 (stable-only by default; a newer stable supersedes an installed prerelease of the same release number).

### FR-9: Multiple-install-method resolution — `must`

**Story:** As a user whose machine shows more than one install channel, I want deterministic, explained behavior so that the tool's choice is predictable.

**Acceptance criteria:**
- WHEN more than one supported install method is detected for the current `pwsh`, the system SHALL select exactly one according to the per-OS precedence order defined in ADR-0004.
- WHEN it resolves a multi-method situation, the system SHALL report which method it selected and note that others were also detected.
- The system SHALL apply the same precedence in both `--check` and update modes so that the reported command matches the command that would run.

### FR-10: Test suite with mocked HTTP and mocked process execution — `must`

**Story:** As a maintainer, I want hermetic tests so that the detection, comparison, and planning logic is verified without touching the network or real package managers.

**Acceptance criteria:**
- The system SHALL include a `cargo test` suite covering: semantic version comparison, install-method detection for each supported channel, and update-plan production (which command would run for a given OS + method + version delta).
- WHILE tests run, the system SHALL use mocked HTTP for the GitHub API and build-info sources and a mocked process executor for package-manager calls — no real network access and no real process spawning.
- The test suite SHALL include at least the edge cases enumerated in Section 7.

### FR-11: Never fabricate a version on source failure — `must`

**Story:** As a user, I want errors surfaced honestly so that I never act on a made-up "latest version".

**Acceptance criteria:**
- IF resolving the latest version fails (network error, HTTP non-success, or unparseable payload), THEN the system SHALL print an error describing the failure and exit non-zero.
- IF latest-version resolution failed, THEN the system SHALL NOT print any value in the "latest version" field and SHALL NOT proceed to an update.

### FR-12: Privilege / permission handling for upgrades — `must`

**Story:** As a user, I want clear behavior when an upgrade needs elevation so that failures are understandable, not silent.

**Acceptance criteria:**
- IF the detected manager's upgrade requires elevated privileges and the current process lacks them, THEN the system SHALL surface the privilege requirement (e.g. "this upgrade requires administrator/root privileges") and exit non-zero rather than silently failing.
- The system SHALL NOT attempt to self-elevate or modify the host's privilege configuration in v1.

### FR-13: Ship AGENTS.md and README — `must`

**Story:** As a contributor and as a user, I want a README and an AGENTS.md so that usage and contribution conventions are documented.

**Acceptance criteria:**
- The delivered project SHALL include a `README` documenting installation, the supported OSes and channels, `--check` vs update usage, and exit-code meanings (ADR-0002).
- The delivered project SHALL include an `AGENTS.md` documenting agent/build conventions for the repository.

## 7. Edge Cases & Unwanted Behavior *(optional)*

- IF `pwsh` is absent from the machine, THEN the system SHALL report that PowerShell is not installed and exit non-zero, and SHALL NOT attempt to install it (out of scope, ADR-0001).
- IF the detected manager is not on `PATH`, THEN the system SHALL exit non-zero naming the missing manager and SHALL NOT silently fall back to another channel (FR-6).
- WHEN the installed version is already greater than or equal to the latest stable, the system SHALL report "up to date" and perform no update (FR-8).
- WHERE the latest GitHub release is a prerelease, the system SHALL ignore it for the default stable channel and use the latest stable per build-info (ADR-0003).
- WHEN multiple install methods are present, the system SHALL resolve to one per ADR-0004 and report the others as also-detected (FR-9).
- IF the GitHub API or build-info endpoint fails or returns an unparseable body, THEN the system SHALL surface the error and exit non-zero without fabricating a version (FR-11).
- IF an upgrade needs privileges the process lacks, THEN the system SHALL surface the requirement and exit non-zero (FR-12).

## 8. Constraints & Assumptions *(core)*

- **Stack:** Rust (stable toolchain, 2021 edition), Cargo. Shipped crates (ADR-0005): `clap` (CLI), `serde`/`serde_json` (metadata parsing), `ureq` (HTTP client), `semver` (comparison), `anyhow`/`thiserror` (errors); `mockito` + `assert_cmd`/`predicates`/`tempfile` for hermetic tests. Package managers (winget, choco, brew, apt/dpkg, dnf/rpm, snap) are runtime dependencies invoked as external processes, not linked libraries.
- **Dependencies/licensing:** MIT / Apache-2.0 only. No GPL-family libraries. No paid-only SaaS.
- **Non-functional:** cross-platform (Win/macOS/Linux); no GUI; tests perform no real network access and spawn no real package-manager process (hermetic via mocked HTTP + mocked process execution).
- **Appetite:** ≤1,500,000 tokens and ≤3h wall-clock per run.
- **Assumptions:**
  - **A-1 (autonomous):** `--check` exit codes: 0 = up to date, a distinct documented non-zero code = update available, other non-zero = error. Concrete value fixed in ADR-0002.
  - **A-2 (autonomous):** Default channel is stable-only; prereleases are ignored unless an explicit opt-in flag is later added (out of scope for v1). Fixed in ADR-0003.
  - **A-3 (autonomous):** When multiple install methods are detected, a per-OS precedence order picks one; it is reported with the others noted. Fixed in ADR-0004.
  - **A-4 (autonomous):** "Latest" means the latest *stable* PowerShell release for the current OS/arch, sourced from the GitHub Releases API cross-checked with build-info.
  - **A-5 (autonomous):** Output is human-readable text on stdout for v1; no machine-readable (`--json`) format is required this increment.
  - **A-6 (autonomous):** The portable tar.gz "upgrade" on Linux is handled as a download-and-replace of the existing portable install location; it is detected when `pwsh` is not owned by a system package manager (ADR-0004 places it lowest precedence).
  - **A-7 (fail-soft):** WebSearch/WebFetch were unavailable to the PM in this headless run; the PRD is grounded in the intake's URLs and reference material rather than live domain research. No requirement depends on unverified facts.

## 9. Boundaries *(optional)*

- **Always:** detect before acting; in `--check` make zero side effects; surface source/manager errors honestly; report the exact command before/instead of running it.
- **Ask first:** N/A — non-interactive CLI; the `--check` mode is the "ask first" surface.
- **Never:** fabricate a version; install pwsh from scratch; manage Windows PowerShell 5.1; touch a non-detected channel; self-elevate privileges; bundle pwsh binaries.

## 10. Risks & Rabbit Holes *(optional)*

- **Install-method detection heuristics** are the main rabbit hole (especially Windows MSIX vs MSI vs winget, and Linux package-ownership vs portable). Mitigation: detection is fixture-driven and unit-tested per channel (FR-10); ambiguity resolves deterministically via ADR-0004.
- **Real upgrade execution across nine channels** cannot be fully integration-tested in CI. Mitigation: process execution is mocked in tests; the upgrade-command mapping is the tested unit, actual invocation is a thin shell-out.
- **build-info vs GitHub API drift.** Mitigation: stable channel defined as build-info-backed; GitHub release tag parsed as cross-check (FR-2).

## 11. UX Flows / Interface Sketch *(optional)*

CLI surface (v1):
- `pwsh-autoupdate` — detect, resolve latest, and update if behind.
- `pwsh-autoupdate --check` — dry-run report: current version, detected method, latest version, exact command; exit code signals update-available per ADR-0002.
- `pwsh-autoupdate --verbose` / `-v` — extra diagnostic detail on stderr (composable with `--check`).

Sample `--check` output (shape, not literal):
```
Current version: 7.4.1
Detected method: Homebrew (also detected: macOS .pkg)
Latest version:  7.4.6
Status:          update available
Would run: brew upgrade powershell
```

## 11b. Changelog & FR amendments

N/A — first increment.

## 12. Release Criteria *(core)*

- [ ] All `must` FRs pass their EARS acceptance criteria (FR-1 … FR-13).
- [ ] G-1 verified: builds and runs on Windows, macOS, Linux from one codebase.
- [ ] G-2 verified: per-channel detection tests pass (FR-3, FR-4, FR-5).
- [ ] G-3 verified: `--check` reports all four fields with zero side effects (FR-7).
- [ ] G-4 verified: source-failure case exits non-zero with no fabricated version (FR-11).
- [ ] G-5 verified: full `cargo test` suite green with no real network and no real process spawn (FR-10).
- [ ] Semver comparison, detection, and plan are covered by `cargo test` (FR-8, FR-10).
- [ ] `README` and `AGENTS.md` shipped and document exit codes per ADR-0002 (FR-13).
- [ ] Dependency licenses are all MIT/Apache-2.0 (no GPL-family).

## 13. Open Questions & Clarifications Log *(core)*

### Clarifications

| Round | Question | Answer / assumption | Source |
|---|---|---|---|
| — | Scope: does v1 install pwsh from scratch if absent? | No — update-existing-only; absent pwsh is reported, not installed. | autonomous assumption (headless) — intake Non-Goals |
| — | What exit code signals "update available" in `--check`? | 0 = up to date; a distinct documented non-zero code = update available; other non-zero = error. | autonomous assumption (headless) — ADR-0002 |
| — | How are prereleases handled? | Stable-only by default; prereleases ignored; no opt-in flag in v1. | autonomous assumption (headless) — ADR-0003 |
| — | When several install methods are detected, which wins? | Deterministic per-OS precedence order; selected one reported with others noted. | autonomous assumption (headless) — ADR-0004 |
| — | What does "latest" mean? | Latest stable release for current OS/arch, GitHub Releases API cross-checked with build-info. | autonomous assumption (headless) |
| — | Is machine-readable (`--json`) output required in v1? | No — human-readable text only for v1. | autonomous assumption (headless) |
| — | Behavior on source/API failure? | Surface error, exit non-zero, never fabricate a version. | autonomous assumption (headless) — intake goal + FR-11 |
| — | Behavior when an upgrade needs elevation? | Surface the privilege requirement and exit non-zero; never self-elevate. | autonomous assumption (headless) — FR-12 |

### Open questions

None in `must` scope. (All gaps resolved via autonomous assumptions above; assumptions mode: `allowed`.)

### Clarity run (ADR-0023)

Clarity run: 2 pass(es), 3 auto-fixes, 0 user-resolved, clean: yes.

### Requirement ADRs (ADR-0023)

- ADR-0001 — Update-existing-only scope (no install-from-scratch) (records the v1 scope-boundary clarification; Non-Goals)
- ADR-0002 — `--check` exit-code contract (records the update-available exit-code clarification; FR-7)
- ADR-0003 — Prerelease handling policy: stable-only by default (records the prerelease clarification; FR-8)
- ADR-0004 — Multiple-install-method resolution precedence (records the multi-method clarification; FR-9)

### Approval

Autonomous approval (headless, autonomous_assumptions allowed). Autonomous draft, assumptions mode: allowed, 7 assumptions logged (A-1 … A-7), 2026-06-29.
