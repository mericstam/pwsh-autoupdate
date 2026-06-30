# ADR-0006: Install PowerShell when missing (supersedes ADR-0001)

## Status
Accepted (supersedes ADR-0001)

## Date
2026-06-29

## Context
ADR-0001 scoped v1 to **update-existing-only**: an absent `pwsh` was a terminal
report, never a trigger to install. That decision explicitly noted its own
downside — *"a user on a fresh machine gets no help installing"* — and listed two
deferred alternatives: (B) auto-install when absent, and (C) install behind an
explicit `--install` flag. The detection + release-resolution machinery already
covers most of what an installer needs.

The operator has now asked for install-when-missing, choosing **auto-install when
absent** (option B) with a **native package manager, falling back to a portable
install**. This reverses the ADR-0001 non-goal.

A second finding motivated revisiting this: the `PortableTarGz` *update* path
emitted a `pwsh-autoupdate --replace-portable` self-invoke that was never
implemented (no real download/extract machinery, and `--replace-portable` is not
even a parsed flag) — a latent stub. The portable installer that option-B's
fallback needs is the same machinery that fixes that stub.

## Decision
When PowerShell is **absent**, the tool installs it (it no longer errors out):

1. **Trigger: auto when absent.** A bare run installs; `--check` stays a
   side-effect-free dry run that reports what it *would* install. We install only
   when *none* is present, so this never creates a second, conflicting install
   (the risk ADR-0001 guarded against is preserved).
2. **Channel: native, fall back to portable.** Prefer the OS's standard package
   manager when present — Windows `winget` (then `choco`), macOS Homebrew
   (`brew install powershell` — the formula; the old `--cask` is deprecated),
   Linux `snap` (`--classic`). `apt`/`dnf`
   are excluded as auto-install channels (a from-zero install needs Microsoft's
   repo configured first — multi-step, root). When no native channel is
   available, fall back to a **portable install** under `~/.local`.
3. **Safety invariants carry over:** never self-elevate (FR-12) — surface the
   requirement and stop; the owning manager must be on PATH (FR-6); never report
   success on a non-zero installer exit; never fabricate a version (FR-11).

### Phased delivery
- **Step 1 (this change):** install-resolution + `--check` dry-run + **native**
  install execution. When no native manager is present the tool gives an honest
  "cannot install — install manually" error (exit non-zero). This is a real,
  working partial — **not** a stub: it never claims a capability it lacks.
- **Step 2 (follow-up):** the portable download/extract/symlink executor, which
  also fixes the `--replace-portable` portable-update stub. After step 2, the
  no-native-manager path installs portably instead of erroring.

### Exit codes (extends ADR-0002)
- `--check`, pwsh absent + installable → `1` (action available), prints
  `Would install: <cmd>`.
- `--check`, pwsh absent + no native channel (step 1) → `2` (error).
- update path, pwsh absent → install runs; success `0`, failure non-zero.

## Consequences
- A fresh machine with a supported package manager is bootstrapped automatically.
- Reverses ADR-0001's non-goal; the README and FR set are updated accordingly.
- Detection can no longer assume an existing `pwsh` to anchor on; the probe now
  enumerates available managers even when pwsh is absent (to pick a channel).
- Step 1 has a real limitation: hosts without a supported native manager still
  can't be bootstrapped until step 2 lands; this is surfaced honestly.

## Supersedes
ADR-0001 (update-existing-only scope).
