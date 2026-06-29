//! Library root for `pwsh-autoupdate`.
//!
//! Keeping the crate testable as a library: the binary (`main.rs`) stays thin
//! and tests drive the library with fake adapters. The dependency arrows point
//! inward — the host depends on adapters and core; adapters depend on core; the
//! core depends on nothing outside `std` + a few pure crates (`serde`,
//! `semver`, `thiserror`).
//!
//! The orchestration entry points live in [`app`]: [`app::run_check`] and
//! [`app::run_update`] take the two trait objects (`&dyn HttpClient`,
//! `&dyn CommandRunner`) plus the resolved [`core::Os`], so the binary and the
//! tests drive the *same* code path — the production wiring is never bypassed.

pub mod adapters;
pub mod app;
pub mod cli;
pub mod core;
