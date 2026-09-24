---
paths: **/*.{rs,toml}
description: "Required Rust lint baseline, scoped exceptions, conditional restrictions, nightly rustfmt settings, and shared local/CI tasks with stable Clippy."
---

# Lints and Formatting

For async code, use the
[async verification rules](rust-async.md#verify-async-contracts-deterministically-required)
alongside the baseline; review lifecycle and cancellation contracts beyond lint coverage.

## Install the complete lint baseline (Required)

When bootstrapping a Rust project, install the complete configuration below in `Cargo.toml`. For a
workspace, use `[workspace.lints.rust]`, `[workspace.lints.clippy]`, and `[workspace.lints.rustdoc]`
instead, and set `[lints] workspace = true` in every member, including a root package. Keep the
baseline in one manifest location; apply the same policy to application and library crates.

```toml
[lints.rust]
unsafe_code = "forbid"
unsafe_op_in_unsafe_fn = "deny"
missing_docs = "warn"
unreachable_pub = "warn"
unnameable_types = "warn"
elided_lifetimes_in_paths = "warn"

[lints.clippy]
all = { level = "warn", priority = -2 }
pedantic = { level = "warn", priority = -2 }
cargo = { level = "warn", priority = -2 }
correctness = { level = "deny", priority = -1 }

multiple_crate_versions = "allow"
cast_precision_loss = "allow"
must_use_candidate = "allow"

unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
unreachable = "deny"
string_slice = "deny"
indexing_slicing = "deny"
get_unwrap = "deny"
unwrap_in_result = "deny"
panic_in_result_fn = "deny"
unchecked_time_subtraction = "deny"
todo = "warn"
unimplemented = "warn"

let_underscore_future = "deny"
let_underscore_must_use = "deny"
unused_result_ok = "deny"
map_err_ignore = "deny"

await_holding_lock = "deny"
await_holding_refcell_ref = "deny"
if_let_mutex = "deny"
large_futures = "deny"

mem_forget = "deny"
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "deny"
unnecessary_safety_doc = "deny"
unnecessary_safety_comment = "deny"

float_cmp = "deny"
lossy_float_literal = "deny"
invalid_upcast_comparisons = "deny"

rc_mutex = "deny"
debug_assert_with_mut_call = "deny"
iter_not_returning_iterator = "deny"
expl_impl_clone_on_copy = "deny"
infallible_try_from = "deny"
dbg_macro = "deny"

print_stdout = "deny"
print_stderr = "deny"
exit = "deny"

cast_lossless = "deny"
cast_possible_truncation = "deny"
cast_possible_wrap = "deny"
cast_sign_loss = "deny"

allow_attributes = "deny"
allow_attributes_without_reason = "deny"

inefficient_to_string = "deny"
large_enum_variant = "warn"
large_stack_arrays = "deny"
needless_pass_by_value = "warn"
missing_errors_doc = "warn"
missing_panics_doc = "warn"

[lints.rustdoc]
broken_intra_doc_links = "deny"
private_intra_doc_links = "deny"
missing_crate_level_docs = "warn"
```

Use the negative group priority so individual settings override group membership. Select restriction
lints individually; do not enable `clippy::restriction` as a group. See
[Clippy lint configuration](https://doc.rust-lang.org/clippy/usage.html#lint-configuration).

Keep correctness lints at `deny` even when Clippy runs without `-D warnings`. Ordinary Cargo build
and run commands do not run Clippy. Retain the individually selected nursery lint
`debug_assert_with_mut_call` to catch mutation inside debug assertions; nursery membership does not
require nightly Clippy.

Use private or `pub(crate)` visibility for internal items. Re-export types that callers must name;
use the sealed-trait exception below for an intentionally unnameable sealing trait. Give each crate
root, including integration test crates under `tests/`, a concise `//!` description of its contract.

Keep Cargo metadata checks enabled. Mark intentionally unpublished packages `publish = false`;
Clippy skips their publication metadata by default. Allow duplicate dependency versions and inspect
`cargo tree -d` when build size, compile times, or incompatible dependency types warrant it. See
[Cargo metadata checking](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/cargo/common_metadata.rs)
and
[dependency duplication](https://doc.rust-lang.org/cargo/reference/resolver.html#version-incompatibility-hazards).

## Configure Clippy and test allowances (Required)

Commit this `clippy.toml` at the project or workspace root:

```toml
allow-expect-in-tests = true
allow-print-in-tests = true
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
avoid-breaking-exported-api = false
excessive-nesting-threshold = 4
max-fn-params-bools = 1
```

Default test functions to returning `()` and use descriptive `expect` messages for fallible setup.
Keep shared helpers fallible or place test-only helpers in `#[cfg(test)]` modules. Apply the
allowances only in contexts Clippy recognizes as tests; a helper's location under `tests/` alone
does not establish that context. Permit indexing and explicit panic for test failures; keep
`unwrap_used` and all other baseline restrictions active. See
[test configuration](https://doc.rust-lang.org/clippy/lint_configuration.html#allow-expect-in-tests).

Lint exported APIs during bootstrap with `avoid-breaking-exported-api = false`. Before applying a
suggested API change to an existing published library, review compatibility and obtain approval for
any necessary lint exception. Keep the nesting threshold at four and permit at most one boolean
parameter; use named states for independent flags. See
[Clippy configuration](https://doc.rust-lang.org/clippy/lint_configuration.html#avoid-breaking-exported-api).

## Limit exceptions to their approved scope (Required)

Apply the predefined exceptions below without further approval. Before introducing another exception
or weakening the baseline, obtain the user's approval. Use `#[expect(lint, reason = "...")]` on the
smallest applicable item and state the concrete contract that permits the operation. Remove stale
expectations when the lint stops firing. Keep `unfulfilled_lint_expectations` enabled and do not
substitute broad `allow` attributes.

When a diagnostic occurs only under a feature or target condition, attach the expectation with the
same condition, such as `#[cfg_attr(feature = "cli", expect(clippy::print_stdout, reason = "CLI
output lists packages"))]`. Passing the checks does not establish compliance with contracts that
require code review.

| Condition                                                                                     | Permitted exception                                                                                                                                                                                                        |
| --------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A function implements CLI output or diagnostics                                               | Expect only the `print_stdout` or `print_stderr` lint that fires in that function.                                                                                                                                         |
| A build script emits Cargo directives or a function implements another stdout/stderr protocol | Scope the corresponding printing expectation to the protocol-emitting function.                                                                                                                                            |
| A public trait in a private module intentionally seals another public trait                   | Expect `unnameable_types` only on the sealing trait; keep ordinary return and argument types nameable.                                                                                                                     |
| An invalid literal is rejected during forced compile-time evaluation                          | Keep rejection in a const evaluation context; request approval if a lint suppression is still needed.                                                                                                                      |
| The user approves unsafe implementation in a crate                                            | Change the applicable `unsafe_code` policy from `forbid` to `deny`, then scope expectations to the approved implementation and satisfy the [unsafe obligations](rust-api-design.md#unsafe-soundness-obligations-required). |

```rust
#[expect(clippy::print_stdout, reason = "CLI output lists the installed packages")]
fn print_packages(packages: &[String]) {
    for package in packages {
        println!("{package}");
    }
}
```

Return an exit status from the entry point rather than calling `std::process::exit`. Use fallible
APIs for runtime validation; an author-controlled value alone does not authorize a runtime panic.
Declaring a function `const fn` does not force its callers to evaluate it at compile time. Follow
the [constructor policy](rust-api-design.md#use-fallible-runtime-constructors-required).

When a workspace member needs a different unsafe policy, account for Cargo's lint inheritance:
member manifests cannot combine `workspace = true` with additional lint entries. Preserve all other
baseline settings in the approved configuration. A local expectation cannot override `forbid`. See
[Cargo lint inheritance](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-lints-table)
and
[Rust lint levels](https://doc.rust-lang.org/reference/attributes/diagnostics.html#lint-check-attributes).

## Enable project-specific restrictions only when needed (Conditional)

Enable the following restrictions only when their stated requirements apply. Set the corresponding
lint in the Clippy manifest table unless the row names a Rust lint, and document the requirement.

| Requirement                                                                            | Setting                                                         |
| -------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| Integer-to-float conversions must preserve exact values                                | `cast_precision_loss = "deny"`                                  |
| Floating-point comparisons against constants must use a domain-defined error tolerance | `float_cmp_const = "deny"`                                      |
| Iteration order affects reproducible output or other observable behavior               | `iter_over_hash_type = "deny"`                                  |
| A public library exposes types that callers need to inspect in diagnostics             | `missing_debug_implementations = "warn"` in the Rust lint table |
| Numeric code needs lint coverage for arithmetic on untrusted or unbounded values       | `arithmetic_side_effects = "deny"`                              |

For public libraries, default to `Debug` implementations on public types and enable the
corresponding Rust lint. Redact sensitive fields in a manual implementation. Apply the lint at the
library crate root when a shared manifest also governs unrelated application targets.

When enabling `arithmetic_side_effects`, configure `arithmetic-side-effects-allowed` only for domain
types whose operations have the required semantics. Keep the
[safe arithmetic contract](rust-defensive-programming.md#safe-arithmetic-required) mandatory even
when this lint is not enabled.

For approximate numeric work, choose conversions and tolerances from the numerical contract. Keep
ordinary `float_cmp` active; seek approval for a necessary exact comparison it flags. Sort hash
collection output only where ordering matters. Keep Clippy's other thresholds at their defaults
unless an approved project requirement establishes different limits.

When a project requires injectable environment or filesystem access, enable `disallowed_methods =
"deny"` and configure the actual adapter API in `clippy.toml`. Treat this as a conditional example,
replacing the adapter name with the project's API:

```toml
disallowed-methods = [
    { path = "std::env::var", reason = "use system::env_var for injectable environment access" },
]
```

When documentation uses identifiers that `doc_markdown` should accept as prose, add the project's
identifiers to `doc-valid-idents` and include `".."` to preserve Clippy's built-in list. Do not copy
another project's domain vocabulary.

## Check tracing guards across await (Required)

When a project uses `tracing`, add these entries to `clippy.toml`, preserving any existing invalid
types:

```toml
await-holding-invalid-types = [
    "tracing::span::Entered",
    "tracing::span::EnteredSpan",
]
```

The baseline's `clippy::all` enables `await_holding_invalid_type`, and the shared `-D warnings`
check rejects these guards across await points. Apply the
[tracing context rule](rust-async.md#preserve-tracing-context-and-field-privacy-required) when
fixing diagnostics. See
[Clippy's invalid-type configuration](https://doc.rust-lang.org/clippy/lint_configuration.html#await-holding-invalid-types).

## Commit the complete nightly rustfmt configuration (Required)

Commit `.rustfmt.toml` or `rustfmt.toml` with all settings below. Set `edition` and `style_edition`
to the project's edition; this example uses Rust 2024. Format imports as one sorted group with one
imported item per `use` statement, including re-exports.

```toml
edition = "2024"
style_edition = "2024"
unstable_features = true
reorder_imports = true
group_imports = "One"
imports_granularity = "Item"
newline_style = "Unix"
normalize_comments = true
normalize_doc_attributes = true
wrap_comments = true
float_literal_trailing_zero = "Always"
```

Run formatting with `cargo +nightly fmt`; the unstable settings require nightly rustfmt. Use
floating stable Rust for compilation, tests, and Clippy, and floating nightly for formatting. Keep
any MSRV check separate from the stable lint task. See
[rustfmt configuration](https://github.com/rust-lang/rustfmt/blob/master/Configurations.md).

Commit `rust-toolchain.toml` so unqualified build commands select stable:

```toml
[toolchain]
channel = "stable"
profile = "minimal"
components = ["clippy"]
```

Configure editor formatting to use nightly rustfmt as well. For rust-analyzer, use this editor
setting; other editors must invoke the equivalent formatter command:

```json
{
  "rust-analyzer.rustfmt.overrideCommand": ["rustup", "run", "nightly", "rustfmt"]
}
```

The override runs `rustfmt` with source on stdin, not `cargo fmt`. See
[rust-analyzer formatter configuration](https://rust-analyzer.github.io/book/configuration.html#rust-analyzer.rustfmt.overrideCommand).

## Share local fix, lint, and CI tasks (Required)

Provide local tasks that apply formatting, apply Clippy fixes, check lints, build documentation,
check dependencies, and run tests. Use the same toolchain channels, target and feature coverage, and
strict checks locally and in CI. Install or update the floating toolchains with:

```sh
rustup toolchain install stable --profile minimal --component clippy
rustup toolchain install nightly --profile minimal --component rustfmt
cargo +stable install --locked cargo-audit cargo-machete
```

For a project using `just`, use these recipes. Adapt them to an existing task runner without
changing the policy. Generate and commit the lockfile for reproducible resolution; `--locked`
requires it to exist and match the manifest.

```just
clippy_scope := "--workspace --all-targets --all-features --locked"

fmt:
    cargo +nightly fmt --all

lint:
    cargo +nightly fmt --all --check
    cargo +stable clippy {{clippy_scope}} -- -D warnings

fix:
    cargo +stable clippy --fix {{clippy_scope}} --allow-dirty
    just fmt
    just lint

test:
    cargo +stable test --workspace --all-features --locked

doc:
    RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings" cargo +stable doc --workspace --all-features --no-deps --locked

dependencies:
    cargo +stable audit
    cargo +stable machete

check: lint test doc dependencies
```

When features cannot be enabled together, replace `--all-features` with explicit supported
combinations and use those same combinations for local fixes, linting, documentation, tests, MSRV
checks, and CI. Keep doctests in test coverage. Have CI install the toolchains and invoke the shared
tasks rather than maintain separate Cargo commands. Review the fix diff; a successful fix task must
finish with strict lint verification.

Use equivalent advisory and unused-dependency tools when the project already standardizes on them;
keep them in the shared local and CI tasks. Configure license and dependency-source policy from the
project's requirements. See
[dependency evaluation](rust-dependencies.md#evaluate-before-adding-default).

Provide a separate MSRV task that reads the selected member's resolved `rust_version` from Cargo
metadata. This `just` example requires Bash and `jq`; run it from the workspace root. In CI, invoke
`just check-msrv PACKAGE` for every member covered by the compatibility promise, including a root
package. Keep this job separate from stable Clippy:

```just
[positional-arguments]
check-msrv package:
    #!/usr/bin/env bash
    set -euo pipefail
    metadata=$(cargo +stable metadata --no-deps --format-version 1 --locked)
    msrv=$(jq -er --arg name "$1" '
        .workspace_members as $members
        | .packages[]
        | select(.id as $id | $members | index($id))
        | select(.name == $name)
        | .rust_version // error("selected package must declare rust-version")
    ' <<< "$metadata")
    rustup toolchain install "$msrv" --profile minimal
    cargo +"$msrv" check --package "$1" --all-targets --all-features --locked
```

The metadata value resolves workspace inheritance. Keep the target and feature scope aligned with
the declared compatibility promise. See
[Cargo metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html).

Enforce `-D warnings` in both the local lint task and CI. Keep `#![deny(warnings)]` out of source.
Treat warnings from new stable Clippy releases and formatting changes from new nightly releases as
maintenance work. `todo!()` and `unimplemented!()` fail the strict check even though their manifest
levels are `warn`.
