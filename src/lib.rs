//! Library root for `pwsh-autoupdate`.
//!
//! Keeping the crate testable as a library: the binary (`main.rs`) stays thin
//! and tests drive the library with fake adapters. The dependency arrows point
//! inward — the host depends on adapters and core; adapters depend on core; the
//! core depends on nothing outside `std` + a few pure crates (`serde`,
//! `semver`, `thiserror`).
//!
//! The orchestration entry points (`run_check`, `run_update`) are added in a
//! later cluster; this cluster delivers the pure core (`core/`) and the
//! compiling adapter scaffolding (`adapters/`).

pub mod adapters;
pub mod cli;
pub mod core;
