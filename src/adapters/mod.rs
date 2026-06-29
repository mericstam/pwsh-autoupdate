//! Adapters — the thin layer that touches the outside world behind traits.
//!
//! The HTTP client and the process runner sit behind traits so tests inject
//! fakes; production wires the real impls. The OS probe is `cfg`-gated and
//! emits plain `DetectionSignals` consumed by the pure core.
//!
//! The `version-resolve` orchestration (`resolve_latest_stable`) is added in a
//! later cluster; this cluster delivers the compiling trait seams.

pub mod http;
pub mod probe;
pub mod runner;
