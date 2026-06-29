//! OS / filesystem probe seam.
//!
//! `cfg`-gated per OS; emits the plain `DetectionSignals` consumed by the pure
//! `core::detect` rules. The real per-OS probing is filled in by the
//! `probe-adapter` task in a later cluster; this scaffold establishes the
//! module so the core/adapter boundary is in place.
