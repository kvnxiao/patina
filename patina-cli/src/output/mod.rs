//! User-facing output layer.
//!
//! [`reporter`] owns the [`Reporter`](reporter::Reporter) trait — the
//! single sanctioned print site — and [`diff`] renders the embedded
//! `similar` diff that feeds it. No other module in the crate prints to
//! stdout / stderr directly; logging goes through `tracing` instead.

pub mod diff;
pub mod reporter;
