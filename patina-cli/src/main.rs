//! Patina CLI entry point.
//!
//! T-001 wires `#[tokio::main]` and proves the binary can await one of
//! `patina-core`'s public async entry points. The full clap-derived
//! subcommand surface lands in T-016.

use anyhow::Context;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");

    // Existing `--version` short-circuit. Argument parsing is migrated
    // to clap in T-016; this hand-rolled check is intentionally minimal.
    let mut args = std::env::args().skip(1);
    if let Some(arg) = args.next()
        && (arg == "--version" || arg == "-V")
    {
        println!("patina {version}");
        return Ok(());
    }

    // Wiring-proof call: await one of patina-core's public async entry
    // points so the `#[tokio::main]` annotation has a real `.await` to
    // drive. The placeholder error is expected until later tasks land
    // the real engine.
    let result = patina_core::status(patina_core::StatusOptions::default()).await;
    result.context("patina-core::status failed")?;

    Ok(())
}
