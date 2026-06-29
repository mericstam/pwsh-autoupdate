# ADR-0003: Prerelease handling policy — stable-only by default

## Status
Accepted

## Date
2026-06-29

## Context
PowerShell publishes prerelease (preview/RC) builds on GitHub alongside stable releases. The tool resolves "latest" and compares it against the installed version with semver rules. semver precedence places prereleases below their release (e.g. `7.5.0-preview.1` < `7.5.0`), and the GitHub `releases/latest` endpoint can surface different artifacts than the stable build-info feed. The PRD must state, unambiguously, what "latest" tracks — otherwise the tool could offer a preview as an "update" or mis-handle a user who is already on a preview.

Alternatives considered:
- **A. Stable-only by default; ignore prereleases; no opt-in this increment.** (chosen)
- **B. Track whatever GitHub `releases/latest` returns**, prerelease or not. Risks offering previews as updates.
- **C. Stable-only with an explicit `--prerelease` opt-in flag in v1.** More surface and another channel-mapping path than v1 needs.

## Decision
The default and only channel in v1 is **stable**. "Latest" is the latest stable release, anchored on the `aka.ms/pwsh-buildinfo-stable` feed and cross-checked with the GitHub Releases tag. Prereleases are ignored when computing "latest". A user already running a prerelease whose release version is at or below the latest stable is offered the newer stable (a newer stable supersedes an installed prerelease of the same release number, per semver). No `--prerelease` opt-in flag is included this increment.

## Consequences
- Positive: deterministic, surprise-free behavior; the tool never pushes a preview as a routine update.
- Positive: a clean upgrade path off a preview onto the matching/newer stable.
- Negative: users who *want* to stay on preview channels are unsupported in v1 (a future `--prerelease` flag is the extension point).
- Binds: FR-2 (resolve stable), FR-8 (semver comparison incl. prerelease case), Section 7 prerelease edge case.

## Supersedes
None.
