//! User-facing output layer.
//!
//! [`reporter`] owns the [`Reporter`](reporter::Reporter) trait, the single
//! sanctioned print site. [`diff`] renders the embedded `similar` diff that
//! feeds it, [`table`] aligns the multi-row listings into columns, and
//! [`style`] defines the palette they paint with. A renderer reads that
//! palette off the reporter. Every other module logs through `tracing` rather
//! than printing to stdout / stderr.

pub mod diff;
pub mod reporter;
pub mod style;
pub mod table;
