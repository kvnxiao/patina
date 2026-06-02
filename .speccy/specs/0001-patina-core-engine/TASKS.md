---
spec: SPEC-0001
spec_hash_at_generation: 1de0928eaf46ec201800ccb02d043a41dc0fabb10539efab2be7ad57f313346b
generated_at: 2026-06-02T18:41:55Z
---
# Tasks: SPEC-0001 Patina core engine — transactional apply with apply/status/rollback CLI

<task id="T-001" state="completed" covers="REQ-001 REQ-002">
## Land the workspace shape and async tokio foundation

The repository root already carries a workspace `Cargo.toml` with both
`patina-core` and `patina-cli` listed, but the per-crate manifests do
not yet declare the SPEC-mandated direct dependencies and the library
is not yet async. This task delivers both: a buildable workspace whose
metadata satisfies REQ-001, plus an async `patina-core` whose three
public entry points compile under `#[tokio::main]` in `patina-cli`.

REQ-001 work — make the metadata true:

- Add `thiserror` as a direct dependency in `patina-core/Cargo.toml`.
- Confirm `patina-core/Cargo.toml` does **not** declare `anyhow` —
  the hard rule is `anyhow` lives in the binary only.
- Add `anyhow` as a direct dependency in `patina-cli/Cargo.toml`.
- Verify both manifests inherit `edition = "2024"`, `rust-version`
  matching the workspace MSRV (`1.95`), and `license = "MIT"`; fix
  any field that drifts.
- Confirm `patina-cli` lists `patina-core` as a direct path
  dependency.
- Pin new deps to recent published majors with no wildcards
  (`deny.toml` forbids them).

REQ-002 work — make the library async:

- Add `tokio` as a direct dependency in `patina-core/Cargo.toml`
  with the feature set `rt-multi-thread`, `fs`, `process`,
  `signal`, `sync`, `time`, `io-util`, `macros`.
- Add `tokio` to `patina-cli/Cargo.toml` too (the `#[tokio::main]`
  proc-macro lives there); features `macros` and `rt-multi-thread`
  at minimum.
- Introduce a top-level `EngineError` enum in
  `patina-core/src/error.rs` built from `thiserror`.
- Expose three public async entry points from
  `patina-core/src/lib.rs`: `pub async fn apply(...)`,
  `pub async fn status(...)`, `pub async fn rollback(...)`, each
  returning `Result<_, EngineError>`. Bodies may return a
  placeholder error variant for now — no `todo!()` / `panic!()`
  per REQ-024.
- Annotate `patina-cli/src/main.rs` with `#[tokio::main]`. Keep the
  existing `--version` short-circuit and add a call site that
  awaits one of the public `patina-core` entry points (stub args
  are fine — the goal is wiring proof, not real apply).
- Do **not** introduce `anyhow::Result` in `patina-core`; the library
  returns `Result<_, EngineError>` and the CLI wraps that into
  `anyhow::Result` at the call site.

<task-scenarios>
Given the repository at HEAD after this task,
when `cargo metadata --format-version 1 --no-deps` runs from the
workspace root,
then the JSON output's `packages` array contains entries whose
`name` fields are `patina-core` and `patina-cli`, each carrying
`"edition": "2024"`, `"license": "MIT"`, and a `rust_version` of
at least `1.95`.

Given the repository at HEAD,
when `cargo build --workspace --locked` runs with the workspace MSRV
toolchain,
then the build exits 0 with no warnings.

Given the same checkout,
when `cargo tree --manifest-path patina-core/Cargo.toml --depth 1` is
inspected,
then `thiserror` appears as a direct dependency and `anyhow` does
not.

Given the same checkout,
when `cargo tree --manifest-path patina-cli/Cargo.toml --depth 1` is
inspected,
then both `anyhow` and `patina-core` appear as direct dependencies.

Given the repository at HEAD,
when `cargo check --workspace --locked` runs,
then the command exits 0 and `patina-core/src/lib.rs` declares
three `pub async fn` entry points named `apply`, `status`, and
`rollback`, each returning a typed `Result`.

Given the same checkout,
when `patina-cli/src/main.rs` is inspected,
then the `main` function carries the `#[tokio::main]` attribute and
the binary uses `.await` on at least one `patina-core` entry point.

Given the same checkout,
when `cargo tree --manifest-path patina-core/Cargo.toml --depth 1
--features default` runs,
then `tokio` appears with the features `rt-multi-thread`, `fs`,
`process`, `signal`, `sync`, `time`, `io-util`, `macros` enabled.

Suggested files: `Cargo.toml`, `Cargo.lock`,
`patina-core/Cargo.toml`, `patina-core/src/lib.rs`,
`patina-core/src/error.rs`, `patina-cli/Cargo.toml`,
`patina-cli/src/main.rs`
</task-scenarios>
</task>

<task id="T-002" state="completed" covers="REQ-024">
## Enforce no-panic Clippy lint set workspace-wide

The workspace `Cargo.toml` already declares the relevant
`[workspace.lints.clippy]` denials (`unwrap_used`, `expect_used`,
`panic`, `unreachable`, plus `todo` / `unimplemented` as warnings).
Promote `todo` and `unimplemented` to `deny` so the lint set matches
REQ-024 exactly (the SPEC enumerates all six as denied).

Then fix the misplaced `allow-expect-in-tests = true` knob: it
currently sits under `[profile.release]` in `Cargo.toml` where Cargo
ignores it. Move it into `clippy.toml` at the workspace root —
`clippy.toml` is the only file Clippy reads for this configuration
key.

Verify the lint chain end-to-end:

1. `cargo clippy --workspace --all-targets --locked -- -D warnings`
   exits 0 on a clean tree.
2. Inserting a deliberate `foo.unwrap()` into a non-test path inside
   `patina-core/src/lib.rs` makes the same command fail with a
   `clippy::unwrap_used` error pointing at the offending line.
3. Inserting `.expect("descriptive message")` inside a
   `#[cfg(test)]` module does **not** trigger the lint, because of
   `allow-expect-in-tests`.

Update the GitHub Actions workflow under `.github/workflows/ci.yml`
only if the lint step is missing or invoked with the wrong flags;
otherwise leave CI alone.

<task-scenarios>
Given the repository at HEAD after this task,
when `cargo clippy --workspace --all-targets --locked -- -D warnings`
runs against an unmodified tree,
then the command exits 0.

Given a working tree where a contributor inserted `foo.unwrap()` on
a non-test line of `patina-core/src/lib.rs`,
when `cargo clippy --workspace --all-targets -- -D warnings` runs,
then the command exits non-zero and the error output contains both
`clippy::unwrap_used` and the substring `patina-core/src/lib.rs`.

Given the same checkout but with the offending `unwrap()` removed
and a `.expect("descriptive message")` added inside a
`#[cfg(test)]` module instead,
when the same Clippy invocation runs,
then the command exits 0.

Given the workspace `clippy.toml`,
when the file is read,
then it contains the line `allow-expect-in-tests = true` and the
same key is absent from `Cargo.toml`.

Suggested files: `Cargo.toml`, `clippy.toml`,
`.github/workflows/ci.yml`
</task-scenarios>
</task>

<task id="T-003" state="completed" covers="REQ-003 REQ-004">
## Discover the repository and enumerate modules with a flat depth-1 layout

Add `patina_core::discovery` covering two adjacent responsibilities:
resolving the dotfiles repository path, then enumerating modules
under it. Both live in the same module because both parse the root
`patina.toml`'s `[patina]` table to validate the `root = true` flag.

**Repository discovery (REQ-003).** Resolve the dotfiles repository
path through three sources in priority order:

1. The `PATINA_REPO` environment variable, when set and non-empty.
2. An upward walk from the current working directory looking for a
   `patina.toml` whose `[patina]` table contains `root = true`. The
   walk stops at the filesystem root.
3. A persisted default path stored under the per-machine state
   directory at `<state>/patina/default_repo` (a single line of
   text). T-005 lands the state-directory resolution and creates
   the parent directory; this task reads the file if present.

When all three sources fail, return a typed error variant that names
every source tried; T-020 maps this to exit code 1. The resolved
path must be a directory containing a parseable `patina.toml` with
`root = true`. Canonicalize the resolved path with absolute form
(T-009 lands the canonical-path helper; for this task call
`Utf8PathBuf::canonicalize_utf8` directly and let T-009 swap in the
lexical-fallback helper later).

The `clap`-derived CLI struct in `patina-cli` must not contain a
`--repo` flag on any subcommand — verified by inspecting the
generated `--help` output.

**Module enumeration (REQ-004).** Walk the resolved repository root
and return a `Vec<ModuleHandle>` ordered alphabetically by directory
name. Each handle carries the module name and absolute path. The
engine recognises exactly two depths:

- The root `patina.toml` with `[patina].root = true`.
- Per-module `patina.toml` files in immediate subdirectories of the
  root that **omit** the `root` key.

Three failure modes must produce distinguishable typed errors:

- A `patina.toml` at depth ≥ 2 (e.g. `zsh/plugins/patina.toml`) —
  error names the offending path and the phrase
  `maximum module depth`.
- A non-root `patina.toml` declaring `root = true` — error names
  the file and the unexpected `root` key.
- A root `patina.toml` omitting `root = true` — error names the
  file and the missing key.

<task-scenarios>
Given a temporary directory `T` containing `patina.toml` whose
`[patina]` table sets `root = true`,
when `PATINA_REPO=T patina apply --yes` runs from an unrelated CWD,
then the engine resolves the repository root to `T`.

Given a temporary directory `T` with `T/patina.toml` (root) and a
CWD at `T/zsh/`,
when `patina apply --yes` runs with `PATINA_REPO` unset,
then the engine walks up from `T/zsh/`, finds `T/patina.toml`, and
the resolved repository root equals `T`.

Given a CWD outside any Patina repository, no `PATINA_REPO`, and no
`default_repo` file under the per-machine state directory,
when `patina apply` runs,
then the process exits with code 1 and stderr contains the
substrings `PATINA_REPO`, `walk-up`, and `persisted default`.

Given the generated `patina --help` output for every subcommand,
when the text is scanned,
then no line contains `--repo`.

Given a temporary repository `T` with files
`T/patina.toml` (containing `[patina]\nroot = true`),
`T/zsh/patina.toml`, and `T/nvim/patina.toml`,
when the engine discovers modules in `T`,
then the result is a module set of exactly `{nvim, zsh}` ordered
alphabetically and both module paths are absolute.

Given a temporary repository `T` with `T/patina.toml` (root) and
`T/zsh/plugins/patina.toml`,
when the engine discovers modules in `T`,
then discovery fails with a typed error whose Display contains
`zsh/plugins/patina.toml` and the phrase `maximum module depth`.

Given a temporary repository `T` with `T/patina.toml` (root) and
`T/zsh/patina.toml` that itself contains `[patina]\nroot = true`,
when the engine discovers modules in `T`,
then discovery fails with a typed error whose Display names the
offending file and the unexpected `root` key.

Given a temporary repository `T` whose root `T/patina.toml` lacks
`root = true`,
when the engine discovers modules in `T`,
then discovery fails with a typed error whose Display names the
file and the missing `root = true` key.

Suggested files: `patina-core/src/discovery/mod.rs`,
`patina-core/src/discovery/repo.rs`,
`patina-core/src/discovery/modules.rs`,
`patina-core/src/error.rs`, `patina-cli/src/cli.rs`,
`patina-core/tests/repo_discovery.rs`,
`patina-core/tests/module_discovery.rs`
</task-scenarios>
</task>

<task id="T-004" state="completed" covers="REQ-005 REQ-006">
## Parse the `[[file]]` and `[[hook]]` TOML schemas

Introduce `patina_core::config` owning the TOML schema for both
table arrays declared in a module's `patina.toml`. This task covers
parsing and validation only; execution semantics live in T-014
(file mode executors) and T-015 (hook execution).

**`[[file]]` schema (REQ-005).** Each entry parses into a
`FileEntry` struct with `source: Utf8PathBuf`,
`targets: Vec<Utf8PathBuf>` (always plural internally — single-target
entries become a one-element vec), and `mode: FileMode`. The
`FileMode` enum has five variants:

- `Symlink` — the default when `mode` is omitted.
- `SymlinkDir`
- `Copy`
- `CopyTree`
- `TemplateRender` — set automatically when the source ends in
  `.tmpl`; never declared by the user.

Parse-time rules:

1. The entry declares **exactly one** of `target` (string) or
   `targets` (non-empty array of strings). Declaring both, neither,
   or `targets = []` is a typed parse error naming both keys (or
   the missing key) and the XOR / non-empty rule.
2. The `mode` field, if present, must be one of `symlink`,
   `symlink-dir`, `copy`, `copy-tree`. Any other value (including
   `merge-json`, `merge-toml`, or `template`) is a typed parse
   error naming the offending value and listing the four accepted
   values. The fifth mode is implicit — derived from the `.tmpl`
   suffix on the source — and may never be declared.
3. A source with a `.tmpl` suffix that also declares any `mode`
   value is a typed parse error.

**`[[hook]]` schema (REQ-006).** Each entry parses into a `HookEntry`
struct with: `event: HookEvent` (the enum `PreApply | PostApply`),
`command: String`, `shell: Option<String>`, `when: Option<String>`
(stored as raw expression source — T-008 evaluates it later), and
`must_succeed: bool` defaulting to `true` when the key is omitted.

Parse-time rules:

1. An `event` value other than `pre_apply` or `post_apply` is a
   typed parse error whose Display contains the offending value
   and lists the two accepted values explicitly. `on_change` and
   `on_drift` are the canonical rejection cases.
2. A missing `must_succeed` field resolves to `true` after parse.
3. A `shell` value, if present, is stored verbatim — the
   shell-on-PATH check is deferred to T-015. At parse time, only
   the type (string) is validated.
4. A `when` value, if present, is stored as the raw expression
   source. Its MiniJinja-compatible compilation is deferred to
   T-008.

The output is a `ModuleConfig` struct carrying both vectors plus
the module's `[variables]` table (returned raw for T-006 to consume).

<task-scenarios>
Given a module declaring
`[[file]] source = "zshrc" target = "~/.zshrc" mode = "symlink"`,
when the engine parses the module's `patina.toml`,
then parsing succeeds and the resulting `FileEntry` has
`mode = Symlink` and `targets = ["~/.zshrc"]`.

Given a module declaring
`[[file]] source = "zshrc" target = "~/.zshrc"` with no `mode` key,
when the engine parses the module's `patina.toml`,
then parsing succeeds and the resulting `FileEntry` has
`mode = Symlink`.

Given a module declaring
`[[file]] source = "agent.toml" targets = ["~/a", "~/b"] mode = "copy"`,
when the engine parses the module's `patina.toml`,
then parsing succeeds, `mode = Copy`, and
`targets = ["~/a", "~/b"]`.

Given a module declaring `[[file]] mode = "merge-json" source = "x"
target = "y"`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display contains
`merge-json` and the substrings `symlink`, `symlink-dir`, `copy`,
and `copy-tree`.

Given a `[[file]]` entry declaring both
`target = "~/.claude/agent.toml"` and
`targets = ["~/.codex/agent.toml"]`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display names both
`target` and `targets` and contains the substring `exactly one`.

Given a `[[file]]` entry declaring neither `target` nor `targets`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display contains
`target`, `targets`, and the word `missing`.

Given a `[[file]]` entry declaring `targets = []`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display contains
`targets` and the substring `non-empty`.

Given a `[[file]]` entry declaring
`source = "foo.tmpl" target = "y" mode = "copy"`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display names the
`.tmpl` suffix and the implicit-template rule.

Given a module declaring
`[[hook]] event = "pre_apply" command = "echo hi"`,
when the engine parses the module's `patina.toml`,
then parsing succeeds and the resulting `HookEntry` has
`event = PreApply`, `command = "echo hi"`, and
`must_succeed = true`.

Given a module declaring
`[[hook]] event = "post_apply" command = "exit 0" must_succeed = false`,
when the engine parses the module's `patina.toml`,
then parsing succeeds and the resulting `HookEntry` has
`event = PostApply` and `must_succeed = false`.

Given a module declaring
`[[hook]] event = "on_change" command = "echo hi"`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display contains
`on_change` and the substrings `pre_apply` and `post_apply`.

Given a module declaring
`[[hook]] event = "pre_apply" command = "echo hi" when = "patina.os == 'macos'"`,
when the engine parses the module's `patina.toml`,
then parsing succeeds and the resulting `HookEntry.when` equals
`Some("patina.os == 'macos'")`.

Suggested files: `patina-core/src/config/mod.rs`,
`patina-core/src/config/file_entry.rs`,
`patina-core/src/config/hook_entry.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/config_file_entry.rs`,
`patina-core/tests/config_hook_entry.rs`
</task-scenarios>
</task>

<task id="T-005" state="completed" covers="REQ-016">
## Resolve the per-machine state directory on macOS, Linux, and Windows

Add `patina_core::state_dir` exposing a `resolve()` function that
returns the per-machine state directory path in canonical absolute
form, applying the OS-specific layout:

- **Linux:** `$XDG_STATE_HOME/patina/` when `XDG_STATE_HOME` is set
  and non-empty; otherwise `$HOME/.local/state/patina/`.
- **macOS:** `$HOME/Library/Application Support/patina/`.
- **Windows:** `%LOCALAPPDATA%\patina\`.

On first resolution, create the directory tree if absent: the
`<state>/patina/` root plus the two subdirectories `journal/` and
`backups/`. The files `profile`, `default_repo`, and `lock` are
created lazily by their respective owners (T-007, T-003, T-013).

The function must be idempotent — calling it twice on the same host
yields the same path and does not error if the directories already
exist. It must not write to the dotfiles repository.

Add `dirs` (or the equivalent platform-aware crate) as a direct
dependency if needed; check `deny.toml` allows the license.

<task-scenarios>
Given a Linux test host with `XDG_STATE_HOME` set to a tempdir `T`,
when the engine resolves the state directory,
then the resolved path equals `T/patina/`, the subdirectories
`T/patina/journal/` and `T/patina/backups/` exist, and the function
exits 0 even when called a second time.

Given a Linux test host with `XDG_STATE_HOME` unset and `HOME=H`,
when the engine resolves the state directory,
then the resolved path equals `H/.local/state/patina/`.

Given a macOS test host with `HOME=H`,
when the engine resolves the state directory,
then the resolved path equals `H/Library/Application Support/patina/`.

Given a Windows test host with `LOCALAPPDATA=L`,
when the engine resolves the state directory,
then the resolved path equals `L\patina\`.

Given a tempdir repository alongside a resolved state directory,
when a full `patina apply --yes` cycle completes,
then no file under the dotfiles repository directory was modified
by the engine (verified by mtime / hash comparison before and after).

Suggested files: `patina-core/src/state_dir.rs`,
`patina-core/Cargo.toml`,
`patina-core/tests/state_dir_resolution.rs`
</task-scenarios>
</task>

<task id="T-006" state="completed" covers="REQ-007">
## Layered variable resolution with the reserved `patina.*` namespace

Build `patina_core::variables` exposing a `Resolver` that composes
six layers in priority order from highest to lowest: CLI overrides
(`-v key=value`), per-machine variables (from a file under the
per-machine state directory resolved by T-005), per-profile variables
(from the active profile's TOML), per-module variables (from the
module's `patina.toml`'s `[variables]` table), repo-shared variables
(from the root `patina.toml`'s `[variables]` table), and built-ins
under the `patina.*` namespace.

Built-ins:

- `patina.os` — `"macos"`, `"linux"`, or `"windows"` (resolved at
  process start).
- `patina.arch` — `std::env::consts::ARCH`.
- `patina.hostname` — from `gethostname` or the platform equivalent.
- `patina.user` — from `whoami` / `USERNAME` env on Windows.
- `patina.home` — from `dirs::home_dir()`.
- `patina.profile` — the active profile name resolved by T-007;
  inject the resolution result lazily so this task does not depend
  on T-007 wiring being complete first.
- `patina.env.*` — a dynamic map exposing the process environment;
  `patina.env.FOO` resolves to the value of `$FOO` at apply time.

Reservation rules:

- Any user-set key starting with `patina.` at any layer (CLI,
  per-machine, per-profile, per-module, repo-shared) is rejected
  at resolution / parse with a typed error whose Display names the
  offending key and contains the substring `reserved`.
- Built-ins themselves are exempt — only user-set keys trigger the
  rejection.

Defer the strict-undefined error path for missing keys *inside
templates* to T-008 (MiniJinja owns that). This task is responsible
for the layered resolution table and the reservation rule alone.

<task-scenarios>
Given a repository whose root `patina.toml` declares
`[variables] email = "root@example.com"` and whose `zsh` module
declares `[variables] email = "module@example.com"`,
when the engine resolves variables with CLI override
`-v email=cli@example.com` and queries the `email` key for the
`zsh` module,
then the resolved value is `cli@example.com`.

Given a repository whose root `patina.toml` declares
`[variables] "patina.foo" = "bar"`,
when the engine parses the file,
then parsing fails with a typed error whose Display contains
`patina.foo` and the substring `reserved`.

Given a CLI invocation that includes `-v patina.os=foo`,
when the engine ingests the CLI override layer,
then the override is rejected with a typed error whose Display
contains `patina.os` and the substring `reserved`.

Given a process environment with `CI=true`,
when the engine resolves `patina.env.CI`,
then the resolved value is `"true"`.

Given a host whose platform reports `macOS`,
when the engine resolves `patina.os`,
then the resolved value is `"macos"`.

Suggested files: `patina-core/src/variables/mod.rs`,
`patina-core/src/variables/builtins.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/variables.rs`
</task-scenarios>
</task>

<task id="T-007" state="completed" covers="REQ-008">
## Resolve the active profile via env, persisted, auto-match, fallback

Add a `patina_core::profile` module that resolves the active profile
by composing four sources in priority order:

1. The `PATINA_PROFILE` environment variable.
2. A persisted profile name stored as a single line in
   `<state>/patina/profile` (the state directory is resolved by
   T-005).
3. An `[[auto_match]]` rule in the root `patina.toml` evaluated
   against the built-in variable context (T-006 provides the
   context; this task evaluates the predicate). Each `auto_match`
   entry has a `when` predicate and a `profile` name; rules are
   evaluated in declaration order and the first match wins.
4. A no-profile fallback — an empty profile name; profile-scoped
   variables and modules contribute nothing.

The `when` predicate evaluates through the same MiniJinja machinery
T-008 sets up. For this task, accept that T-008 may not yet be
landed: if the MiniJinja path is unavailable, evaluate against a
narrowly typed predicate API (e.g., just `patina.hostname == "x"`)
or stub the predicate path behind a trait that T-008 fills in later.
The user-visible behaviour at SPEC scenario level must hold once
both tasks land.

Verify the clap-derived parser exposes **no** `--profile` flag on
any subcommand.

<task-scenarios>
Given a tempdir repository with no `[[auto_match]]` rules and a
state directory containing no persisted profile,
when `PATINA_PROFILE=work patina apply --yes --json` runs,
then the JSON output's top-level `profile` field equals `"work"`.

Given a tempdir repository whose root `patina.toml` declares
`[[auto_match]] when = "patina.hostname == 'CHK-host'" profile = "desktop"`
and a host configured to report hostname `CHK-host`,
when `patina apply --yes --json` runs with `PATINA_PROFILE` unset
and no persisted choice,
then the JSON output's `profile` field equals `"desktop"`.

Given a tempdir repository with no `[[auto_match]]` rules, no env
var, and a state directory containing `profile` with content `home`,
when `patina apply --yes --json` runs,
then the JSON output's `profile` field equals `"home"`.

Given a tempdir repository with no env, no persisted choice, and no
matching `[[auto_match]]`,
when `patina apply --yes --json` runs,
then the JSON output's `profile` field is the empty string.

Given the generated `patina --help` output for every subcommand,
when the text is scanned,
then no line contains `--profile`.

Suggested files: `patina-core/src/profile.rs`,
`patina-cli/src/cli.rs`,
`patina-core/tests/profile_resolution.rs`
</task-scenarios>
</task>

<task id="T-008" state="completed" covers="REQ-009">
## Single MiniJinja environment with `UndefinedBehavior::Strict` for `.tmpl` and `when`

Introduce `patina_core::template` owning a single
`minijinja::Environment` instance configured with
`UndefinedBehavior::Strict`. The same instance serves two callers:
rendering `*.tmpl` files into target paths and evaluating the `when`
expressions on `[[file]]` and `[[hook]]` entries.

For both render and `when` evaluation, the variable context is the
six-layer resolved table from T-006 (already serialized into a
`minijinja::Value`). Add an adapter that builds the `Value` from the
resolver output once per apply invocation and reuses it across all
templates and predicates.

Strict-undefined semantics:

- A `{{ undefined_var }}` reference inside a template body produces
  a typed engine error during plan computation; the Display contains
  the variable name.
- A `when = "undefined_var == 'x'"` predicate evaluates to a typed
  engine error during plan computation; the Display contains the
  variable name.
- The Jinja2-inherited carve-out for `{% else %}` blocks holds: a
  template body `{% if defined %}{{ undefined_var }}{% else %}fallback{% endif %}`
  with `defined` unset renders `fallback` rather than failing.

Add `minijinja` as a direct dependency in `patina-core/Cargo.toml`.

<task-scenarios>
Given a template `gitconfig.tmpl` containing
`[user]\nemail = {{ user_email }}` and no variable named
`user_email` in any resolution layer,
when `patina apply --yes` runs,
then the command exits 1 and stderr contains the substring
`user_email`.

Given a `[[file]]` entry with
`when = "patina.os == 'macos' and missing_var"` and no `missing_var`
in the resolved variable context,
when plan computation runs,
then the engine returns a typed error whose Display contains
`missing_var`.

Given a template containing
`{% if defined %}{{ undefined_var }}{% else %}fallback{% endif %}`
and no variable named `defined`,
when the engine renders the template against an empty user-variable
table,
then the rendered output is `fallback` and no error fires.

Given the same single MiniJinja environment used to render a
`.tmpl` and to evaluate a `[[hook]].when` predicate in the same
apply pipeline,
when the engine inspects its internal state,
then both calls share the same `minijinja::Environment` instance
(verifiable by a single shared `Arc<Environment>` in the engine
constructor wiring).

Suggested files: `patina-core/Cargo.toml`,
`patina-core/src/template/mod.rs`,
`patina-core/src/template/strict.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/template_strict.rs`
</task-scenarios>
</task>

<task id="T-009" state="completed" covers="REQ-010">
## Absolute-path canonicalization with lexical fallback for non-existent paths

Add `patina_core::paths` exposing a `canonicalize` helper that
takes a `Utf8Path` and returns a `Utf8PathBuf` in canonical absolute
form. The helper has two branches:

1. If the path already exists on disk, canonicalize through the
   filesystem — resolve symlinks and `.` / `..` components.
2. If the path does not exist (typical for target paths whose
   parent directories have yet to be created), canonicalize
   lexically: join with the canonical absolute parent (if the
   parent exists) or with the canonical current working directory.

Expand `~` to `patina.home` (the resolved built-in from T-006) when
it appears at the start of a path; this is the user-input variant.

Wire the helper into the discovery layer (T-003) — replacing the
`canonicalize_utf8` stub — into module enumeration (T-003), into
file-entry parsing (T-004), and into journal writes (T-010) so
every absolute path that ever surfaces in error messages, journal
records, or user-facing output is canonical. Relative paths must
never appear in journal records.

Use `camino` types throughout — `Utf8PathBuf` / `Utf8Path`. The
public surface is
`pub fn canonicalize(p: &Utf8Path) -> Result<Utf8PathBuf, PathError>`.

<task-scenarios>
Given a CWD `/tmp/work` and a tempdir repository at `/tmp/work/dot`,
when `PATINA_REPO=./dot patina apply --yes --json` runs,
then the JSON output's `repo_root` field equals `/tmp/work/dot` and
contains no `.` or `..` segments.

Given a target path `~/.config/foo/bar.conf` whose `~/.config/foo/`
parent directory does not yet exist,
when the engine canonicalizes the target during plan computation,
then the resolved path is the lexical join of the canonical home
directory and `.config/foo/bar.conf`, not an error.

Given a tempdir repository whose `~/.config` is itself a symlink to
`~/dotfiles-config`,
when the engine canonicalizes a target path of
`~/.config/foo.conf` and the parent exists,
then the resolved path passes through the symlink (resolves to
`~/dotfiles-config/foo.conf`).

Given a journal file written by an apply against a relative
`PATINA_REPO`,
when the journal record is decoded,
then every path field in the record is absolute and canonical.

Suggested files: `patina-core/src/paths.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/path_canon.rs`
</task-scenarios>
</task>

<task id="T-010" state="completed" covers="REQ-011 REQ-012">
## Postcard plan journal with single upfront fsync plus progress cursor

Add `patina_core::journal` owning the binary plan file and the
progress cursor. The plan file lives at
`<state>/patina/journal/<ts>.plan` and is encoded with `postcard`.
Each plan record opens with a version envelope: a fixed `u16` at
offset 0 carrying the current major version (start at `1`). The
decoder reads the envelope first and refuses any plan whose major
version exceeds the running binary's compiled value, returning a
typed `JournalVersionMismatch` error.

Write semantics for the plan file:

1. Compute the full plan (list of file operations and hook
   invocations).
2. Serialize the plan to the binary file via `postcard`.
3. Fsync the plan file.
4. Fsync the parent directory.
5. Only after both fsyncs return: begin mutations.

Use `<ts>` of the form `YYYYMMDDTHHMMSSZ` (UTC, lexicographically
sortable). The plan filename is the only place where wall-clock
timestamps surface — user-facing stdout has none (REQ-021, T-021).

Progress cursor at `<state>/patina/journal/<ts>.progress`:

- Append-only; one record per completed operation in operation
  order.
- Each record encodes the operation's plan index plus a completion
  marker.
- Do **not** fsync per operation; rely on the filesystem-probe
  recovery path (T-011) as the source of truth.
- The progress file's last record may lag the actual filesystem
  state by at most one operation after a crash.

After every operation in the plan completes and any post-hook
side-effects settle, write `<ts>.COMMIT` with a single fsync of
both the sentinel file and its parent directory. The plan file is
deleted only after the `COMMIT` sentinel is fsync'd.

Provide a syscall-counting test hook (e.g., behind a `#[cfg(test)]`
trait) so T-011 can assert no per-op `fsync` calls fire on the
progress file.

<task-scenarios>
Given a tempdir repository with one `[[file]]` entry,
when `patina apply --yes` runs and the process is killed
(`SIGKILL` on POSIX, `TerminateProcess` on Windows) immediately
after `flush_plan_and_fsync` returns but before the first mutation,
then `<state>/patina/journal/` contains exactly one `<ts>.plan`
file, no `<ts>.COMMIT` sentinel, and at most one `<ts>.progress`
file (which may be empty).

Given a tempdir repository declaring three file operations and a
test harness that records `fsync` calls via the syscall-counting
hook,
when `patina apply --yes` runs successfully,
then the recorded fsyncs include exactly one on the plan file, one
on the journal parent directory, one on the `COMMIT` sentinel, and
zero per-operation fsyncs on the progress file.

Given a plan file whose version envelope's `u16` at offset 0 is
`u16::MAX`,
when the engine attempts to decode it on a binary whose compiled
major version is `1`,
then decoding fails with a typed `JournalVersionMismatch` error
whose Display names both versions.

Given a successful apply that wrote `<ts>.COMMIT`,
when the engine starts a subsequent apply,
then the prior apply's `<ts>.plan` and `<ts>.progress` files have
been deleted and only the `<ts>.COMMIT` sentinel remains alongside
new plan files.

Suggested files: `patina-core/Cargo.toml`,
`patina-core/src/journal/mod.rs`,
`patina-core/src/journal/plan.rs`,
`patina-core/src/journal/progress.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/journal_fsync_shape.rs`
</task-scenarios>
</task>

<task id="T-011" state="completed" covers="REQ-013">
## Crash recovery converges backward to pre-apply state via filesystem probe

Add a `recovery` submodule under `patina_core::journal`. On apply
startup, before computing a new plan, the engine looks for orphan
plan files (`<ts>.plan` without a matching `<ts>.COMMIT` or
`<ts>.ROLLED_BACK`). For each orphan:

1. Decode the plan (re-using the version envelope check from T-010).
2. For each operation in the plan, probe the filesystem to determine
   whether it completed, partially completed, or did not start.
   Probing uses the operation's target path and the expected
   pre-state hash (which the plan records before execution).
3. Reverse every completed operation using the backup directory
   (T-012) and the journaled inverse semantics. Symlink ops are
   reversed by deleting the new symlink and restoring the original;
   copy ops are reversed by restoring the byte content from
   backup; freshly-created targets (no backup) are deleted.
4. After all reversals finish, delete the orphan plan file and its
   progress cursor.

Recovery is **idempotent**: running it twice in a row yields the
same final state as running it once. It is also **backward-only** —
the engine never finishes a partial apply forward; it always rolls
back to pre-apply state (per DEC-011). After recovery completes,
the engine continues with the user's new apply invocation as if no
prior partial work had occurred.

The progress cursor (T-010) is advisory only — recovery does not
trust its last record; it probes the filesystem.

<task-scenarios>
Given a tempdir repository, an apply that completed 3 of 5 file
operations before SIGKILL, and the corresponding backup directory
intact,
when `patina apply --yes` is next invoked,
then before any new mutation occurs the engine restores the 3
previously-overwritten targets from backups to their pre-apply
content, removes the orphaned `<ts>.plan` and `<ts>.progress`
files, then proceeds with the new plan from scratch.

Given a crashed apply where the plan was fsync'd but no mutation
had executed,
when recovery runs,
then no targets are touched, the orphaned `<ts>.plan` and
`<ts>.progress` files are deleted, and the engine proceeds with the
new plan against an unchanged filesystem.

Given a state directory in which recovery completed once and was
invoked again with no intervening apply,
when recovery runs the second time,
then the final filesystem state matches the first recovery's final
state and no errors fire (idempotence).

Given a crashed apply mid-execution and a corrupted progress
cursor whose last record reports more operations completed than
the filesystem reflects,
when recovery runs,
then the engine probes the filesystem to determine actual
completion and ignores the progress cursor's lying record.

Suggested files: `patina-core/src/journal/recovery.rs`,
`patina-core/src/journal/probe.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/recovery_crash.rs`
</task-scenarios>
</task>

<task id="T-012" state="completed" covers="REQ-014 REQ-015">
## Backups before overwrite, mirrored target paths, retention by count of ten

Add `patina_core::backups` covering two responsibilities:

**Backup-on-overwrite (REQ-014):** Before the engine overwrites any
pre-existing target (including replacing a regular file with a
symlink), copy the original to
`<state>/patina/backups/<ts>/<mirrored-absolute-target-path>`. The
mirrored path is the target's absolute path with the platform's
filesystem root stripped (or with an OS-portable encoding so the
backup directory can hold paths from any volume). Targets that did
not pre-exist do not produce a backup entry. The dotfiles
repository is never written to during apply.

**Retention by count (REQ-015):** After the engine writes the
`COMMIT` sentinel for an apply, sort the existing
`<state>/patina/backups/*` subdirectories by timestamp (lex sort on
the directory name) and remove every subdirectory older than the
tenth most recent. The retention is exactly ten cycles after the
just-completed apply. GC runs only on a successful apply — a failed
apply (no `COMMIT`) does not trigger GC.

No `patina gc` subcommand exists in the CLI; this housekeeping is
implicit.

<task-scenarios>
Given a tempdir HOME containing a pre-existing `~/.zshrc` with
content "original" and a Patina repository declaring a symlink
target on `~/.zshrc`,
when `patina apply --yes` runs successfully,
then `<state>/patina/backups/<ts>/<mirrored-path>/.zshrc` is a
regular file containing the bytes "original" and the final
`~/.zshrc` is a symbolic link.

Given a tempdir HOME with no pre-existing `~/.gitconfig` and a
template-render `[[file]]` entry pointing at `~/.gitconfig`,
when `patina apply --yes` runs successfully,
then `<state>/patina/backups/<ts>/` exists but contains no entry
for `.gitconfig`.

Given a tempdir state directory with
`<state>/patina/backups/` containing 15 timestamped subdirectories
and a successful apply that completes,
when the engine finishes writing `COMMIT` and runs retention GC,
then `<state>/patina/backups/` contains exactly 10 subdirectories
— the newest plus the 9 most recent prior ones — and the 5 oldest
have been removed.

Given a tempdir state directory with 3 historical backup
subdirectories and an apply that fails before the `COMMIT`
sentinel is written,
when the engine surfaces the failure to the CLI,
then `<state>/patina/backups/` still contains the 3 historical
subdirectories (no GC ran) plus any partial backup directory for
the failed attempt — none of the 3 priors were touched.

Given a clean dotfiles repository at HEAD,
when a successful apply runs against it,
then `git status --porcelain` of the dotfiles repository directory
shows no changes attributable to the engine.

Given the generated `patina --help` output,
when the text is scanned,
then no line contains a `gc` subcommand or `--gc` flag.

Suggested files: `patina-core/src/backups/mod.rs`,
`patina-core/src/backups/mirror.rs`,
`patina-core/src/backups/retention.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/backups.rs`,
`patina-core/tests/backup_retention.rs`
</task-scenarios>
</task>

<task id="T-013" state="completed" covers="REQ-023">
## Advisory file lock with shared (status) and exclusive (apply/rollback) modes

Add `patina_core::lock` wrapping `fs2`'s advisory file lock around
`<state>/patina/lock`. Two acquisition modes:

- **Exclusive** — required by `apply` and `rollback`. The mutating
  subcommands hold the lock for the full apply duration. A second
  process attempting an exclusive lock blocks. If the wait exceeds
  60 seconds, the second process returns a typed
  `LockTimeout(LockKind::Exclusive)` error which T-020 maps to exit
  code 4.
- **Shared** — required by `status`. Multiple shared lock holders
  may coexist. A shared lock holder blocks an exclusive acquirer
  and vice versa. If `status` waits more than 5 seconds for the
  shared lock, it emits a warning to stderr and proceeds without
  the lock (the warn-and-proceed behaviour is REQ-023's escape
  hatch for the read-only case).

OS-level lock release on process death must work: if a process
holding the lock crashes, the OS releases the lock automatically
and the next process acquires cleanly. Test this on at least one
POSIX target and Windows in the integration suite.

Add `fs2` (or `fd-lock` per the DEC-005 fallback) as a direct
dependency in `patina-core/Cargo.toml`.

<task-scenarios>
Given a test harness that spawns two `patina apply --yes` processes
against the same tempdir state directory and synchronises their
start within a 100 ms window,
when both processes complete,
then `<state>/patina/journal/` contains two `<ts>.plan` files whose
timestamp ranges (start to `COMMIT`) are non-overlapping (i.e. the
second apply waited for the first to release the exclusive lock).

Given a process A holding the exclusive lock and process B
attempting `patina apply --yes` with a 60-second cap (parameterised
in the test harness so it can run quickly),
when process A holds the lock past the cap,
then process B exits with code 4 and stderr contains a
`lock timeout` / `exclusive` message.

Given a process A holding the exclusive lock and a concurrent
`patina status` invocation,
when the status invocation has waited 5 seconds,
then it emits a warning to stderr naming the lock and proceeds to
read state; the status process exits with code 0.

Given a process holding the lock that is killed with SIGKILL
(POSIX) or `TerminateProcess` (Windows),
when the next `patina apply --yes` is invoked,
then the new process acquires the lock cleanly (the OS released
it on process death) and the apply proceeds.

Suggested files: `patina-core/Cargo.toml`,
`patina-core/src/lock.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/lock_concurrency.rs`
</task-scenarios>
</task>

<task id="T-014" state="completed" covers="REQ-005">
## Implement the five file-mode executors with multi-target fan-out

Add `patina_core::apply::executors` covering the execution side of
each `FileMode` variant from T-004:

- **`Symlink`** — when the source is a regular file, create a
  symbolic link at the target whose readlink target equals the
  canonical absolute source path. When the source is a directory,
  walk the directory and create one symbolic link per file under
  the source at the mirrored target path (the default mode's
  per-file walk; see CHK-041 and the SPEC's note that atomic
  directory symlinks require an explicit `symlink-dir`).
- **`SymlinkDir`** — create a single symbolic link at the target
  pointing at the source directory. Do not walk into the source.
- **`Copy`** — copy the source file bytes to the target.
- **`CopyTree`** — recursively copy the source directory tree to
  the target.
- **`TemplateRender`** — render the source `.tmpl` through the
  T-008 MiniJinja environment and write the result to the target
  (with the `.tmpl` suffix stripped). The target is a regular
  file, not a symlink.

Multi-target fan-out: every executor accepts a slice of target
paths. For symlink-family modes, fan out the link to each target
(both linking at the same canonical source path). For copy modes,
copy the source bytes to each target. For template render, render
the template **once** against the resolved variable context and
write the same rendered bytes to each target.

The executor returns per-target completion records (one per
`(source, target_i)` pair) so T-010 can record one progress entry
per target — keep the per-target granularity throughout so backups,
status (T-017), and rollback (T-018) inherit it without
special-casing.

Refuse Windows symlink creation that lacks Developer Mode by
surfacing a typed `EngineError::WindowsSymlinkPermission` variant
(the prompt/elevate flow lives in SPEC-0002; this SPEC only needs
to surface the typed error).

<task-scenarios>
Given a tempdir repository with `T/patina.toml`,
`T/zsh/patina.toml` declaring
`[[file]] source = "zshrc" target = "~/.zshrc" mode = "symlink"`,
and `T/zsh/zshrc` with arbitrary content,
when `patina apply --yes` runs against a HOME pointing at a
tempdir,
then `$HOME/.zshrc` is a symbolic link whose readlink target equals
the canonical absolute path of `T/zsh/zshrc`.

Given a tempdir repository declaring a `[[file]]` with
`source = "config" target = "~/.config/nvim" mode = "symlink-dir"`,
when `patina apply --yes` runs,
then `~/.config/nvim` is a single symbolic link whose readlink
target equals the canonical path of `<module>/config` — no per-file
walk has occurred.

Given a tempdir repository declaring a `[[file]]` with
`source = "gitconfig.tmpl"`, `target = "~/.gitconfig"`, no
explicit mode, and content
`[user]\n    email = {{ patina.profile_email }}` plus a resolved
variable `patina.profile_email = "kevin@example.com"`,
when `patina apply --yes` runs,
then `$HOME/.gitconfig` is a regular file whose content is
`[user]\n    email = kevin@example.com` and `$HOME/.gitconfig.tmpl`
does not exist.

Given a tempdir repository with a `[[file]]` entry declaring
`source = "agent.toml" targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"] mode = "symlink"`
and `<module>/agent.toml` with arbitrary content,
when `patina apply --yes` runs,
then both `$HOME/.claude/agent.toml` and `$HOME/.codex/agent.toml`
are symbolic links whose readlink targets equal the canonical
path of `<module>/agent.toml`.

Given the same fixture but with `mode = "copy"`,
when `patina apply --yes` runs,
then both target paths are regular files whose byte content equals
the source.

Given a tempdir repository declaring
`[[file]] source = "agent.toml.tmpl" targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`
with the template body `name = {{ patina.user }}`,
when `patina apply --yes` runs,
then both target paths (with `.tmpl` stripped) are regular files
containing the same MiniJinja-rendered output and the engine
performed exactly one template render (not two).

Given a Windows host without Developer Mode and a `[[file]]` with
`mode = "symlink"`,
when `patina apply --yes` attempts the symlink op,
then the engine returns a typed
`EngineError::WindowsSymlinkPermission` variant; the CLI surfaces
the underlying message via exit code 1 (the elevate flow is
deferred to SPEC-0002).

Suggested files: `patina-core/src/apply/mod.rs`,
`patina-core/src/apply/executors.rs`,
`patina-core/src/apply/symlink.rs`,
`patina-core/src/apply/copy.rs`,
`patina-core/src/apply/template.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/executor_modes.rs`,
`patina-core/tests/multi_target.rs`
</task-scenarios>
</task>

<task id="T-015" state="completed" covers="REQ-006">
## Execute hooks with `must_succeed` semantics and the `--force-deploy` override

Add `patina_core::apply::hooks` covering hook execution. The
parser side (T-004) gives a `Vec<HookEntry>` per module; this task
runs them at the right times in the apply pipeline.

Order of operations within an apply:

1. Evaluate every hook's `when` predicate (via T-008 MiniJinja) and
   filter out hooks that evaluate `false`.
2. Run all `pre_apply` hooks in declaration order, before any file
   operation executes.
3. Run the file-mode executors (T-014) and persist their
   completion records.
4. Run all `post_apply` hooks in declaration order, after the last
   file operation completes but before `COMMIT` is written.

Hook failure semantics (with the `--force-deploy` override layered
on top):

- A `pre_apply` hook returning non-zero with `must_succeed = true`
  aborts the apply before any file operation executes. The CLI
  surface (T-016) maps this to exit code 2.
- A `post_apply` hook returning non-zero with `must_succeed = true`
  triggers rollback of every file operation in the apply via the
  T-011 / T-018 machinery (rollback uses backups + inverse ops).
  The CLI maps this to exit code 3.
- A hook with `must_succeed = false` that returns non-zero only
  warns on stderr; the apply proceeds.
- `patina apply --force-deploy` (T-016) overrides every hook in
  the current invocation to `must_succeed = false` regardless of
  its declared value, so failures always degrade to warnings.

The shell resolution rule from REQ-006: when `shell` is unset,
default to `bash` on macOS / Linux and `pwsh` on Windows. When
`shell` is set, the executable must resolve on PATH; an unresolved
shell is a typed error before any hook runs.

<task-scenarios>
Given a tempdir repository declaring
`[[hook]] event = "pre_apply" command = "false"` (default
`must_succeed = true`),
when `patina apply --yes` runs,
then no file operation executes, the process exits with code 2,
and stderr names the failing hook command.

Given a tempdir repository declaring a single file op and
`[[hook]] event = "post_apply" command = "exit 1"` (default
`must_succeed = true`),
when `patina apply --yes` runs,
then the file operation executes first, the hook returns non-zero,
the engine rolls back the file operation using the backup, and
the process exits with code 3.

Given the same fixture,
when `patina apply --yes --force-deploy` runs,
then the file operation executes, the hook returns non-zero, no
rollback fires, stderr contains a warning naming the hook, and
the process exits with code 0.

Given a hook declaring `when = "patina.os == 'macos'"` and a host
whose resolved `patina.os` is `linux`,
when the apply pipeline evaluates the hook,
then the hook is filtered out before execution.

Given a hook declaring `shell = "nonexistent-shell-xyz"`,
when the engine prepares to execute hooks,
then the engine returns a typed error naming the unresolved shell
binary before any hook actually runs and before any file op
executes; the process exits with code 1.

Given a host environment with `CI=true` and a hook declaring
`when = "patina.env.CI == 'true'"`,
when the apply pipeline evaluates the hook,
then the `when` predicate evaluates true and the hook runs.

Suggested files: `patina-core/src/apply/hooks.rs`,
`patina-core/src/apply/executor_pipeline.rs`,
`patina-core/src/error.rs`,
`patina-core/tests/hooks_must_succeed.rs`,
`patina-core/tests/hooks_force_deploy.rs`
</task-scenarios>
</task>

<task id="T-016" state="completed" covers="REQ-017">
## `patina apply` CLI surface with TTY prompt, `--yes`, `--force-deploy`, `--json`, `--pager`

Wire the `patina apply` subcommand in `patina-cli`. The clap-derived
parser accepts:

- `--yes` — apply unconditionally with no prompt regardless of TTY.
- `--force-deploy` — override every hook in this invocation to
  `must_succeed = false`. Combine with `--yes` for unattended
  deployment over flaky hooks.
- `--json` — emit a JSON envelope with fields `repo_root`,
  `profile`, `plan` (array of operations), and `result` (one of
  `previewed`, `applied`, `rolled_back`, or `aborted`). `--json`
  alone does **not** mutate (the apply is treated as a preview);
  `--json --yes` does mutate.
- `--pager=<delta|difft>` — pipe the rendered diff through the
  named external tool if it resolves on PATH; fall back to the
  embedded `similar`-rendered diff with a one-line stderr warning
  if absent.
- `-v key=value` (repeatable) — CLI variable overrides for T-006.

TTY-driven semantics (using the `is-terminal` crate):

- TTY + no `--yes`: render the diff to stdout, prompt
  `Apply? [y/N]` on stderr, read one line. On `y`/`Y` apply; on
  anything else exit 5.
- Non-TTY + no `--yes`: render the diff to stdout and exit 0
  without mutating.
- `--yes`: mutate regardless of TTY, no prompt.

Diff rendering uses the `similar` crate; line-level diff against
the resolved target content for copy/template modes, "old symlink
target → new symlink target" for symlink modes.

Add an `output::Reporter` abstraction layer that owns all
user-facing output (human and JSON). Both `--json` and human
output funnel through it; T-021 verifies the determinism property
on top of this abstraction. No direct `println!` outside the
reporter; use `tracing` macros for logs.

<task-scenarios>
Given a tempdir Patina repository declaring one symlink `[[file]]`
and a test harness simulating a non-TTY stdin,
when `patina apply` runs (no `--yes`),
then the process exits 0, no symlink is created, and stdout
contains the rendered diff.

Given a tempdir Patina repository declaring one symlink `[[file]]`
and a TTY-simulating harness that feeds `n` to stdin,
when `patina apply` runs (no `--yes`),
then the process exits with code 5, no symlink is created, and
stderr contains the `Apply? [y/N]` prompt text.

Given a tempdir Patina repository declaring one symlink `[[file]]`
and a TTY-simulating harness that feeds `y` to stdin,
when `patina apply` runs (no `--yes`),
then the process exits 0, the symlink target is created, and
stdout contains the rendered diff.

Given a tempdir repository and a clean state directory,
when `patina apply --json` runs,
then stdout contains a single JSON document whose `result` field
equals `previewed` and the filesystem under HOME has not been
mutated.

Given the same fixture,
when `patina apply --json --yes` runs,
then stdout contains a single JSON document whose `result` field
equals `applied` and the filesystem under HOME reflects the new
state.

Given a tempdir repository declaring a `[[hook]]` whose
`post_apply` command exits non-zero with `must_succeed = true`,
when `patina apply --json --yes` runs,
then the JSON document's `result` field equals `rolled_back` and
the process exits with code 3.

Given a host without `delta` on PATH,
when `patina apply --pager=delta --yes` runs,
then the apply succeeds, stdout contains the embedded `similar`
diff (not piped through `delta`), and stderr contains a one-line
warning naming the missing tool.

Given a CLI invocation with `-v email=cli@example.com -v profile=work`,
when `patina apply --yes --json` runs against a template that
references `{{ email }}`,
then the JSON document's `plan` shows the rendered target content
containing `cli@example.com`.

Suggested files: `patina-cli/src/cli.rs`,
`patina-cli/src/cmd/apply.rs`,
`patina-cli/src/output/reporter.rs`,
`patina-cli/src/output/diff.rs`,
`patina-cli/Cargo.toml`,
`patina-cli/tests/apply_cli.rs`
</task-scenarios>
</task>

<task id="T-017" state="completed" covers="REQ-018">
## `patina status` classifies every managed target as CLEAN / DRIFTED / MISSING / ORPHANED

Wire the `patina status` subcommand in `patina-cli`, backed by a
`patina_core::status` module. The command reads the latest
`COMMIT`-sentineled apply journal under `<state>/patina/journal/`,
walks every managed target in the recorded plan, and classifies
each into one of four states by comparing the recorded expected
hash to the live filesystem content:

- **CLEAN** — target exists and matches expected.
- **DRIFTED** — target exists but content differs from expected.
- **MISSING** — target was applied but no longer exists on disk.
- **ORPHANED** — target exists on disk but the **current** module
  plan no longer manages it (so it appears in a prior journal but
  not in the freshly-computed current plan).

Output:

- Human-readable default: one row per `(source, target_i)` pair
  with the target path, state, and a short note (e.g.
  `5 lines changed` for DRIFTED). Per the multi-target rule in
  REQ-005, a `[[file]]` entry with three targets produces three
  rows.
- `--json`: a top-level object with fields
  - `last_apply` — object with `at`, `user`, `host`.
  - `files` — array of `{path, state}` objects.
  - `clean`, `drifted`, `missing`, `orphaned` — aggregate counters.
    Multi-target entries contribute one count per target.

Acquire a **shared** lock during the read (T-013), with the
warn-and-proceed-after-5s fallback REQ-023 requires.

<task-scenarios>
Given a successful apply of three file operations against a tempdir
repository and no subsequent filesystem changes,
when `patina status --json` runs,
then the JSON output's `clean` counter is 3 and `drifted`,
`missing`, `orphaned` are each 0.

Given a successful apply that materialised `~/.gitconfig` (copy
mode), followed by a test step that appends bytes to
`~/.gitconfig`,
when `patina status --json` runs,
then the `drifted` counter is 1 and the `files` array contains an
entry with `path` resolving to `.gitconfig` and `state` equal to
`drifted`.

Given a tempdir repository with a `[[file]]` entry declaring
`source = "agent.toml" targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"] mode = "copy"`,
an applied state, and a test step that overwrites
`~/.codex/agent.toml` with different bytes,
when `patina status --json` runs,
then the `files` array contains two entries — one with `path`
resolving to `.claude/agent.toml` and `state = "clean"`, one with
`path` resolving to `.codex/agent.toml` and `state = "drifted"`
— the `clean` counter is at least 1, and the `drifted` counter is
at least 1.

Given a successful apply that materialised `~/.zshrc` followed by
a test step that deletes `~/.zshrc`,
when `patina status --json` runs,
then the `missing` counter is 1 and the `files` array contains an
entry with `path` resolving to `.zshrc` and `state = "missing"`.

Given a successful apply that materialised `~/.oldconfig` and a
subsequent change to the repository removing the `[[file]]` entry
that produced `~/.oldconfig`,
when `patina status --json` runs,
then the `orphaned` counter is 1 and the `files` array contains an
entry with `path` resolving to `.oldconfig` and
`state = "orphaned"`.

Given a process holding an exclusive lock for 6 seconds,
when `patina status --json` runs concurrently,
then status emits a warning to stderr at the 5-second mark naming
the lock and proceeds; status exits with code 0.

Suggested files: `patina-core/src/status/mod.rs`,
`patina-core/src/status/classify.rs`,
`patina-cli/src/cmd/status.rs`,
`patina-cli/tests/status_cli.rs`
</task-scenarios>
</task>

<task id="T-018" state="completed" covers="REQ-019">
## `patina rollback` reverses the last successful apply via the journal and backups

Wire the `patina rollback` subcommand in `patina-cli`, backed by a
`patina_core::rollback` module. The command:

1. Acquires the exclusive lock (T-013).
2. Finds the most recent `<ts>.COMMIT`-sentineled apply that is not
   already marked `<ts>.ROLLED_BACK`. If none exists, return a typed
   `NoPriorApply` error which the CLI surfaces as exit code 1.
3. Replays the journal's inverse operations using the corresponding
   `<state>/patina/backups/<ts>/` directory:
   - Targets that existed pre-apply (have a backup) are restored
     from the backup's bytes / mode.
   - Targets that were created fresh by apply (no backup) are
     deleted.
4. Writes `<ts>.ROLLED_BACK` and fsync's it (and its parent
   directory) so the `<ts>` no longer participates in
   `patina status`'s "last apply" computation.
5. Releases the lock.

Per-`[[file]]`-entry atomicity for multi-target entries (REQ-005 +
REQ-019 fan-out): every target in the entry reverts to pre-apply
state, or rollback fails for the entry and surfaces a typed error
without partial restore. Match the all-or-nothing semantic the
engine applies during crash recovery (T-011).

Honour `--yes` exactly like apply: no prompt under `--yes`; in a
TTY without `--yes`, show the planned rollback diff and prompt.

<task-scenarios>
Given a pre-existing `~/.zshrc` with content "original" and a
Patina apply that materialised it as a symlink to a repo file,
when `patina rollback --yes` runs,
then `~/.zshrc` is a regular file (not a symlink) with content
`"original"` and the journal contains a `<ts>.ROLLED_BACK`
sentinel.

Given a target `~/.gitconfig` that did not exist before apply,
when `patina rollback --yes` runs,
then `~/.gitconfig` no longer exists on disk.

Given no prior apply on a fresh state directory,
when `patina rollback --yes` runs,
then the process exits with code 1 and stderr names
`no prior apply found`.

Given a tempdir HOME with a pre-existing `~/.claude/agent.toml`
(content "old") and no pre-existing `~/.codex/agent.toml`, a
Patina apply that materialised both targets via a `[[file]]` entry
with `targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`
and `mode = "copy"` from a source containing "new",
when `patina rollback --yes` runs,
then `~/.claude/agent.toml` is a regular file with content "old",
`~/.codex/agent.toml` does not exist, and the journal contains a
`<ts>.ROLLED_BACK` sentinel.

Given a multi-target `[[file]]` rollback where restoring the
second target fails (e.g. permission error simulated by the test
harness),
when the rollback handler responds,
then the first target's restoration is reverted (the
restored-from-backup file is removed if it was created or
restored to its post-apply state if it was modified), the entry
ends up atomically post-apply for both targets, and the CLI
surfaces a typed `RollbackPartial` error with exit code 1.

Suggested files: `patina-core/src/rollback/mod.rs`,
`patina-core/src/rollback/replay.rs`,
`patina-cli/src/cmd/rollback.rs`,
`patina-cli/tests/rollback_cli.rs`,
`patina-core/tests/rollback_atomic.rs`
</task-scenarios>
</task>

<task id="T-019" state="completed" covers="REQ-020">
## `patina debug journal <path>` decodes a postcard plan into human-readable form

Add a hidden-but-documented `patina debug journal <path>`
subcommand. The clap-derived `patina debug` group acts as a
namespace for debugging subcommands; `journal` is its first
member. The subcommand:

1. Opens the file at `<path>` and reads the version envelope
   (T-010). On a version-mismatch — the file's major version
   exceeds the running binary's — return a typed
   `JournalVersionMismatch` error naming both versions and exit
   with code 1.
2. Decodes the postcard-encoded plan body.
3. Renders a human-readable view to stdout: one block per
   operation, identifying the operation's `mode`, `source`, and
   `target`; and the plan timestamp.
4. On a missing or unreadable `<path>`, return a typed error
   naming the path and exit with code 1.

The output format is documented (e.g. a one-line summary per op
followed by indented detail) but not stable across releases —
it is debug output, not machine-parsed.

Per REQ-021 (T-021), the debug output is **not** required to be
deterministic across runs; it is the only user-facing output path
allowed to contain wall-clock timestamps (the plan's recorded
timestamp).

<task-scenarios>
Given a tempdir repository, a successful `patina apply`, and the
resulting `<state>/patina/journal/<ts>.plan` file,
when `patina debug journal <state>/patina/journal/<ts>.plan` runs,
then stdout contains a substring matching one of `symlink`,
`symlink-dir`, `copy`, `copy-tree`, or `template-render`
corresponding to the modes declared in the test fixture, plus the
absolute path of at least one target, and the process exits 0.

Given a path that does not exist,
when `patina debug journal /nonexistent/path.plan` runs,
then the process exits with code 1 and stderr names the path.

Given a plan file whose version envelope's `u16` is `u16::MAX`,
when `patina debug journal <path>` runs against a binary whose
compiled major is `1`,
then the process exits with code 1 and stderr names both versions
(`u16::MAX` and `1`) plus the substring `version`.

Suggested files: `patina-cli/src/cmd/debug.rs`,
`patina-cli/src/cmd/debug_journal.rs`,
`patina-core/src/journal/render.rs`,
`patina-cli/tests/debug_journal_cli.rs`
</task-scenarios>
</task>

<task id="T-020" state="completed" covers="REQ-022">
## Formalise CLI exit codes 0 / 1 / 2 / 3 / 4 / 5

Add an `ExitCode` enum (or `i32` constants) in `patina-cli` mapping
every terminal CLI state to its required exit code:

- `0` — success.
- `1` — generic error (config parse failure, IO error, undefined
  variable, missing prior apply, version mismatch, unresolved
  shell, etc.).
- `2` — `must_succeed = true` `pre_apply` hook failed; apply
  aborted before any file operation.
- `3` — `must_succeed = true` `post_apply` hook failed; file
  operations rolled back.
- `4` — exclusive lock timeout (apply / rollback) or shared lock
  timeout-with-warning fallback that subsequently failed.
- `5` — interactive prompt declined (TTY user entered anything
  other than `y`/`Y`). Reserved also for the
  refused-elevation case SPEC-0002 adds.

Centralise the mapping: every CLI command path (apply, status,
rollback, debug) terminates through a single `cli::exit(code)`
helper or equivalent so the contract is enforced in one place.
A `EngineError -> ExitCode` mapping function expresses the rule
from one source.

Update the `anyhow` integration in `patina-cli`'s top-level error
handling: the helper inspects the error chain for known
`EngineError` variants before falling through to generic exit 1.

<task-scenarios>
Given a tempdir repository declaring a `[[hook]]` with
`event = "pre_apply" command = "false"` and the default
`must_succeed = true`,
when `patina apply --yes` runs,
then the process exits with code 2.

Given a tempdir repository declaring a `[[hook]]` with
`event = "post_apply" command = "exit 1"` and the default
`must_succeed = true`,
when `patina apply --yes` runs,
then the process exits with code 3.

Given a tempdir repository whose `patina.toml` has a TOML syntax
error,
when `patina apply --yes` runs,
then the process exits with code 1 and stderr names the offending
line.

Given a process A holding the exclusive lock past the apply
timeout cap (parameterised in the test harness) and process B
attempting `patina apply --yes`,
when process B gives up waiting,
then process B exits with code 4.

Given a TTY simulation feeding `n` to the apply prompt,
when `patina apply` runs without `--yes`,
then the process exits with code 5.

Given a `patina apply --yes` invocation that succeeds end-to-end,
when the process terminates,
then the exit code is 0.

Suggested files: `patina-cli/src/exit_code.rs`,
`patina-cli/src/cli.rs`,
`patina-cli/src/cmd/apply.rs`,
`patina-cli/src/cmd/rollback.rs`,
`patina-cli/src/cmd/status.rs`,
`patina-cli/src/cmd/debug_journal.rs`,
`patina-cli/tests/exit_codes.rs`
</task-scenarios>
</task>

<task id="T-021" state="completed" covers="REQ-021">
## Make user-facing stdout byte-deterministic across consecutive applies

Audit and harden the `output::Reporter` (T-016) abstraction so two
consecutive `patina apply` runs against an unchanged source
repository produce **byte-identical** stdout. Remove every
non-deterministic input from user-facing output paths:

- No `chrono::Utc::now()` / `jiff::Timestamp::now()` /
  `std::time::SystemTime::now()` calls in any code path that
  contributes to stdout (human or `--json`).
- No `std::process::id()` in user-facing output.
- No randomised IDs (UUID v4, random nonces) in user-facing
  output. Stable IDs (e.g. content hashes, sequential plan
  indices) are fine.
- Sort every collection that surfaces in stdout deterministically
  (modules alphabetically by name from T-003's enumeration;
  per-target rows in stable input order from T-004's
  `targets = [...]`; status `files` array sorted lexicographically
  by path).

Wall-clock timestamps may still appear in:

- The journal binary record (the `<ts>` filename and any
  `at`/`user`/`host` fields used by `patina status --json
  last_apply`).
- The debug journal decoder (T-019) since debug output is
  explicitly exempt.

The journal `<ts>` filename is the only place a timestamp leaks
into user-visible state, but the filename never appears in stdout
unless the user explicitly inspects the journal directory.

Add an integration test that runs the same apply twice and asserts
`diff -u out1 out2` is empty for both human and `--json` output
modes. Verify on a fixture small enough to compute the diff
quickly but rich enough to exercise multiple modes and at least
one multi-target entry.

<task-scenarios>
Given a tempdir repository and a clean state directory,
when `patina apply --yes --json > out1.json` runs and then
`patina apply --yes --json > out2.json` runs against the
unchanged repository,
then `diff -u out1.json out2.json` produces no output.

Given the same fixture but invoked in human mode,
when `patina apply --yes > out1.txt` runs and
`patina apply --yes > out2.txt` runs second,
then `diff -u out1.txt out2.txt` produces no output.

Given the patina-cli and patina-core source tree at HEAD,
when `grep -rn 'Utc::now\|SystemTime::now\|process::id\|jiff::Timestamp::now' patina-cli/src patina-core/src`
filtered to exclude `patina-core/src/journal/`, the debug-render
code paths, and `#[cfg(test)]` modules runs,
then the grep result is empty.

Given a tempdir repository containing a multi-target `[[file]]`
entry whose `targets` array is declared as
`["~/.codex/agent.toml", "~/.claude/agent.toml"]` (deliberately
not alphabetical),
when `patina apply --yes --json` runs and the JSON output's
`plan` array is inspected,
then the per-target rows appear in the input declaration order
(`.codex/agent.toml` before `.claude/agent.toml`), not sorted —
proving that "deterministic" means "stable function of inputs",
not "alphabetised".

Suggested files: `patina-cli/src/output/reporter.rs`,
`patina-cli/src/output/diff.rs`,
`patina-cli/src/output/json.rs`,
`patina-core/src/status/mod.rs`,
`patina-cli/tests/deterministic_stdout.rs`
</task-scenarios>
</task>

<task id="T-022" state="completed" covers="REQ-025">
## Cross-platform CI matrix gates merge on macOS / Linux / Windows

Stand up the GitHub Actions workflow that operationalises REQ-025's
parity rule. The workflow runs on every `push` to `main` and every
`pull_request`; the test job uses `strategy.matrix.os` containing
`macos-latest`, `ubuntu-latest`, and `windows-latest`. Each matrix
job runs:

- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`

All three matrix jobs are configured as required status checks on
`main`, so merge is blocked when any single OS fails. The workflow
uses `dtolnay/rust-toolchain@stable` for the toolchain installation
(channel-floating per the github-actions-versioning rule) and a
caching action (e.g. `Swatinem/rust-cache` at its current latest
major) so cold-cache runtime stays bounded. Third-party `uses:`
references are otherwise pinned to the current latest published
major per `.claude/rules/github-actions/github-actions-versioning.md`.

The branch-protection configuration (required-status-checks set)
is operator-administered on GitHub. The task's deliverable is the
workflow file plus a one-paragraph note in the workflow's leading
comment documenting which check names the operator must add to
required-status-checks on `main`.

<task-scenarios>
Given the repository at HEAD,
when the CI workflow file under `.github/workflows/` is parsed as
YAML,
then a job exists whose `strategy.matrix.os` list contains, as
strings, `macos-latest`, `ubuntu-latest`, and `windows-latest`, and
the workflow `on:` block triggers on both `push` (to `main`) and
`pull_request`.

Given the matrix workflow against the workspace at HEAD,
when each OS job runs `cargo test --workspace --locked` followed by
`cargo clippy --workspace --all-targets --locked -- -D warnings`,
then all three matrix jobs exit 0.

Given a PR that introduces
`#[cfg(target_os = "windows")] compile_error!("forced");` into
`patina-core/src/lib.rs`,
when the matrix workflow runs,
then `windows-latest` exits non-zero, `macos-latest` and
`ubuntu-latest` exit 0, and the workflow's overall conclusion is
`failure`.

Given the workflow file at HEAD,
when its `uses:` references are inspected,
then every third-party action other than channel-floating
`dtolnay/rust-toolchain@<channel>` references is pinned either to
its current latest published major tag (e.g. `@v5`) or to a commit
SHA with an inline `# vX.Y.Z` comment.

Suggested files: `.github/workflows/ci.yml`
</task-scenarios>
</task>

<task id="T-023" state="completed" covers="REQ-026">
## Clippy `disallowed-macros` denies `println!`/`eprintln!`/`print!`/`eprint!` outside the `output` module

T-016 stands up the `output::Reporter` trait and routes user-facing
prints through it. This task adds the clippy gate that makes the
Reporter the ONLY permitted call site for those macros across the
workspace.

Configure `clippy.toml` so `disallowed-macros` lists `std::println`,
`std::eprintln`, `std::print`, and `std::eprint`. The `output`
module — wherever the `Reporter` implementations live inside
`patina-cli` — is the sole permitted call site for those macros
and bears a module-scoped `#[allow(clippy::disallowed_macros)]`
attribute (clippy's `disallowed-macros` configuration is
workspace-wide; the carve-out is per-module via the allow
attribute). The `tracing` macros (`info!`, `warn!`, `error!`,
`debug!`, `trace!`) remain unaffected — they emit structured
events, not user output.

Add an integration test that constructs a fixture working tree
containing a fresh `println!("hi")` somewhere outside the `output`
module, invokes the project's clippy command with `-D warnings`,
and asserts the run exits non-zero with a
`clippy::disallowed_macros` diagnostic naming the offending file.
The test can drive this via `cargo clippy --message-format=json`
and parse the diagnostics, or via a `trycmd`-style snapshot test;
implementer picks the cheapest reliable mechanism.

This task depends on T-016 having stood up the `output` module
and the Reporter abstraction; without T-016 there is no carve-out
location to allow the macros in.

<task-scenarios>
Given the workspace at HEAD with `clippy.toml` configured per this
task,
when `cargo clippy --workspace --all-targets --locked -- -D warnings`
runs,
then it exits 0.

Given the repository at HEAD,
when `clippy.toml` is parsed as TOML,
then its `disallowed-macros` array contains, as literal strings,
`std::println`, `std::eprintln`, `std::print`, and `std::eprint`.

Given a working tree where a contributor has inserted
`println!("hi")` in `patina-core/src/plan.rs`,
when `cargo clippy --workspace --all-targets --locked -- -D warnings`
runs,
then it exits non-zero with a `clippy::disallowed_macros`
diagnostic naming `patina-core/src/plan.rs` and the offending
line.

Given the same working tree with the offending line replaced by
`tracing::info!("hi")`,
when the same clippy command runs,
then it exits 0 — `tracing` macros are not in the disallowed
list.

Given a working tree where the contributor has inserted
`println!("hi")` into the `output` module (e.g.
`patina-cli/src/output/human.rs`),
when the same clippy command runs,
then it exits 0 — the module-scoped
`#[allow(clippy::disallowed_macros)]` carve-out applies.

Suggested files: `clippy.toml`,
`patina-cli/src/output/mod.rs` (module-scoped allow attribute),
`patina-cli/tests/clippy_disallowed_macros.rs`
</task-scenarios>
</task>

<task id="T-024" state="completed" covers="REQ-027">
## Ship `docs/ARCHITECTURE.md` + `docs/USER_GUIDE.md` with named structural anchors

Author two documentation files under `docs/`:

- `docs/ARCHITECTURE.md` — contributor-facing high-level
  architecture document. Required `##`-level headings (exact
  text): `## Engine layers`, `## Journal format`, `## Apply phases`,
  `## Recovery`. The narrative covers the layered crate boundaries
  (`patina-core` lib vs `patina-cli` bin), the postcard journal
  envelope and single-fsync write protocol, the three apply phases
  (plan → diff → mutate), and the recovery/rollback primitives.
  Cross-link relevant REQs from SPEC-0001 by ID. Mermaid is
  preferred over ASCII art for any architecture diagram (per
  AGENTS.md "Code conventions" diagrams rule).

- `docs/USER_GUIDE.md` — user-facing usage and operational
  guidance. Required `##`-level headings (exact text):
  `## Installation`, `## Declaring dotfiles`, `## Apply flow`,
  `## State directory`, `## Recovery`, `## Troubleshooting`. The
  `## State directory` section MUST contain a markdown bullet
  list naming cloud-sync providers the state directory must not
  live on; the bullets MUST include the literal entries
  `iCloud Drive`, `OneDrive`, `Dropbox`, `Box`, `Google Drive`,
  `Syncthing`. (Per the product north star's Known-Unknowns note:
  SPEC-0001 documents only; SPEC-0002 adds doctor warnings; the
  `--linger` snippet for SPEC-0003 lands in this same file once
  SPEC-0003 starts work.)

Add a docs-structure integration test that:

1. Parses both files as CommonMark (e.g. via `pulldown-cmark`).
2. Extracts the set of `##`-level headings from each file.
3. Asserts each required heading appears, by exact text.
4. Locates the `## State directory` section in
   `docs/USER_GUIDE.md`, extracts its bullet-list items, and
   asserts each of the six required provider names is present as a
   literal bullet entry.

The test gates structural presence only — it never substring-matches
prose around the headings (per the test-hygiene rule in AGENTS.md
prohibiting tests over editorial choices).

<task-scenarios>
Given the repository at HEAD,
when the docs-structure integration test parses
`docs/ARCHITECTURE.md` and extracts its `##`-level headings,
then the set contains, by exact text, `Engine layers`,
`Journal format`, `Apply phases`, `Recovery`.

Given the repository at HEAD,
when the same test parses `docs/USER_GUIDE.md` and extracts its
`##`-level headings,
then the set contains, by exact text, `Installation`,
`Declaring dotfiles`, `Apply flow`, `State directory`, `Recovery`,
`Troubleshooting`.

Given the repository at HEAD,
when the test extracts bullet-list items from the body of the
`## State directory` section of `docs/USER_GUIDE.md`,
then the extracted set contains each of `iCloud Drive`, `OneDrive`,
`Dropbox`, `Box`, `Google Drive`, `Syncthing` as a literal entry.

Given a working tree where the `## State directory` heading is
renamed to `## State dir`,
when the docs-structure test runs,
then it fails naming the missing literal heading.

Given a working tree where the `Dropbox` bullet is replaced with
`Dropbox (via Smart Sync)`,
when the docs-structure test runs,
then it fails naming the missing literal bullet entry (the test
gates exact text, not prefix match).

Suggested files: `docs/ARCHITECTURE.md`, `docs/USER_GUIDE.md`,
`patina-cli/tests/docs_structure.rs`, workspace `Cargo.toml`
(`pulldown-cmark` as a dev-dependency)
</task-scenarios>
</task>

<task id="T-025" state="completed" covers="REQ-028">
## `deny.toml` + `cargo deny check` gate CI on every push and PR

Author `deny.toml` at the repository root with these top-level
tables populated:

- `[licenses]` — allowlist the project's distribution policy
  (typical Rust workspace: `MIT`, `Apache-2.0`, `BSD-3-Clause`,
  `ISC`, `Unicode-DFS-2016`, `Unicode-3.0`, `MPL-2.0`, `Zlib`).
  GPL-family licences are NOT in the allowlist. Configure
  `unlicensed = "deny"` (or the schema-current equivalent).

- `[advisories]` — pull the RustSec database via the default
  source. At minimum: `vulnerability = "deny"`,
  `unsound = "deny"`, `yanked = "deny"`,
  `unmaintained = "warn"`, `notice = "warn"`.

- `[bans]` — `multiple-versions = "warn"`,
  `wildcards = "deny"`. The `deny` list of explicitly-banned
  crates starts empty and grows organically as the project hits
  cases.

- `[sources]` — `unknown-registry = "deny"`,
  `unknown-git = "deny"`. Allow crates.io implicitly; any approved
  git registries must be explicitly listed.

Wire `cargo deny check` into the CI workflow T-022 stands up.
The job runs on every `push` to `main` and every `pull_request`.
The implementer chooses between (a) installing the `cargo-deny`
binary in the workflow and invoking `cargo deny check` directly,
or (b) using `EmbarkStudios/cargo-deny-action` at its current
latest published major; either is acceptable so long as the
github-actions-versioning rule is satisfied. Document the
required-status check name in the workflow file's leading comment
so the operator can add it to branch protection.

<task-scenarios>
Given the repository at HEAD with `deny.toml` configured per this
task,
when `cargo deny check` runs against `deny.toml`,
then it exits 0.

Given `deny.toml` at the repository root,
when parsed as TOML,
then the resulting document contains top-level tables named
`licenses`, `advisories`, `bans`, and `sources`.

Given the CI workflow at HEAD,
when its job list is parsed,
then a job exists that invokes `cargo deny check` (directly or via
an action wrapper at its current latest published major) and the
workflow `on:` block triggers on both `push` to `main` and
`pull_request`.

Given a working tree where `Cargo.toml` has been edited to declare
a dependency on a crate published under the `GPL-3.0` licence,
when `cargo deny check licenses` runs against the configured
`deny.toml`,
then it exits non-zero with a `licenses` diagnostic naming the
offending crate.

Given a working tree that adds a dependency with a wildcard
version (`some-crate = "*"`) to `Cargo.toml`,
when `cargo deny check bans` runs,
then it exits non-zero with a `wildcards` diagnostic naming the
offending dependency.

Suggested files: `deny.toml`, `.github/workflows/ci.yml`
(extending T-022's workflow file)
</task-scenarios>
</task>

<task id="T-026" state="completed" covers="REQ-029">
## Widen the committed apply record: per-target source + blake3 content hash

REQ-029 (added by the 2026-05-29 amendment) tightens the committed
`ApplyRecord` so downstream SPECs can read each target's source and a
real content hash from the `<ts>.COMMIT` sentinel — the `<ts>.plan`
that also held sources is deleted at commit, so the COMMIT record is
the only post-commit source of truth.

Land this as ONE atomic change so the workspace compiles and CI stays
green after the task: the record type, the write side, and the read
side change together (a split would leave an intermediate tree where
`classify.rs` references a field the record no longer has).

Type (`patina-core/src/journal/record.rs`):

- `ExpectedTarget::Content` replaces `fingerprint: u64` with
  `hash: [u8; 32]` (a `blake3` digest) and gains a `source: String`
  (canonical absolute source path).
- Document `ExpectedTarget::Symlink.link_target` as the canonical
  source for symlink targets, and add a `source()` accessor returning
  the source for either variant (parallel to the existing `target()`
  / `entry()` accessors).
- Replace `fingerprint_bytes(&[u8]) -> u64` with a `blake3`-based
  `content_hash(&[u8]) -> [u8; 32]` helper used by BOTH the write and
  read sides so the two agree byte-for-byte.
- Bump the shared `FILE_MAJOR_VERSION` (in
  `patina-core/src/journal/plan.rs`, imported by `record.rs`) from
  `1` to `2`; the envelope rule already refuses a newer major, so no
  other version-handling code changes.

Write side (`patina-core/src/apply/engine.rs`, ~lines 414-432):

- When building each `ExpectedTarget`, capture the canonical source
  path and, for content targets, compute the `blake3` hash of the
  bytes written (replacing the `fingerprint_bytes(&bytes)` call).

Read side (`patina-core/src/status/classify.rs`):

- Compare a freshly computed `blake3` of the live file against the
  recorded `hash`. CLEAN / DRIFTED / MISSING / ORPHANED behaviour is
  unchanged — REQ-018 scenarios CHK-031 / CHK-032 / CHK-048 must
  still pass.

Mechanical ripples — update the call sites that destructure or build
the changed variants so the workspace compiles: `status/mod.rs`,
`rollback/mod.rs`, `rollback/replay.rs`, `journal/recovery.rs`, and
the in-module / integration tests. Rollback and recovery behaviour is
unchanged; they read `source` / `entry` and ignore the hash.

Dependency: add `blake3` to `patina-core/Cargo.toml`. Its license
expression includes `Apache-2.0`, which the `deny.toml` allowlist
already permits, so `cargo deny check` passes without an allowlist
edit — verify rather than assume.

<task-scenarios>
Given a tempdir repository whose `git` module declares
`[[file]] source = "gitconfig" target = "~/.gitconfig" mode = "copy"`
and `<repo>/git/gitconfig` with arbitrary content,
when `patina apply --yes` runs and the resulting
`<state>/patina/journal/<ts>.COMMIT` record is decoded,
then the decoded record contains an entry whose target resolves to
`~/.gitconfig`, whose source equals the canonical absolute path of
`<repo>/git/gitconfig`, and whose content hash equals the 32-byte
`blake3` of the bytes of `<repo>/git/gitconfig`.

Given a tempdir repository declaring a symlink `[[file]]`,
when `patina apply --yes` runs and the COMMIT record is decoded,
then the entry's source equals the canonical path the link points
at.

Given a tempdir repository with one content-mode `[[file]]` entry,
when `patina apply --yes` runs twice against the unchanged source and
both `<ts>.COMMIT` records are decoded,
then the recorded `blake3` content hash for that target is
byte-identical across the two records.

Given the `<state>/patina/journal/<ts>.COMMIT` file produced by a
successful apply,
when its first two bytes are read as a little-endian `u16`,
then the value equals `2`.

Given a copy-mode apply followed by an external edit to the target's
bytes,
when `patina status --json` runs,
then the target is reported `drifted`; with no edit it is reported
`clean` — confirming the read side compares the recorded `blake3`.

Given the workspace at HEAD after this task,
when `cargo test --workspace --locked`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`cargo deny check` run,
then all three exit 0.

Suggested files: `patina-core/src/journal/record.rs`,
`patina-core/src/journal/plan.rs`,
`patina-core/src/apply/engine.rs`,
`patina-core/src/status/classify.rs`,
`patina-core/src/status/mod.rs`,
`patina-core/src/rollback/mod.rs`,
`patina-core/src/rollback/replay.rs`,
`patina-core/src/journal/recovery.rs`,
`patina-core/Cargo.toml`,
`patina-core/tests/commit_record.rs`
</task-scenarios>
</task>

<task id="T-027" state="completed" covers="REQ-030">
## Add a lock-acquisition policy to the engine apply path

REQ-030 (added by the 2026-05-30 amendment) lets a caller select how
the engine apply path obtains the exclusive advisory lock, so SPEC-0002
`remove`/`promote` can re-journal while already holding the lock and the
SPEC-0003 watcher can attempt the lock non-blocking and skip on
contention. Today `execute_plan`
(`patina-core/src/apply/engine.rs`, ~line 300) unconditionally calls
`acquire_lock(&resolved.lock_path(), LockKind::Exclusive,
exclusive_timeout())`, with no non-blocking and no
apply-while-holding-lock path.

Land this so the default path is byte-for-byte the pre-amendment
behaviour; only the two new acquisition strategies are added.

Policy type (`patina-core/src/lock.rs` or `apply/engine.rs`):

- Add a `LockPolicy` with three variants: `Blocking` (the default),
  `NonBlocking`, and `Held(LockGuard)` (carries the caller's
  already-acquired exclusive guard). `Blocking` must be the `Default`.
- Add a non-blocking acquisition primitive to `lock.rs` — either a
  `try_acquire(path, kind) -> Result<LockGuard, LockError>` that makes
  exactly one `try_lock_*` attempt, or reuse `acquire` with a
  zero-duration cap — and surface contention as a distinct typed error
  the engine can match (e.g. a `LockError::Contended` variant or an
  `EngineError` mapping) rather than the generic `Timeout`.

Wire through the apply entry points
(`patina-core/src/apply/engine.rs`, `patina-core/src/lib.rs`):

- `execute_plan` takes the policy (thread it via `ApplyRequest` /
  `ApplyOptions`, defaulting to `Blocking`) and, before any filesystem
  mutation, resolves the guard per policy:
  - `Blocking`: acquire exclusive with `exclusive_timeout()` exactly as
    today; a timeout still maps to exit 4.
  - `NonBlocking`: one attempt; on contention return the typed
    contention error and perform ZERO mutation. The lock is already
    acquired at the very top of `execute_plan` before the plan flush,
    so the zero-mutation guarantee falls out of returning early — keep
    it that way.
  - `Held(guard)`: do not acquire; use the supplied guard for the run.
- `run_rollback` is unchanged (keeps its internal `Blocking` acquire);
  REQ-030 scopes the policy to the apply entry points only.

CLI call sites (`patina-cli/src/cmd/apply.rs`,
`patina-cli/src/cmd/rollback.rs`): pass the default `Blocking` policy so
`patina apply` / `patina rollback` behaviour and the REQ-022/REQ-023
exit codes are unchanged. The watcher (`NonBlocking`) and
`remove`/`promote` (`Held`) call-site wiring is owned by SPEC-0003 /
SPEC-0002 respectively — not this task.

<task-scenarios>
Given a tempdir state directory whose `<state>/patina/lock` is held
exclusively by a test-controlled guard,
when an apply is driven under the `NonBlocking` policy,
then it returns the typed contention error and
`<state>/patina/journal/` contains no new `<ts>.plan` or `<ts>.COMMIT`
written by the contended attempt (CHK-065).

Given a test that acquires the exclusive lock at `<state>/patina/lock`
and retains the guard,
when it drives an apply under the `Held` policy passing that guard,
then the apply completes successfully (it does not time out against its
own held lock) and the resulting `<ts>.COMMIT` record is present
(CHK-066).

Given a tempdir repository and an uncontended state directory,
when `patina apply --yes` runs (default `Blocking` policy) and then a
second `patina apply --yes` runs against the unchanged source,
then both exit 0 and the second produces byte-identical stdout to the
first (CHK-067; REQ-021 determinism preserved under the default).

Given the workspace at HEAD after this task,
when `cargo test --workspace --locked`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`cargo deny check` run,
then all three exit 0.

Suggested files: `patina-core/src/lock.rs`,
`patina-core/src/apply/engine.rs`,
`patina-core/src/lib.rs`,
`patina-core/src/error.rs`,
`patina-cli/src/cmd/apply.rs`,
`patina-cli/src/cmd/rollback.rs`,
`patina-core/tests/lock_policy.rs`
</task-scenarios>
</task>

<task id="T-028" state="completed" covers="REQ-013 REQ-030">
## Move orphan recovery under the exclusive lock (acquire-then-recover)

The 2026-05-30 hardening amendment closed a concurrency hazard: the
shipped engine runs `recover_orphans` *before* it resolves the lock,
for all three `LockPolicy` variants. Today `execute`
(`patina-core/src/apply/engine.rs`) calls
`recover_orphans(&journal_dir, &backups_dir)?` at the top of the
function (~line 337), and only *then* resolves the guard per policy
(~line 342). Because `orphan_timestamps`
(`patina-core/src/journal/recovery.rs`) treats any `<ts>.plan` lacking
a `.COMMIT`/`.ROLLED_BACK` sibling as an orphan — with no lock, PID, or
age guard — a second apply's recovery can reverse a *live in-flight*
apply's operations, and a contended `NonBlocking` attempt that finds a
pending orphan mutates the filesystem (reversing backups, deleting the
orphan's plan/progress) before it returns the contention error,
violating REQ-030's strengthened "writes nothing" guarantee that the
SPEC-0003 watcher (REQ-006 / REQ-008) depends on.

Reorder so the engine acquires/resolves the lock FIRST, then runs
recovery UNDER the held lock, for every policy:

- In `execute` (`apply/engine.rs`), move the `recover_orphans(...)`
  call to AFTER the `let _guard = match policy { ... }` block, so it
  runs only once the guard is held. The `Blocking` and `Held` paths
  then recover with the lock held; the `NonBlocking` path, on
  contention, returns the typed contention error via
  `try_acquire_lock` BEFORE `recover_orphans` is ever reached — so it
  performs zero filesystem mutation (no recovery, no plan, no COMMIT,
  no backup) even when an orphan is pending.
- Confirm the move does not change single-process / uncontended
  behaviour: recovery still precedes the plan flush and the fresh
  apply; only its position relative to lock acquisition changes. The
  `Blocking` path must stay observably identical for one process
  (CHK-067 determinism must still hold).
- Keep `run_rollback` (`rollback/mod.rs`) as is — verify it does not
  call `recover_orphans` before its own exclusive acquire.

Secondary deliverable (test-only; same subsystem, and it protects this
task's all-tests-green gate): harden the pre-existing flaky test
`blocked_exclusive_exits_with_lock_timeout_code`
(`patina-core/tests/lock_concurrency.rs`). It assumes the holder wins
the exclusive lock within a hardcoded 150ms head-start before the
contender starts; under machine load that is not enough (the
freshly-built holder binary cold-starts slower than 150ms), the
contender acquires the still-free lock, and the test fails by exiting 0
instead of the timeout code 4 (observed during the verification pass
that triggered this amendment — finding F2). Gate the contender's
launch on the holder's *observed* acquisition (e.g. wait for the
holder's `ACQUIRED` stdout marker) instead of a fixed sleep, so the
outcome is deterministic under load. No production behaviour changes;
it is bundled here only because it lives in the lock subsystem this
task already edits and otherwise intermittently breaks this task's
`cargo test` gate.

<task-scenarios>
Given a tempdir state directory whose `<state>/patina/lock` is held
exclusively by a test-controlled guard AND whose
`<state>/patina/journal/` contains an orphan `<ts>.plan` (no matching
`<ts>.COMMIT`, no `<ts>.ROLLED_BACK`) from a prior crashed apply,
when an apply is driven under the `NonBlocking` policy,
then it returns the typed contention error, the orphan `<ts>.plan` and
its `<ts>.progress` are left untouched (not reversed, not deleted), and
no new `<ts>.plan`, `<ts>.COMMIT`, or backup is written by the
contended attempt (CHK-068).

Given an uncontended state directory with a pending orphan `<ts>.plan`,
when an apply is driven under the default `Blocking` policy,
then the orphan is recovered (overwrite-targets restored from backups,
fresh targets deleted, plan/progress removed) under the held exclusive
lock and the new apply proceeds — the same end state as before the
reorder (REQ-013 recovery semantics unchanged for a single process).

Given a tempdir repository and an uncontended state directory,
when `patina apply --yes` runs and then a second `patina apply --yes`
runs against the unchanged source,
then both exit 0 and the second produces byte-identical stdout to the
first (CHK-067; REQ-021 determinism preserved under the reorder).

Given the lock-concurrency suite run repeatedly and under machine load,
when `blocked_exclusive_exits_with_lock_timeout_code` runs,
then it deterministically exits 4 with a `TIMEOUT exclusive` stderr —
its outcome no longer depends on a fixed head-start sleep (the
contender starts only after the holder's observed acquisition; F2).

Given the workspace at HEAD after this task,
when `cargo test --workspace --locked`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`cargo deny check` run,
then all three exit 0.

Suggested files: `patina-core/src/apply/engine.rs`,
`patina-core/src/journal/recovery.rs`,
`patina-core/tests/recovery_crash.rs`,
`patina-core/tests/lock_policy.rs`,
`patina-core/tests/lock_concurrency.rs`
</task-scenarios>
</task>
