---
spec: SPEC-0002
spec_hash_at_generation: befdad68d2652a0421f16ee44208210e20eba5eac52efb4d05cdaf3cba2de167
generated_at: 2026-05-30T07:11:51Z
---
# Tasks: SPEC-0002 Patina complete CLI surface and Windows symlink elevation — init/add/remove/promote/doctor plus the Windows Developer Mode UAC elevation flow

<task id="T-001" state="completed" covers="REQ-002 REQ-003">
## Add a format-preserving `toml_edit` manifest writer to `patina-core::config`

`patina-core::config` is parse-only today: `parse_module_config`
deserializes via the `toml` crate and `FileEntry` is `Deserialize`-only.
SPEC-0002's `init` / `add` / `remove` commands must *write* and *edit*
`patina.toml` files, so this task introduces the writer DEC-007 selects —
`toml_edit` (format/comment-preserving), not `toml` (reserialize). The read
side stays on the existing `toml` dependency; only the write side is new.

Work:

- Add `toml_edit` to the root `[workspace.dependencies]` table in
  `/Users/kevin/src/patina/Cargo.toml`, then reference it from
  `patina-core/Cargo.toml` with `toml_edit.workspace = true`. Run
  `cargo deny check` afterward (the hard rule forbids introducing a
  dependency without clearing `deny.toml`; `toml_edit` is MIT/Apache-2.0,
  already on the license allowlist, but the advisory/bans pass must be
  green and `cargo_deny_config.rs` must still pass).
- Create `/Users/kevin/src/patina/patina-core/src/config/writer.rs` with a
  typed `ConfigWriteError` (`thiserror`, `#[non_exhaustive]`) and three
  functions operating on manifest text (`String` in, `String` out — the
  caller owns reading/writing the file via `fs-err`):
  - `scaffold_root_manifest(created_at: &str) -> String` — emit a root
    manifest with a `[patina]` table containing `root = true` and a
    `created_at` RFC 3339 string field (REQ-001 done-when; the only
    timestamp permitted in user-facing artefacts because it is config).
  - `append_file_entry(doc_text: &str, source: &str, target: &str, mode: FileMode) -> Result<String, ConfigWriteError>`
    — parse `doc_text` as a `toml_edit::DocumentMut` (empty text yields an
    empty document), push one `[[file]]` array-of-tables element carrying
    `source`, `target`, and `mode` keys, and return the serialized text
    with all pre-existing tables, comments, key ordering, and whitespace
    intact. Map `FileMode` to its manifest string via the same spellings
    the parser accepts in `config/file_entry.rs` (`"symlink"`,
    `"symlink-dir"`, `"copy"`, `"copy-tree"`); the writer never emits a
    `mode` for `FileMode::TemplateRender` (templating is implied by a
    `.tmpl` source suffix, and the parser rejects an explicit template
    mode — see `FileEntryError::ImplicitTemplateModeDeclared`).
  - `remove_file_entry(doc_text: &str, target: &str) -> Result<String, ConfigWriteError>`
    — delete exactly the one `[[file]]` element whose `target` key equals
    `target`, leaving every sibling `[[file]]`, every `[[hook]]`, the
    `[variables]` table, comments, and formatting untouched. Return a
    typed error when no entry matches.
- Wire the new module into `patina-core/src/config/mod.rs`
  (`pub mod writer;`) and re-export `scaffold_root_manifest`,
  `append_file_entry`, `remove_file_entry`, and `ConfigWriteError` from
  `patina-core/src/lib.rs` alongside the existing `config` re-exports.
- Add a `EngineError::ConfigWrite(#[from] ConfigWriteError)` variant in
  `patina-core/src/error.rs` so command code can propagate writer failures
  through the engine error type (`EngineError` is `#[non_exhaustive]`, so
  this is additive).

The load-bearing path is `remove_file_entry`: DEC-007 exists specifically
so a one-entry delete does not rewrite sibling entries or discard the
user's comments, and so REQ-010 determinism holds across edits. Prove that
with the comment/sibling-preservation unit test below.

<task-scenarios>
Given a module manifest text containing two `[[file]]` entries (targets
`~/.zshrc` and `~/.vimrc`), a `[[hook]]` entry, a `[variables]` table, and
a `# hand-written comment` line,
when `remove_file_entry(text, "~/.zshrc")` runs,
then the returned text parses via `toml::from_str` with the `~/.vimrc`
`[[file]]` entry, the `[[hook]]`, the `[variables]` table, and the
`# hand-written comment` all still present, and no `[[file]]` entry whose
`target` is `~/.zshrc` remains.

Given an empty manifest text,
when `append_file_entry("", "zshrc", "~/.zshrc", FileMode::Symlink)` runs,
then the returned text parses via `parse_module_config` into a config whose
single `[[file]]` entry has `source = "zshrc"`, `target = "~/.zshrc"`, and
resolves to `FileMode::Symlink`.

Given `scaffold_root_manifest("2026-05-30T12:00:00Z")`,
when the returned text is parsed as TOML,
then its `[patina]` table contains `root = true` and a `created_at` value
equal to `2026-05-30T12:00:00Z`.

Suggested files: `patina-core/src/config/writer.rs`,
`patina-core/src/config/mod.rs`, `patina-core/src/lib.rs`,
`patina-core/src/error.rs`, `patina-core/Cargo.toml`, `Cargo.toml`
</task-scenarios>
</task>

<task id="T-002" state="completed" covers="REQ-001">
## Add a persisted `default_repo` pointer writer to `patina-core::discovery`

Repo discovery already *reads* the persisted default
(`discovery/repo.rs`: `PERSISTED_DEFAULT_FILENAME = "default_repo"`, the
private `persisted_default_repo_path()` and `read_persisted_default()`
helpers route through `state_dir::compute_root`). SPEC-0002's `init` writes
that pointer and `doctor` reports its absence, so this task adds the
public write + existence surface, reusing the existing filename constant
and path derivation rather than re-deriving the per-OS layout in the CLI.

Work in `/Users/kevin/src/patina/patina-core/src/discovery/repo.rs`:

- Add `pub fn write_persisted_default(state_dir: &Utf8Path, repo: &Utf8Path) -> Result<(), RepoDiscoveryError>`
  (or a dedicated typed error variant) that writes `repo` as one
  UTF-8 line with a trailing newline to `state_dir.join(PERSISTED_DEFAULT_FILENAME)`,
  via `fs-err`. The `state_dir` argument is the resolved per-machine
  patina directory (`state_dir::resolve()` output), so the file lands at
  `<state>/patina/default_repo`. The written path must be the canonical
  absolute repo path (callers canonicalize via `canonicalize_path` before
  calling).
- Add `pub fn default_repo_pointer_path(state_dir: &Utf8Path) -> Utf8PathBuf`
  returning `state_dir.join(PERSISTED_DEFAULT_FILENAME)`, and
  `pub fn persisted_default_present(state_dir: &Utf8Path) -> bool` (a plain
  `Utf8Path::exists` on that path) for `doctor`'s missing-pointer finding.
  Reuse `PERSISTED_DEFAULT_FILENAME`; do not hardcode `"default_repo"` at
  the call sites.
- Re-export the new functions and `PERSISTED_DEFAULT_FILENAME` from
  `patina-core/src/lib.rs` next to the existing `discovery` re-exports.

Keep the write parameterized on an explicit `state_dir` (rather than
re-resolving from the environment inside the function) so the CLI and the
integration harness can point it at an isolated tempdir state directory.

<task-scenarios>
Given an isolated tempdir state directory `S` and a canonical repo path
`R`,
when `write_persisted_default(S, R)` runs and then the existing read path
resolves the persisted default against `S`,
then `default_repo_pointer_path(S)` exists, its contents trimmed equal
`R`, and `persisted_default_present(S)` returns `true`.

Given a tempdir state directory `S` with no `default_repo` file,
when `persisted_default_present(S)` is called,
then it returns `false`.

Suggested files: `patina-core/src/discovery/repo.rs`,
`patina-core/src/lib.rs`
</task-scenarios>
</task>

<task id="T-003" state="completed" covers="REQ-001 REQ-010">
## Implement `patina init` and generalize the integration test harness

Land the first SPEC-0002 subcommand: `patina init` scaffolds a root
`patina.toml`, persists the default-repo pointer, prints a next-step hint,
and supports `--json`. This task also generalizes the integration harness
(`patina-cli/tests/common/mod.rs`) so subsequent command tasks can drive
any subcommand, not just `apply`.

Work:

- Add an `Init` variant + `InitArgs` to the `Command` enum in
  `patina-cli/src/cli.rs`. `InitArgs` carries an optional positional
  `path: Option<Utf8PathBuf>` (target directory; defaults to the current
  working directory), `--json`, and `--yes` (init is a mutating command
  per REQ-009). Keep the derive surface parsing-only, as the existing
  args structs do.
- Create `patina-cli/src/cmd/init.rs` with
  `pub async fn run(args: &InitArgs, reporter: &mut impl Reporter) -> Result<i32>`
  and register `pub mod init;` in `patina-cli/src/cmd/mod.rs`; dispatch the
  `Command::Init` arm in `patina-cli/src/main.rs`.
- Behaviour (REQ-001): resolve the target directory (positional or CWD),
  creating it if necessary. Acquire the engine's exclusive advisory lock at
  `state_dir::resolve()?.join("lock")` via `acquire_lock(.., LockKind::Exclusive, exclusive_timeout())`
  before any filesystem mutation (REQ-009). If a `patina.toml` already
  exists at the target, refuse with a typed `anyhow` error naming the
  existing file path and return exit 1 (do not overwrite). Otherwise write
  `scaffold_root_manifest(<rfc3339-now>)` (T-001) to
  `<target>/patina.toml` via `fs-err`, then call
  `write_persisted_default(&state, &canonical_target)` (T-002) with the
  canonicalized target path.
- Output (REQ-010): human path prints, as the final stdout line, exactly
  `Next: run `patina add <path>` to register an existing dotfile.` (the
  next-step hint, with the target path substituted). `--json` emits a
  single deterministic JSON document on stdout (no timestamps/PIDs/random
  ids) describing the created path and the persisted pointer; all warnings
  and prompts go to stderr via the `Reporter`.
- Harness: extend `patina-cli/tests/common/mod.rs` with a generic invoker
  (e.g. `fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> Output`)
  that spawns `CARGO_BIN_EXE_patina` with the same `PATINA_REPO` / `HOME` /
  `USERPROFILE` / `XDG_STATE_HOME` / `LOCALAPPDATA` isolation the existing
  `apply_with_env` uses, but with a caller-supplied subcommand. Keep
  `apply_with_env` working (have it delegate). Add a new integration test
  crate `patina-cli/tests/init_cli.rs`.

Note for REQ-010 determinism: `init` against an already-initialized
directory must fail *identically* every time so its `--json` error document
is byte-stable across reruns (CHK-017 asserts the second and third runs
match byte-for-byte).

<task-scenarios>
Given an empty tempdir `T` and an isolated, clean state directory,
when `patina init T` runs,
then `T/patina.toml` exists with `[patina]` `root = true`, the state
directory's `default_repo` file contains the canonical absolute path of
`T`, stdout's final line is the `patina add` next-step hint, and the
process exits 0.

Given a tempdir `T` already containing a `patina.toml`,
when `patina init T` runs,
then `T/patina.toml` is byte-identical to before, stderr contains
`already exists` and the path of `T/patina.toml`, and the process exits 1.

Given a tempdir `T` already containing a `patina.toml`,
when `patina init T --json` is run twice in succession,
then the two stdout outputs are byte-identical (deterministic failure
document).

Suggested files: `patina-cli/src/cli.rs`, `patina-cli/src/cmd/init.rs`,
`patina-cli/src/cmd/mod.rs`, `patina-cli/src/main.rs`,
`patina-cli/tests/common/mod.rs`, `patina-cli/tests/init_cli.rs`
</task-scenarios>
</task>

<task id="T-004" state="completed" covers="REQ-002 REQ-009">
## Implement `patina add <path>`

`patina add <path>` brings an existing dotfile under management: it moves
the file into a module subdirectory, writes a `[[file]]` entry, and leaves
the original target as a regular file so a subsequent `patina apply` would
converge (move-on-add, resolved open-question (a)). `add` does NOT call the
engine apply path — the move is bespoke CLI filesystem work; materialization
is deferred to a later `patina apply`.

Work:

- Add an `Add` variant + `AddArgs` to `patina-cli/src/cli.rs`: a positional
  `path` (absolute or HOME-relative, expanded via `expand_tilde`),
  `--module <name>` (optional), `--json`, `--yes`, and a clap argument
  group of `--symlink` / `--copy` / `--template` declared
  `#[group(multiple = false)]` so at most one mode flag is accepted (two
  mode flags must produce clap usage exit code 2). In v1.0 `add` exposes
  only these three modes — not `symlink-dir` / `copy-tree` (non-goal).
- Create `patina-cli/src/cmd/add.rs` (`pub async fn run(args, tty, reader, reporter)`),
  reusing `Tty` / `PromptReader` / `StdinReader` from `cmd::apply`. Register
  the module and dispatch the arm in `main.rs` with the same TTY-detection
  wiring `apply` uses.
- Behaviour (REQ-002): resolve the repository root via
  `resolve_repository_root`. Acquire the exclusive lock (REQ-009) before
  mutating. Determine the module: from `--module`, else prompt in a TTY;
  in a non-TTY without `--module` exit 1 with a typed error naming the
  missing `--module`. Determine the mode: from the single mode flag, else
  prompt in a TTY; in a non-TTY without a mode flag exit 1. Refuse if the
  path is already managed (scan existing modules via `discover_modules` +
  `parse_module_config` for a `[[file]]` whose target matches) — exit 1
  with a typed error naming the existing entry and its module. Otherwise:
  create `<repo>/<module>/` if absent, move the file to
  `<repo>/<module>/<basename>` (`fs-err` rename, with a copy-then-remove
  fallback for cross-device moves), and append the entry with
  `append_file_entry` (T-001), creating `<repo>/<module>/patina.toml` if it
  does not exist. The target path is left as a regular file containing the
  original bytes (apply has not run).
- Output (REQ-010): human prose to stdout, `--json` document on stdout,
  telemetry/prompts to stderr.
- Tests: new `patina-cli/tests/add_cli.rs`. Prove the move + entry write +
  untouched-target slice (CHK-003), the two-mode-flags clap-usage exit 2,
  and the non-TTY-without-module exit 1. Prove REQ-009 lock serialization
  with the contention slice below, mirroring the holder pattern in
  `patina-core/tests/lock_concurrency.rs` and parameterizing the wait via
  `PATINA_LOCK_TIMEOUT_MS`.

<task-scenarios>
Given a tempdir HOME containing `~/.zshrc` with content `foo` and a tempdir
repository with only a root `patina.toml`,
when `patina add ~/.zshrc --module zsh --symlink --yes` runs,
then `<repo>/zsh/zshrc` is a regular file with content `foo`,
`<repo>/zsh/patina.toml` contains a `[[file]]` entry with `source = "zshrc"`,
`target = "~/.zshrc"`, `mode = "symlink"`, and `~/.zshrc` is still a regular
file with content `foo` (apply has not run).

Given the same fixture,
when `patina add ~/.zshrc --symlink --copy` runs,
then the process exits 2 (clap usage error) and stderr names the
conflicting flags.

Given a process A holding the engine's exclusive lock and a concurrent
process B running `patina add ~/.zshrc --module zsh --symlink --yes`,
when both run,
then B blocks until A releases the lock, then completes successfully, and
the two processes' journal writes do not interleave.

Suggested files: `patina-cli/src/cli.rs`, `patina-cli/src/cmd/add.rs`,
`patina-cli/src/cmd/mod.rs`, `patina-cli/src/main.rs`,
`patina-cli/tests/add_cli.rs`
</task-scenarios>
</task>

<task id="T-005" state="completed" covers="REQ-003 REQ-009">
## Implement `patina remove <path>` with `--purge` and `Held`-policy re-journal

`patina remove <path>` unmanages a target: it removes the `[[file]]` entry,
replaces the target with a regular file holding the last-applied content
(so the system stays functional), and re-journals the new managed set so
`patina status` treats the path as unmanaged (absent) rather than ORPHANED.
This is the first command to drive the engine re-apply under
`LockPolicy::Held` (REQ-009 / SPEC-0001 REQ-030): it holds ONE exclusive
lock for the whole command so the re-apply does not self-contend.

Work:

- Add a `Remove` variant + `RemoveArgs` (positional `path`, `--purge`,
  `--json`, `--yes`) to `cli.rs`; create `cmd/remove.rs`
  (`run(args, tty, reader, reporter)`); register and dispatch in `mod.rs` /
  `main.rs` with the TTY wiring.
- Behaviour (REQ-003), in order, under one exclusive guard acquired at
  `state_dir::resolve()?.join("lock")`:
  1. `read_latest_commit(<state>/patina/journal)` and find the
     `ExpectedTarget` whose `target()` equals the canonicalized input path.
     If none, exit 1 with a typed error naming the path and the three
     discovery sources (env, walk-up, persisted default), matching the
     established discovery-error wording.
  2. Reconstruct the last-applied content from the journaled source
     (`ExpectedTarget::source()`, the canonical repo source path recorded
     per SPEC-0001 REQ-029): for a `Symlink` or copy-mode `Content` target,
     read the source bytes from the repository via `fs-err`; for a
     template-rendered target (the journaled source ends in `.tmpl`),
     re-render that source through MiniJinja against the variable context
     resolved at remove time (DEC-005 — the journal records only a blake3
     hash of the rendered bytes, not the bytes, so reconstruction is a
     re-render, which may differ from the byte-exact last-applied output if
     the context changed; this is the deliberate reset-to-current-intent
     semantics). Reuse the engine's existing resolver/render construction
     rather than reimplementing layer-stacking.
  3. Replace the target: without `--purge`, remove the existing
     symlink/file and write a regular file containing the reconstructed
     content; with `--purge`, delete the target entirely and write nothing.
     This is `remove`-specific filesystem work done before the re-apply.
  4. `remove_file_entry` (T-001) on the owning module's `patina.toml`
     (located from the journaled source path's module directory). If this
     empties the module manifest of `[[file]]`/`[[hook]]` entries, leave
     the empty `patina.toml` in place (do not auto-delete user files).
  5. Re-journal by re-applying: `plan_apply(&ApplyRequest::default(), ts)`
     (the plan now omits the removed entry) then
     `execute_plan(&resolved, &request, LockPolicy::Held(guard))`. The fresh
     `<ts>.COMMIT` omits the target, so `status` no longer lists it. Do not
     hand-write a COMMIT — drive it through the engine re-apply.
  6. The repository source file is NOT deleted (purge or not).
- Output (REQ-010): human prose / `--json` on stdout; telemetry on stderr.
- Tests: new `patina-cli/tests/remove_cli.rs`. Cover the no-purge replace +
  entry removal + status-omits-target slice (CHK-005), the `--purge` slice
  (CHK-006), and the unmanaged-path exit 1.

<task-scenarios>
Given an applied symbolic link at `~/.zshrc` pointing to `<repo>/zsh/zshrc`
(content `shell-config`),
when `patina remove ~/.zshrc --yes` runs,
then `~/.zshrc` is a regular file with content `shell-config`,
`<repo>/zsh/patina.toml` no longer contains a `[[file]]` entry for
`~/.zshrc`, `<repo>/zsh/zshrc` is unchanged, and a subsequent
`patina status --json` does not list `~/.zshrc` in its `files` array.

Given the same applied symbolic link,
when `patina remove ~/.zshrc --purge --yes` runs,
then `~/.zshrc` does not exist on disk, the `[[file]]` entry is removed, and
`<repo>/zsh/zshrc` is unchanged.

Given an unmanaged path `~/.bashrc`,
when `patina remove ~/.bashrc --yes` runs,
then the file is unchanged, no manifest is mutated, and the process exits 1.

Suggested files: `patina-cli/src/cli.rs`, `patina-cli/src/cmd/remove.rs`,
`patina-cli/src/cmd/mod.rs`, `patina-cli/src/main.rs`,
`patina-cli/tests/remove_cli.rs`
</task-scenarios>
</task>

<task id="T-006" state="completed" covers="REQ-004 REQ-009">
## Implement `patina promote <target>` for drifted copy-mode targets

`patina promote <target>` copies an externally-edited copy-mode target's
bytes back into its repository source, then re-applies so the journal
records the new content as the expected hash. It refuses on
template-rendered targets (DEC-006 — templating is non-invertible) and on
symbolic-link targets (the target IS the source). Like `remove`, it holds
one exclusive lock and re-journals under `LockPolicy::Held` (REQ-009). This
task can reuse the read-commit + Held-re-apply scaffolding T-005 builds —
factor any shared `pub(crate)` helper in `cmd/` rather than duplicating it.

Work:

- Add a `Promote` variant + `PromoteArgs` (positional `target`, `--json`,
  `--yes`) to `cli.rs`; create `cmd/promote.rs`; register and dispatch.
- Behaviour (REQ-004), under one exclusive guard:
  1. `read_latest_commit` and locate the `ExpectedTarget` for the
     canonicalized target. If absent, exit 1.
  2. Refuse, with exit 1 and a typed error, when the target is an
     `ExpectedTarget::Symlink` (message names the target and explains
     symlink targets share content with their source so promotion is
     meaningless) or when the journaled `source()` ends in `.tmpl` (message
     names the `.tmpl` source path and the word `template`). A `copy-tree`
     target promotes the individual leaf file named, not the whole tree.
  3. Read the current bytes of the target via `fs-err` and write them to
     the journaled source path in the repository.
  4. Re-journal: `plan_apply(&ApplyRequest::default(), ts)` then
     `execute_plan(&resolved, &request, LockPolicy::Held(guard))`, so the
     new `<ts>.COMMIT` records `content_hash(new bytes)` as the expected
     hash and `status` classifies the target CLEAN.
- Output (REQ-010): human prose / `--json` on stdout; telemetry on stderr.
- Tests: new `patina-cli/tests/promote_cli.rs` covering the copy-mode
  promote-then-CLEAN slice (CHK-007) and the template-refusal slice
  (CHK-008); add a symlink-refusal slice.

<task-scenarios>
Given an applied copy-mode target `~/.gitconfig` whose source is
`<repo>/git/gitconfig` (content `[user]\nemail = old@example.com`) and a
test that overwrites `~/.gitconfig` with `[user]\nemail = new@example.com`,
when `patina promote ~/.gitconfig --yes` runs,
then `<repo>/git/gitconfig` contains `[user]\nemail = new@example.com`, the
most recent journal record's expected hash for `~/.gitconfig` equals
`content_hash` of the new bytes, and a subsequent `patina status` reports
`~/.gitconfig` CLEAN.

Given an applied target `~/.gitconfig` whose journaled source is
`gitconfig.tmpl`,
when `patina promote ~/.gitconfig --yes` runs,
then no file is mutated, stderr contains `gitconfig.tmpl` and the substring
`template`, and the process exits 1.

Given an applied symbolic-link target `~/.zshrc`,
when `patina promote ~/.zshrc --yes` runs,
then no file is mutated, stderr names the target and explains symlink
targets share content with their source, and the process exits 1.

Suggested files: `patina-cli/src/cli.rs`, `patina-cli/src/cmd/promote.rs`,
`patina-cli/src/cmd/mod.rs`, `patina-cli/src/main.rs`,
`patina-cli/tests/promote_cli.rs`
</task-scenarios>
</task>

<task id="T-007" state="completed" covers="REQ-007">
## Add the cross-platform Windows dev-mode detection module to `patina-core`

Introduce a `patina-core::windows` module that provides the Developer Mode
detection capability REQ-007 needs, behind a cross-platform façade with
non-Windows stubs and an injectable seam so the decision logic is testable
on the macOS/Linux CI. This task delivers detection (read-side) and the
pure helpers; the elevation launch and the engine gate land in T-009.

Architecture note (DEC-008 pins the engine/CLI layering split for REQ-007):
`patina-core` must not perform user-facing IO (the Reporter layer and all
prompting live in `patina-cli`; the no-`println!` hard rule applies). So the
*capability* — read the registry flag, query elevation, query OS build —
lives here in the engine crate as plain functions returning typed values,
and the *orchestration* (prompt the user, decide) lives in the CLI (T-009).
"The engine reads the flag / spawns the helper" in REQ-007 is satisfied by
these engine-crate functions; the prompt is rendered by the CLI before it
calls `execute_plan`, which still satisfies "prompt before any filesystem
mutation."

Work:

- Create `/Users/kevin/src/patina/patina-core/src/windows/mod.rs` and
  register `pub mod windows;` + re-exports in `patina-core/src/lib.rs`.
  Define:
  - `DevModeStatus { Enabled, Disabled, Unsupported, NotWindows }` and a
    typed `WindowsError` / `DevModeError` (`thiserror`).
  - `pub fn dev_mode_status() -> DevModeStatus` — on non-Windows returns
    `NotWindows`; on Windows reads the registry flag (delegating to the
    `#[cfg(windows)]` submodule).
  - `pub fn is_elevated() -> bool` — non-Windows returns `false`; Windows
    inspects the process token.
  - `pub fn windows_build_supports_dev_mode() -> bool` — whether the
    running OS is Windows 10 1703 or newer (non-Windows: not applicable).
  - `pub fn is_unc_path(path: &Utf8Path) -> bool` — a pure prefix check
    (`\\`), defined for all platforms so `doctor`'s UNC finding is
    CI-testable.
  - `pub fn plan_has_symlink_op(plan: &ResolvedPlan) -> bool` — whether any
    `ResolvedOperation.mode` is `FileMode::Symlink` or `FileMode::SymlinkDir`
    (the predicate that gates the dev-mode flow).
  - An injectable seam for the decision: e.g. a `DevModeProbe` trait (or a
    function-pointer parameter) returning `DevModeStatus` + elevation, so
    T-009's gate logic (symlink-in-plan + disabled + not-elevated → require
    elevation; enabled → proceed; elevated → proceed-with-warning) is unit
    testable on Linux against a fake probe with no real registry.
- Create `/Users/kevin/src/patina/patina-core/src/windows/registry.rs`
  (`#[cfg(windows)]`) with the `winsafe`-backed read of the registry value
  `AllowDevelopmentWithoutDevLicense` under
  `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock`. Hold the
  registry key path and value name as constants here. (These same constants
  are duplicated deliberately in `patina-elevate` per DEC-002 — the helper
  crate must not depend on `patina-core` — so do not try to share them via
  a cross-crate dependency.)
- Add `winsafe` to `[workspace.dependencies]` in the root `Cargo.toml`, and
  reference it from `patina-core/Cargo.toml` only under
  `[target.'cfg(windows)'.dependencies]` so non-Windows builds never
  resolve it into the graph. Enable only the features REQ-007 needs
  (registry read via `advapi`, process-token elevation check, OS version
  query); do NOT enable `taskschd` — that feature is SPEC-0003's watcher
  service-install surface, and enabling an unused feature now violates the
  enable-only-needed-features rule. Run `cargo deny check` and confirm
  `cargo_deny_config.rs` still passes (`winsafe` is MIT — already on the
  allowlist — but the advisory/bans pass must be green).
- Non-Windows builds must compile clean (`cargo check` on the CI host) with
  every Windows entry point reduced to its stub.

<task-scenarios>
Given the workspace built on a non-Windows host,
when `dev_mode_status()` and `is_elevated()` are called,
then they return `DevModeStatus::NotWindows` and `false` respectively, and
no registry access is attempted.

Given the pure helper `is_unc_path`,
when called with `\\fileserver\share\dotfiles` and with `/home/user/dot`,
then it returns `true` for the first and `false` for the second.

Given a fake `DevModeProbe` reporting `Disabled` + not-elevated and a
resolved plan containing one symlink operation,
when the gate decision function is evaluated,
then it reports that elevation is required; and given the same probe
reporting `Enabled`, it reports proceed.

Suggested files: `patina-core/src/windows/mod.rs`,
`patina-core/src/windows/registry.rs`, `patina-core/src/lib.rs`,
`patina-core/Cargo.toml`, `Cargo.toml`
</task-scenarios>
</task>

<task id="T-008" state="completed" covers="REQ-008">
## Create the Windows-only `patina-elevate` helper crate

Add the third workspace crate `patina-elevate`, building the standalone
`patina-elevate.exe` helper whose sole job is to toggle the Developer Mode
registry flag under one-time UAC elevation. It has no dependency on
`patina-core` or `patina-cli` (DEC-002: minimal trust surface), and is
excluded from non-Windows release artifacts (DEC-003 / CHK-015).

Work:

- Add `"patina-elevate"` to `[workspace.members]` in the root
  `Cargo.toml`. Create `patina-elevate/Cargo.toml` inheriting the workspace
  package metadata and lint set (`[lints] workspace = true` — the helper
  enforces the same panic-free invariant as the other crates, REQ-008).
  Declare `clap` (workspace) for arg parsing and `winsafe` only under
  `[target.'cfg(windows)'.dependencies]` (registry-write features only; not
  `taskschd`). No `patina-core` / `patina-cli` dependency.
- Exclude the binary from non-Windows artifacts so `cargo build --release`
  on macOS/Linux produces NO `patina-elevate` file (CHK-015). DEC-003
  specifies `required-features` gating: declare
  `[[bin]] name = "patina-elevate" required-features = ["windows"]` with a
  `windows = []` feature enabled only on Windows targets. Cargo silently
  skips a bin whose required feature is not enabled, satisfying CHK-015.
  Verify the chosen mechanism actually drops the artifact on Linux — this
  is the most error-prone item in the crate; prove it with the
  artifact-absence test below rather than by inspection.
- `patina-elevate/src/main.rs`: a `clap`-derived surface with exactly one
  subcommand `enable-developer-mode`. Any other/unknown subcommand exits 2
  with a usage message listing the supported subcommands (clap default).
  The real registry write lives in a `#[cfg(windows)]`
  `patina-elevate/src/devmode.rs` module: set
  `AllowDevelopmentWithoutDevLicense` to `1` under
  `HKLM\...\AppModelUnlock` (constants duplicated from
  `patina-core::windows::registry`, deliberately — DEC-002 forbids the
  cross-crate dependency). Exit 0 on success; exit 1 with a typed stderr
  error naming `ERROR_ACCESS_DENIED` (or the observed HRESULT) when invoked
  non-elevated. No `unwrap`/`expect`/`panic!` in production code.
- Tests: arg-parsing / usage exit codes are cross-platform (the binary runs
  on any OS via `CARGO_BIN_EXE_patina-elevate`; the registry body is
  `cfg(windows)`-gated so on Linux the `enable-developer-mode` arm returns
  the not-Windows error path while still proving arg parsing and the
  exit-2 unknown-subcommand path). Add a build-artifact-absence test for
  CHK-015. Gate the real elevated-toggle behaviour (CHK-014) behind
  `#[cfg(windows)]` `#[ignore]` since CI is not Windows and the path needs a
  real UAC accept.

<task-scenarios>
Given the workspace built with `cargo build --release` on a non-Windows
host,
when the build completes,
then `target/release/` contains `patina` but contains no file named
`patina-elevate` or `patina-elevate.exe`.

Given the `patina-elevate` binary on any host,
when it is invoked with an unsupported subcommand,
then it exits 2 and prints a usage message listing `enable-developer-mode`.

Given a Windows host with Developer Mode OFF (gated `#[cfg(windows)]`
`#[ignore]` host test),
when an elevated `patina-elevate.exe enable-developer-mode` is invoked,
then on exit the registry value `AllowDevelopmentWithoutDevLicense` reads
`1` and the exit code is 0.

Suggested files: `Cargo.toml`, `patina-elevate/Cargo.toml`,
`patina-elevate/src/main.rs`, `patina-elevate/src/devmode.rs`,
`patina-elevate/tests/cli.rs`
</task-scenarios>
</task>

<task id="T-009" state="completed" covers="REQ-007">
## Wire the dev-mode elevation flow: helper launch, engine gate, CLI prompt

Complete REQ-007 by connecting T-007's detection to T-008's helper: add the
`ShellExecuteEx`/`runas` launch in `patina-core`, gate the engine apply so a
symlink-bearing plan cannot mutate the filesystem on a dev-mode-disabled
Windows host without consent, and have the CLI render the one-time prompt
and drive the helper. macOS and Linux never enter any of this path.

Work:

- `patina-core/src/windows/elevate.rs` (`#[cfg(windows)]`): add
  `pub fn launch_elevate_helper() -> Result<ElevationOutcome, WindowsError>`
  that resolves `patina-elevate.exe` as a sibling of the running executable
  (`std::env::current_exe`), launches it with the `runas` verb via
  `ShellExecuteEx`, waits for exit, then re-reads the Developer Mode flag.
  Model the outcomes: user-declined UAC (the canonical `ERROR_CANCELLED`
  pattern), helper-succeeded-and-flag-now-1, and helper-ran-but-flag-still-0.
  Re-export from `patina-core/src/lib.rs`.
- Engine gate in `patina-core/src/apply/engine.rs`: inside `execute`, after
  lock acquisition and `recover_orphans` but BEFORE the first
  `backup_before_overwrite` / `materialize`, add a Windows-only gate. When
  `plan_has_symlink_op` (T-007) is true and the host is dev-mode-disabled
  and not elevated, the engine must not proceed to mutate; surface this as a
  typed signal the CLI can act on (a dedicated `EngineError` variant naming
  Developer Mode, or an `ApplyResult` outcome the CLI inspects — choose the
  shape that keeps `patina-core` prompt-free and lets the CLI render the
  prompt and re-drive `execute` under `LockPolicy::Held` after the helper
  toggles the flag, per DEC-008). When elevated, proceed but emit a `tracing` warning
  recommending against running Patina elevated. The pre-existing
  `ExecutorError::WindowsSymlinkPermission` remains the backstop if a
  symlink materialization is somehow attempted without dev mode.
- CLI orchestration in `patina-cli/src/cmd/apply.rs`: on Windows, after
  planning and before calling `execute_plan`, when the plan has a symlink
  op and dev mode is disabled and the process is not elevated, prompt via
  the `Reporter` (reusing the `Tty` / `PromptReader` pattern). On decline,
  return `Ok(ExitCode::UserDeclined.code())` (5) with stderr naming
  `Developer Mode` and `patina doctor --fix` (CHK-012). On accept, call
  `launch_elevate_helper`; if the flag is then `1`, proceed with
  `execute_plan`; if the helper ran but the flag is still `0`, surface a
  typed error naming the registry path and exit 1 (REQ-007). Map the
  declined-UAC path to exit 5 in the command layer (exit code 5 is a
  command-layer control-flow decision per `exit_code.rs`, not an
  `EngineError` mapping). When the process is already elevated, suppress the
  prompt and emit the avoid-running-elevated warning.
- Cross-platform guarantee: on macOS/Linux none of this compiles into an
  active path — no registry read, no helper spawn. Prove with a CI test
  that `patina apply` on the non-Windows host neither reads the registry nor
  spawns `patina-elevate` (e.g. assert no such child process / that the
  symlink apply path is unchanged from SPEC-0001 behaviour). Gate the real
  Windows accept/decline round-trips (CHK-012 / CHK-013) behind
  `#[cfg(windows)]` `#[ignore]`; unit-test the gate decision branches on
  Linux via T-007's fake probe.

<task-scenarios>
Given a macOS or Linux host,
when `patina apply --yes` runs against a repository with a symbolic-link
`[[file]]` entry,
then no Developer Mode registry read is attempted, `patina-elevate` is not
spawned, and the apply proceeds exactly as in SPEC-0001.

Given a Windows test host with Developer Mode OFF, a repository declaring a
`[[file]] mode = "symlink"`, and a harness that declines the UAC prompt
(`#[cfg(windows)]` `#[ignore]`),
when `patina apply --yes` runs,
then no symbolic link is created, stderr contains `Developer Mode` and
`patina doctor --fix`, and the process exits 5.

Given a Windows test host with Developer Mode ON and the same repository
(`#[cfg(windows)]` `#[ignore]`),
when `patina apply --yes` runs,
then no UAC prompt is presented, no `patina-elevate.exe` is spawned, and the
symbolic link is created.

Suggested files: `patina-core/src/windows/elevate.rs`,
`patina-core/src/windows/mod.rs`, `patina-core/src/apply/engine.rs`,
`patina-core/src/error.rs`, `patina-cli/src/cmd/apply.rs`,
`patina-cli/src/exit_code.rs`, `patina-cli/tests/apply_cli.rs`
</task-scenarios>
</task>

<task id="T-010" state="completed" covers="REQ-005 REQ-010">
## Implement `patina doctor` read-only findings with `--json`

`patina doctor` inspects the environment and emits an exhaustively-specified
set of findings to stderr, exits 0 when only warnings were raised, and
supports a deterministic `--json` document. Cloud-sync directory detection
is explicitly out of scope (DEC-004 — docs-only); the finding set here is
the complete v1.0 set, and adding to it requires a SPEC amendment.

Work:

- Add a `Doctor` variant + `DoctorArgs` (`--fix`, `--json`, `--yes`) to
  `cli.rs`; create `cmd/doctor.rs`; register and dispatch in `mod.rs` /
  `main.rs`. This task implements the read-only path (no `--fix`); T-011
  adds remediation.
- Findings model in `cmd/doctor.rs`:
  `Finding { code, level, message, path: Option<Utf8PathBuf> }` with a
  `FindingCode` enum and a `Level { Info, Warning, Error }`. The v1.0
  findings:
  - `DOC-WIN-UNC` (warning) — on Windows, the resolved repository path is a
    UNC path (`is_unc_path`, T-007).
  - `DOC-WIN-DEVMODE` (warning) — on Windows, the repository declares any
    `[[file]]` with `mode = "symlink"` or `mode = "symlink-dir"` AND
    `dev_mode_status()` (T-007) is `Disabled`; the message names
    `Developer Mode` and the registry key path.
  - `DOC-WIN-OSOLD` (warning) — on Windows, the build predates Windows 10
    1703 (`windows_build_supports_dev_mode()` is false).
  - `DOC-NO-DEFAULT-REPO` (info, not warning) — no `default_repo` file in
    the state directory (`persisted_default_present`, T-002); suggested fix
    is `patina init`.
- Acquire only the SHARED lock for the read-only path (REQ-009): acquire
  `LockKind::Shared` with `SHARED_TIMEOUT`, warning and proceeding on the
  5-second timeout per SPEC-0001's read-only lock semantics.
- Exit codes: 0 when only warning/info findings were raised; 1 only on an
  error-level finding (the v1.0 set has none — the exit-1 path is reserved
  for future additions).
- Output (REQ-010): human findings to stderr; `--json` emits a single
  deterministic document on stdout with a `findings` array of objects
  `{code, level, message, path?}`, no timestamps/PIDs/random ids, so two
  runs against unchanged state are byte-identical (CHK-018).
- Tests: new `patina-cli/tests/doctor_cli.rs`. Cover the
  `DOC-NO-DEFAULT-REPO` info on a clean state dir, the determinism slice
  (CHK-018), and the UNC finding shape via the cross-platform `is_unc_path`
  predicate. Gate the Windows dev-mode finding (CHK-010) behind
  `#[cfg(windows)]` `#[ignore]`.

<task-scenarios>
Given a tempdir state directory with no `default_repo` and a valid
repository,
when `patina doctor --json` runs,
then the JSON `findings` array contains an object with
`code = DOC-NO-DEFAULT-REPO` and `level = info`, and the process exits 0.

Given the same unchanged state directory and repository,
when `patina doctor --json` is run twice,
then the two stdout outputs are byte-identical.

Given a Windows test host with Developer Mode OFF and a repository declaring
at least one `mode = "symlink"` entry (`#[cfg(windows)]` `#[ignore]`),
when `patina doctor --json` runs,
then the `findings` array contains an object with `code = DOC-WIN-DEVMODE`,
`level = warning`, and a `message` naming `Developer Mode` and the registry
path.

Suggested files: `patina-cli/src/cli.rs`, `patina-cli/src/cmd/doctor.rs`,
`patina-cli/src/cmd/mod.rs`, `patina-cli/src/main.rs`,
`patina-cli/tests/doctor_cli.rs`
</task-scenarios>
</task>

<task id="T-011" state="pending" covers="REQ-006 REQ-009">
## Implement `patina doctor --fix` interactive remediation

Extend `doctor` with `--fix`: enumerate each finding for which Patina knows
a remediation, prompt per finding, and remediate on accept. The fixable
findings in v1.0 are exactly Developer Mode missing on Windows (remedied via
the REQ-007 UAC elevation flow) and a missing `default_repo` pointer
(remedied by writing the current working directory). Non-fixable findings
(UNC paths, OS-too-old) are listed with a brief why-not explanation.

Work in `patina-cli/src/cmd/doctor.rs` (and `main.rs` TTY wiring):

- `--fix` acquires the EXCLUSIVE lock (REQ-009) — it mutates — at
  `state_dir::resolve()?.join("lock")`, distinct from the read-only path's
  shared lock.
- For each fixable finding, prompt via the `Reporter` (reuse the `Tty` /
  `PromptReader` pattern); on `y`/`Y`, remediate:
  - `DOC-WIN-DEVMODE` → invoke the elevation flow from T-009
    (`launch_elevate_helper`), then re-check `dev_mode_status()`.
  - `DOC-NO-DEFAULT-REPO` → write the current working directory's canonical
    absolute path via `write_persisted_default` (T-002).
- Non-TTY without `--yes` exits 1 with a typed error naming the missing
  `--yes` flag (no per-finding prompt is possible without a TTY).
- `--fix --yes` accepts every fixable prompt automatically; non-fixable
  findings still emit their warnings.
- Each remediation that runs writes a structured `tracing` event recording
  the finding code, the remediation chosen, and the outcome.
- Tests: extend `patina-cli/tests/doctor_cli.rs`. Cover the
  `--fix --yes` writes-`default_repo` slice on a clean state dir (the CWD
  must resolve to a valid repo), and the non-TTY-`--fix`-without-`--yes`
  exit 1. Gate the Windows dev-mode remediation (CHK-011) behind
  `#[cfg(windows)]` `#[ignore]`.

<task-scenarios>
Given a clean state directory with no `default_repo` and a current working
directory that is a valid Patina repository,
when `patina doctor --fix --yes` runs,
then the state directory's `default_repo` file is written and contains the
CWD's canonical absolute path, and the process exits 0.

Given a non-TTY shell and a state directory with a fixable finding,
when `patina doctor --fix` runs without `--yes`,
then the process exits 1 and stderr names the missing `--yes` flag.

Given a Windows test host with Developer Mode OFF, a repository with a
symlink `[[file]]`, and a harness that auto-accepts the first prompt
(`#[cfg(windows)]` `#[ignore]`),
when `patina doctor --fix` runs,
then the registry value `AllowDevelopmentWithoutDevLicense` reads `1`
afterward and the command exits 0.

Suggested files: `patina-cli/src/cmd/doctor.rs`, `patina-cli/src/main.rs`,
`patina-cli/tests/doctor_cli.rs`
</task-scenarios>
</task>

<task id="T-012" state="pending" covers="REQ-005">
## Document the new commands and verify the cloud-sync callout in `docs/USER_GUIDE.md`

SPEC-0002 adds five user-facing commands and a Windows elevation flow;
the docs-drift hard rule requires `docs/` to track observable behaviour in
the same PR. REQ-005 and DEC-004 also pin that `docs/USER_GUIDE.md` carries
the user-facing cloud-sync callout (the docs-only stance, since cloud-sync
detection is a non-goal). This task lands the documentation; it must not
break the structural anchors the existing `patina-cli/tests/docs_structure.rs`
test gates.

Work in `/Users/kevin/src/patina/docs/USER_GUIDE.md`:

- Verify the `## State directory` section still carries the cloud-sync
  paths-to-avoid bullet list (iCloud Drive / OneDrive / Dropbox / Box /
  Google Drive / Syncthing) that SPEC-0001 REQ-027 established and
  `docs_structure.rs` asserts; do not rename the section anchors that test
  keys on. Confirm the wording states cloud-sync detection is out of scope
  and the user is responsible for keeping the state directory off those
  mounts (DEC-004).
- Add a commands section documenting `init`, `add`, `remove` (and
  `--purge`), `promote`, and `doctor` (and `--fix`): purpose, the common
  `--json` / `--yes` flags, and the exit codes they reuse from SPEC-0001
  REQ-022 (0 / 1 / 4 / 5). Document the Windows Developer Mode elevation
  flow: when the one-time UAC prompt appears, that accepting it toggles
  Developer Mode via `patina-elevate.exe`, and that declining exits 5 with
  a pointer to `patina doctor --fix`. Note the manual
  `sudo loginctl enable-linger $USER` snippet is a SPEC-0003 concern and is
  out of scope here.
- Run `cargo test -p patina-cli --test docs_structure` to confirm the
  structural anchors still pass.

<task-scenarios>
Given `docs/USER_GUIDE.md` at HEAD after this task,
when `patina-cli/tests/docs_structure.rs` runs,
then it passes (the `## State directory` anchors and the cloud-sync
provider bullets remain intact).

Given `docs/USER_GUIDE.md` at HEAD after this task,
when the commands section is scanned,
then it documents each of `init`, `add`, `remove`, `promote`, and `doctor`
and names the exit codes 0 / 1 / 4 / 5 reused from SPEC-0001 REQ-022.

Suggested files: `docs/USER_GUIDE.md`, `patina-cli/tests/docs_structure.rs`
</task-scenarios>
</task>
