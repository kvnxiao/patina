//! Patina CLI entry point.
//!
//! Phase 0 of SPEC 0001 ships only `--version` / `-V`. The full subcommand
//! surface lands in Phase 10.

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    println!("patina {version}");
}
