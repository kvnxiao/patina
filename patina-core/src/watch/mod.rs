//! The filesystem watcher subsystem (SPEC-0003).
//!
//! The watcher reapplies on source changes and surfaces files modified
//! outside Patina. This task (T-004) lands only the drift-cache format —
//! the watcher's notification ledger at `<state>/patina/drift.cache` — via
//! the [`drift_cache`] submodule. The event loop, per-OS service install,
//! and drift detection itself land in later tasks.

pub mod drift_cache;
