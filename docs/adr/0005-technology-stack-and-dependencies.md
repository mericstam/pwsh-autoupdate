# ADR-0005: Technology stack & dependencies

## Status
Accepted

## Date
2026-06-29

## Context
The PRD fixes Rust (stable, edition 2021) and Cargo as the stack and names candidate crates but
leaves the final selection — and, critically, the **exact pinned versions** — to the architect
(PRD §8). The build must be reproducible and the coder must not invent dependency choices or
versions. Constraints that bind this decision:

- **Licensing:** MIT or Apache-2.0 only (or dual). No GPL-family (GPL/LGPL/AGPL). No paid SaaS.
- **Hermetic, non-async CLI:** tests perform no real network and spawn no real package-manager
  process; the tool is a short-lived non-interactive CLI, so a heavy async runtime is unjustified.
- **HTTP client choice (single):** the PRD lists `reqwest` or `ureq`. We must pick one.
- The design (`docs/design.md`) places all HTTP behind an `HttpClient` trait and all process
  execution behind a `CommandRunner` trait, so the concrete crates are swappable and confined to
  the adapter layer.

## Decision

### Edition & toolchain
- **Edition 2021**, **stable** Rust toolchain. **MSRV policy: latest stable** — we do not pin or
  promise an old MSRV this increment; CI uses `dtolnay/rust-toolchain@stable`. (Note: clap 4.6's own
  MSRV is recent stable; this is consistent with a stable-only policy.)

### HTTP client: `ureq` (chosen over `reqwest`)
We use **`ureq`** (blocking, no async runtime, MIT OR Apache-2.0). Rationale:
- The tool makes a couple of simple GET requests (GitHub Releases API + the build-info feed). It has
  no need for async concurrency, connection pooling at scale, or advanced TLS features.
- `ureq` is blocking and pulls **no async runtime** (no `tokio`), keeping the dependency tree, the
  binary, and the test surface small and synchronous. The whole orchestration is a straight-line
  blocking flow, which is simpler to reason about and to test with `mockito` (blocking).
- `reqwest` would be justified only by async or advanced-TLS needs, which v1 does not have; choosing
  it would drag in `tokio`/`hyper` for no benefit. We therefore explicitly reject `reqwest` for this
  increment.

**Version note (important for the coder):** `ureq` is pinned at **3.x**, which is a different API
from the `ureq 2.x` shown in the `rust-cli` skill. In 3.x there is **no `features = ["json"]`**;
JSON is read via `response.body_mut().read_json::<T>()` (with the `json` capability enabled by
default through `serde`). Build the request with `ureq::get(url)`, set a `User-Agent` header (GitHub
rejects requests without one), call `.call()`, and read the body. Keep all of this inside
`adapters/http.rs` behind the `HttpClient` trait; nothing else in the crate touches `ureq` types.

### Error handling
- **`thiserror` 2.x** for typed errors in `core/` (note: 2.x, not the 1.x in the skill — same
  attribute surface for our use). **`anyhow` 1.x** for application-level error context in
  `main.rs`/adapters. Never `unwrap`/`expect` on IO; convert to `Result` and surface to stderr.

### Pinned dependencies (the coder MUST use these)

```toml
[package]
edition = "2021"

[dependencies]
clap        = { version = "4.6",  features = ["derive"] }
serde       = { version = "1.0",  features = ["derive"] }
serde_json  = "1.0"
semver      = "1.0"
anyhow      = "1.0"
thiserror   = "2.0"
ureq        = "3.3"

[dev-dependencies]
mockito     = "1.7"
assert_cmd  = "2.0"
predicates  = "3.1"
tempfile    = "3.27"
```

Concrete current-stable versions this resolves to at authoring time (record for reproducibility):
clap 4.6.1, serde 1.0.x, serde_json 1.0.x, semver 1.0.28, anyhow 1.0.x, thiserror 2.0.18,
ureq 3.3.0, mockito 1.7.2, assert_cmd 2.2.2, predicates 3.1.4, tempfile 3.27.0. A `Cargo.lock` is
committed (binary crate) to lock the full transitive graph.

HTTP mocking uses **`mockito`** (blocking, fits `ureq`'s blocking client) rather than `wiremock`
(async) — consistent with the no-async-runtime decision.

### License posture
Every crate above is **MIT OR Apache-2.0** (clap, serde, serde_json, semver, anyhow, thiserror,
ureq, assert_cmd, predicates, tempfile) or **MIT** (mockito). No GPL-family crate is introduced. A
machine-checkable license gate (`cargo deny check licenses`, with a `deny.toml` allowing only
MIT/Apache-2.0/Unicode-style permissive licenses, or an equivalent manifest audit) runs in CI
(`packaging-ci` task) and must pass before a task is "done".

### External package managers are runtime shell-out deps, not linked libraries
`winget`, `choco`, `brew`, `apt-get`/`dpkg`, `dnf`/`rpm`, `snap`, `installer`, etc. are invoked as
**external processes** via the `CommandRunner` adapter. They are **not** Cargo dependencies, are not
linked, vendored, or bundled, and carry no license obligation on this crate. Availability is checked
(`CommandRunner::exists`) before invocation; absence is surfaced as a clear error (graceful
degradation), never a silent fallback to another channel (FR-6).

## Consequences
- **Positive:** blocking HTTP + no async runtime keeps the CLI and its hermetic tests simple and
  synchronous; `mockito` (blocking) pairs cleanly with `ureq`. Small dependency tree, fast builds.
- **Positive:** all versions are pinned, licenses are uniformly permissive and CI-gated, and the
  concrete HTTP/process crates are confined behind traits so they are swappable without touching core.
- **Negative / watch-items:** `ureq` 3.x and `thiserror` 2.x differ from the 2.x/1.x examples in the
  `rust-cli` skill — the coder must follow the 3.x/2.x APIs noted above, not the skill's snippets.
  Choosing `ureq` forgoes `reqwest`'s richer feature set; if a future increment needs async or
  advanced TLS, that is a superseding ADR.
- **Binds:** PRD §8 (stack & licensing), FR-2 (HTTP sources), FR-10 (hermetic mocked tests),
  `scaffold-crate`/`http-adapter`/`hermetic-tests`/`packaging-ci` tasks, and `docs/design.md`.

## Supersedes
None. Continues the product ADR sequence (0001–0004) without contradicting any of them.
