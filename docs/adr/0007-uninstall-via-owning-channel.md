# ADR-0007: Uninstall via the owning channel (`--uninstall`)

## Status
Accepted

## Date
2026-07-02

## Context
The tool's core competence is working out which channel owns the installed
`pwsh`. That knowledge is exactly what a correct removal needs too: uninstalling
through the wrong channel (deleting files under an MSI install, `brew
uninstall` on a `.pkg`) strands registrations and files the same way the wrong
upgrade procedure does. Users asked for uninstall support since the detection
work is already done.

Uninstall differs from update in one decisive way: it is **destructive and
irreversible**. The default update path mutates without confirmation because
its failure mode is "still on the old version"; an accidental uninstall's
failure mode is "PowerShell is gone".

## Decision
1. **CLI shape: interactive confirmation by default, `--yes` to skip it.**
   `--uninstall` prints the uninstall report (detected channel + the exact
   command) and then asks `Proceed with the uninstall? [y/N]` — the default is
   No, so a bare Enter aborts. Confirming runs the removal; declining exits 1
   with nothing removed. `--uninstall --yes` (`-y`) skips the prompt for
   scripts. When stdin is **not a terminal** (a pipe, CI), the tool refuses and
   points at `--yes` instead of consuming piped bytes as consent — accidental
   `echo y | ...` or redirected input can never authorize a removal. The
   report is printed *before* the prompt, so the user confirms exactly the
   command that will run (FR-9). `--uninstall` conflicts with `--check` and
   `--replace-portable`. The uninstall path performs **no network read** —
   removing PowerShell never needs the latest version.
2. **Per-channel command table** (pure, in `core::plan::build_uninstall_plan`;
   see design.md §6.1): winget `uninstall --id Microsoft.PowerShell --silent
   --accept-source-agreements` (NOT `--accept-package-agreements`, which
   `winget uninstall` rejects), choco `uninstall powershell-core -y`, brew
   `uninstall powershell`, apt-get `remove -y powershell` (**remove, not
   purge** — `/etc` config stays recoverable), dnf `remove -y powershell`,
   snap `remove powershell`.
3. **macOS `.pkg` is a fixed procedure**, like its update row: no manager
   exists for a `.pkg` install, so the plan encodes Microsoft's documented
   removal — `rm -rf /usr/local/microsoft/powershell /usr/local/bin/pwsh`
   (elevation-gated) with best-effort `pkgutil --forget
   com.microsoft.powershell` receipt cleanup as a post-step.
4. **Portable tar.gz is deleted in-process** (`adapters::portable::
   remove_portable`), reported as `pwsh-autoupdate --uninstall --yes` (FR-9)
   but never PATH self-spawned. Only the layout-gated install directory is
   deleted (the same pure recognition rule as the replace path — never an
   arbitrary directory). In the versioned layout the now-empty `powershell/`
   root is removed non-recursively, so a sibling version keeps it. Symlinks and
   PATH entries (e.g. `~/.local/bin/pwsh`) are not tracked and not removed; the
   host prints a note saying so.
5. **Verification over exit codes.** A non-zero manager exit re-probes and
   reports success (with a transparency line) only if `pwsh` is actually gone;
   a zero exit with a `pwsh` still present (a second install through another
   channel) gets an informational note — that install is never touched (FR-6).
   `pwsh` absent up front is an idempotent success ("nothing to uninstall").
6. **Safety invariants carry over:** uninstall only via the single owning
   channel, never a fallback (FR-6); never self-elevate — surface the
   requirement and stop (FR-12); never report success on a real failure; the
   unconfirmed run executes zero mutating commands (G-3). The current version is parsed
   leniently (`unknown` renders in the report) — an unparseable version must
   not block a removal that does not depend on it.

## Consequences
- The lifecycle is now symmetric: install when missing (ADR-0006), update, and
  uninstall all flow through the same detection and the same owning channel.
- The README's "no other flags" claim and exit-code tables are extended; the
  design doc gains the uninstall command table (§6.1) and pseudocode (§7).
- The `.pkg` row hardcodes Microsoft's documented paths — accepted, the same
  fixed-procedure precedent as the update table's `installer -pkg … -target /`.
- A portable uninstall can leave dangling user-created symlinks; documented and
  surfaced at runtime rather than guessed at.
- Non-interactive environments (pipes, CI) must pass `--yes` explicitly; a
  declined or unaskable confirmation exits 1 with nothing removed.
