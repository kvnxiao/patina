//! The filesystem watcher subsystem (SPEC-0003).
//!
//! The watcher reapplies on source changes and surfaces files modified
//! outside Patina. So far this subsystem lands two pieces: the drift-cache
//! format — the watcher's notification ledger at `<state>/patina/drift.cache`
//! — via the [`drift_cache`] submodule, and the structured-log sink — the
//! daily-rotating `<state>/patina/logs/` stack the watcher writes its metrics
//! into (REQ-009) — via the [`logging`] submodule; and the pure mapping from
//! a committed journal record to the watcher's FS subscription set (REQ-005)
//! — via the [`subscriptions`] submodule. The event loop, per-OS service
//! install, and drift detection itself land in later tasks.

pub mod drift_cache;
pub mod logging;
pub mod subscriptions;
