# pwsh-autoupdate

A single-binary, cross-platform command-line tool that detects how the existing
cross-platform PowerShell (`pwsh`) was installed and updates it through the same
package manager that installed it.

PowerShell ships through many channels per operating system, and each channel
has its own upgrade procedure. Using the wrong procedure (for example running an
MSI over a winget install, or `brew upgrade` on a `.pkg` install) leaves
duplicate or broken installs. `pwsh-autoupdate` works out which channel owns your
`pwsh`, resolves the latest stable PowerShell release from official sources,
compares versions with semantic-versioning rules, and — when you ask it to —
upgrades through the owning channel.

## Install

### Homebrew (macOS, Linux)

```sh
brew install mericstam/tap/pwsh-autoupdate
```

### Scoop (Windows)

```powershell
scoop bucket add mericstam https://github.com/mericstam/scoop-bucket
scoop install mericstam/pwsh-autoupdate
```

Upgrade later with `scoop update pwsh-autoupdate`.

### From source (any platform with a Rust toolchain)

```sh
cargo install --git https://github.com/mericstam/pwsh-autoupdate
```

### Direct download

Download the archive for your platform from the
[latest release](https://github.com/mericstam/pwsh-autoupdate/releases/latest),
verify its `.sha256`, and put the binary on your `PATH`. Each release also
attaches a ready-to-use Homebrew formula (`pwsh-autoupdate.rb`) and Scoop
manifest (`pwsh-autoupdate.json`).

The binaries are not yet code-signed, so a **browser** download may be flagged by
the OS. A package manager above avoids this; for a direct download:

- **macOS:** clear the quarantine flag with `xattr -d com.apple.quarantine ./pwsh-autoupdate`
  (or right-click → Open once). Fetching the tarball with `curl`/`tar` instead of a
  browser usually avoids the flag entirely.
- **Windows:** Right-click the `.exe` → Properties → **Unblock**; or just run it from a
  terminal — SmartScreen's prompt targets double-clicked GUI apps, not command-line use.

## What it does

1. Probes the local `pwsh` to read its installed version.
2. Detects which package manager or installer owns that `pwsh` for the current
   OS.
3. Resolves the latest stable PowerShell release from official sources (the
   GitHub Releases API for PowerShell, cross-checked against the official
   stable build-info feed).
4. Compares the installed version against the latest stable using semver.
5. Either reports what it would do (`--check`) or performs the upgrade through
   the detected channel (default).

If PowerShell is **not installed**, the tool installs it from scratch via the
OS's native package manager — winget (then Chocolatey) on Windows, Homebrew on
macOS, snap on Linux — and `--check` reports the install command it would run.
It installs only when none is present, so it never creates a second, conflicting
install. On a host with no supported native manager it currently reports that it
cannot install (install PowerShell manually); a portable `~/.local` install for
that case is planned (ADR-0006). As with updates, it never self-elevates —
it surfaces an elevation requirement and stops.

It never fabricates a version: if a source cannot be fetched or parsed, it
surfaces the error and exits non-zero without printing any "latest version".

## Supported operating systems and channels

| OS | Detected install channels |
|----|---------------------------|
| Windows | winget (`Microsoft.PowerShell`), MSIX / Microsoft Store, MSI, Chocolatey |
| macOS | Homebrew, direct `.pkg` installer |
| Linux | apt/dpkg, dnf/rpm, snap, portable `tar.gz` |

When more than one channel is detected for the same `pwsh`, the tool resolves to
a single channel deterministically using a per-OS precedence order (ADR-0004)
and reports the others as also-detected.

## Install / build

`pwsh-autoupdate` is a Rust crate. Build a release binary with Cargo:

```sh
cargo build --release
```

The resulting binary is at `target/release/pwsh-autoupdate` (`.exe` on Windows).

## Usage

Update PowerShell through its owning channel (the default, mutating, path):

```sh
pwsh-autoupdate
```

Dry run — report only, with no side effects:

```sh
pwsh-autoupdate --check
```

Add extra diagnostic detail on stderr:

```sh
pwsh-autoupdate --verbose
pwsh-autoupdate --check --verbose
```

There are no other flags. `--check` is read-only: it executes no
package-manager process and performs no state-changing or network-write side
effect. It prints the current installed version, the detected install method
(plus any also-detected channels), the latest available stable version, and the
exact command it would run to update.

Sample `--check` output:

```
Current version: 7.4.0
Detected method: Homebrew (also detected: macOS .pkg)
Latest version:  7.5.0
Status:          update available
Would run: brew upgrade powershell
```

## Exit codes

The exit-code contract follows [ADR-0002](docs/adr/0002-check-exit-code-contract.md).

`--check` (dry run) — designed to be scriptable without parsing stdout:

| Code | Meaning |
|------|---------|
| 0 | up to date |
| 1 | update available |
| 2 | error (network/API failure, parse failure, pwsh not installed, or otherwise unable to determine) |

Default (full-run, mutating) mode:

| Code | Meaning |
|------|---------|
| 0 | success, including the up-to-date no-op |
| non-zero | failure |

Example: branch on whether an update is available, using exit status alone:

```sh
if pwsh-autoupdate --check; then
  echo "PowerShell is up to date"
fi
```

## Non-goals

- Does **not** manage Windows PowerShell 5.1 (the in-box `powershell.exe`,
  Desktop edition). Only the cross-platform `pwsh` (PowerShell 6+/Core) is in
  scope.
- Does **not** bundle, vendor, or embed any PowerShell binary. The upgrade is
  delegated to the host package manager.
- No npm-based update path. npm is not an official PowerShell distribution
  channel.
- No GUI, TUI, or web interface — CLI only.
- No self-update, no scheduling/daemon mode, no telemetry, and no
  rollback/downgrade.
- Never self-elevates. When an upgrade needs administrator/root privileges and
  the process lacks them, the tool surfaces the requirement and exits non-zero
  rather than silently failing.

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE-2.0](LICENSE-APACHE-2.0))

at your option (`MIT OR Apache-2.0`).
