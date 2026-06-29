# ADR-0004: Multiple-install-method resolution precedence

## Status
Accepted

## Date
2026-06-29

## Context
On a real machine more than one install channel can appear to be present (e.g. Windows shows both winget and an MSI registration; macOS shows both Homebrew and a `.pkg`; Linux shows a system package plus a leftover portable tar.gz). The tool must pick exactly one channel to upgrade through, deterministically, and the same choice must drive both the `--check` report and the actual upgrade so the reported command equals the executed one. Without a stated precedence the choice would be nondeterministic and untestable.

Alternatives considered:
- **A. Deterministic per-OS precedence order, report the winner and note the others.** (chosen)
- **B. Prompt the user to choose.** Breaks the non-interactive CLI / scriptability goal.
- **C. Error out and refuse when ambiguous.** Safe but unhelpful; many machines are legitimately multi-channel.

## Decision
When multiple supported methods are detected, the tool selects exactly one via a fixed per-OS precedence and reports the selection plus the also-detected methods. The same precedence is applied identically in `--check` and update modes.

Precedence (highest wins):
- **Windows:** winget (`Microsoft.PowerShell`) > MSIX/Microsoft Store > MSI > Chocolatey.
- **macOS:** Homebrew > `.pkg`.
- **Linux:** native system package manager that owns the `pwsh` binary (apt/dpkg, dnf/rpm) > snap > portable tar.gz.

Rationale: prefer the channel most likely to *own* and cleanly manage the binary and to be the maintained/managed path on that OS; the portable tar.gz, which no package manager owns, is always lowest.

## Consequences
- Positive: deterministic, testable selection (fixture-driven per FR-10); `--check` and update agree by construction.
- Positive: keeps the CLI non-interactive and scriptable.
- Negative: the precedence is an opinion; a user whose "real" install is a lower-precedence channel may be upgraded through a higher-precedence one. Mitigated by reporting all detected methods so the user can see the choice. A future `--method` override is the extension point.
- Binds: FR-3/FR-4/FR-5 (per-OS detection), FR-9 (resolution + reporting), FR-7 (report matches execution).

## Supersedes
None.
