//! Patina CLI entry point.
//!
//! T-001 wires `#[tokio::main]` and proves the binary can await one of
//! `patina-core`'s public async entry points. The full clap-derived
//! subcommand surface — including user-facing output routed through
//! the `output::Reporter` layer — lands in T-016.

use anyhow::Context;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Wiring-proof call: await one of patina-core's public async entry
    // points so the `#[tokio::main]` annotation has a real `.await` to
    // drive. The placeholder error is expected until later tasks land
    // the real engine.
    //
    // No user-facing output is emitted here: the `output::Reporter`
    // layer (T-016 / Phase 10) is the only sanctioned print site per
    // AGENTS.md, and argument parsing (including `--version`) is
    // migrated to clap in T-016.
    let result = patina_core::status(patina_core::StatusOptions::default()).await;
    result.context("patina-core::status failed")?;

    Ok(())
}
