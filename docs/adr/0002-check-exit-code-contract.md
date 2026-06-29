# ADR-0002: `--check` exit-code contract

## Status
Accepted

## Date
2026-06-29

## Context
The `--check` / dry-run mode reports current version, detected method, latest version, and the exact command that would run, with no side effects. Operators script `--check` in CI and cron, so its exit code is a machine contract: it must distinguish "up to date", "update available", and "error" without parsing stdout. The intake did not pin the codes, so a concrete, documented mapping is needed and binds the CLI surface, the README, and any automation built on the tool.

Alternatives considered:
- **A. 0 = up to date, 1 = update available, 2 = error.** (chosen)
- **B. Always exit 0 from `--check`; convey state only on stdout.** Forces callers to parse text; brittle.
- **C. 0 = up to date, non-zero only on error; update-available also 0.** Cannot script "is an update available?" via exit status.

## Decision
`--check` exit codes are: **0 = up to date**, **1 = update available**, **2 = error** (network/API failure, parse failure, or otherwise unable to determine). The full-run (non-`--check`) mode uses **0 = success** (including the up-to-date no-op) and **non-zero = failure**. These codes are documented in the README (FR-13).

## Consequences
- Positive: `--check` is scriptable — a caller branches on exit status alone (`if pwsh-autoupdate --check; then ...`), no stdout parsing.
- Positive: clear separation of "update available" (1) from real errors (2), so automation does not treat an available update as a failure-to-investigate.
- Negative: exit code 1 meaning "update available" (not "error") is mildly unconventional and must be documented prominently to avoid surprise.
- Binds: FR-7 (`--check` reporting + exit signal), FR-11 (error path → code 2), FR-13 (README documents codes), G-3/G-4.

## Supersedes
None.
