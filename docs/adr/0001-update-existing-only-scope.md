# ADR-0001: Update-existing-only scope (no install-from-scratch)

## Status
Superseded by ADR-0006 (install-when-missing). The v1 update-existing-only scope
below was the original decision; install-from-scratch is now in scope.

## Date
2026-06-29

## Context
pwsh-autoupdate detects how PowerShell was installed and upgrades it through the owning channel. A reasonable builder could extend this to *install* PowerShell on a machine that has none — the detection and release-resolution machinery is most of what an installer needs. The intake Non-Goals exclude this, but the boundary is load-bearing for the planner (it changes which channels must support a from-zero install path) and for error handling (absent `pwsh` is a terminal report, not a trigger to install).

Alternatives considered:
- **A. Update-existing-only (chosen).** Absent `pwsh` ⇒ report and exit non-zero.
- **B. Also install from scratch when absent.** Pick a default channel per OS and run its install command.
- **C. Install behind an explicit `--install` flag.** Smaller surface than B but still pulls in per-OS install-command mappings and a default-channel choice.

## Decision
v1 only updates an existing `pwsh`. If PowerShell is not installed, the tool reports that it is not installed and exits non-zero without attempting installation. The tool also never bundles or vendors PowerShell binaries; the upgrade is always delegated to the detected host package manager.

## Consequences
- Positive: smaller, safer scope; no need to choose a "default install channel"; detection can assume an existing `pwsh` to anchor method detection; fits the v1 appetite.
- Positive: avoids the risk of creating a second, differently-channeled install on a machine.
- Negative: a user on a fresh machine gets no help installing; they must install once by hand, then the tool maintains it.
- Binds: FR-1 (unsupported-OS / absent handling), FR-6 (delegate to manager, no bundled binaries), Section 7 edge case "pwsh absent".

## Supersedes
None.
