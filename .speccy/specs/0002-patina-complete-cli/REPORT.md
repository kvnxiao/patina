---
spec: SPEC-0002
outcome: implemented
generated_at: 2026-05-31T00:00:00Z
---

# REPORT: SPEC-0002 Patina complete CLI surface and Windows symlink elevation

<report spec="SPEC-0002">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002">
T-001 introduced the `toml_edit`-backed manifest writer (`scaffold_root_manifest`,
`append_file_entry`, `remove_file_entry`). T-002 added `write_persisted_default`,
`default_repo_pointer_path`, and `persisted_default_present` to
`patina-core::discovery`. T-003 implemented `patina init`: resolves or creates the
target directory, acquires the exclusive lock, refuses with exit 1 and "already
exists" if a `patina.toml` is present, otherwise writes the scaffolded manifest and
the persisted default pointer, and prints the `patina add` next-step hint. `--json`
emits a deterministic JSON document; failures are byte-stable across reruns (CHK-017).
Retry count: 0.
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-003 CHK-004">
T-004 implemented `patina add`: tilde-expands the input path, resolves the
repository root, acquires the exclusive lock, determines the module and mode (via
`--module` / `--symlink` / `--copy` / `--template` flags or TTY prompts), refuses
with exit 1 on already-managed paths, copies the file into `<repo>/<module>/` (move
semantics were clarified to copy-and-leave per CHK-003's authoritative scenario),
appends a `[[file]]` entry via `append_file_entry`, and leaves the original target
as a regular file. Two mode flags produce clap exit 2. Non-TTY without `--module` or
a mode flag exits 1. In v1.0 only `--symlink`, `--copy`, `--template` are exposed
(not `symlink-dir`/`copy-tree`). Retry count: 1.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-005 CHK-006">
T-005 implemented `patina remove`: reads the latest journal commit, locates the
matching `ExpectedTarget`, reconstructs last-applied content (source bytes for
symlink/copy; MiniJinja re-render for template targets per DEC-005), replaces the
target with a regular file (or deletes it with `--purge`), calls `remove_file_entry`
on the owning module manifest, then re-journals the new managed set via a
`LockPolicy::Held` re-apply so `patina status` omits the target (unmanaged) rather
than classifying it ORPHANED. Repository source file is not deleted. Retry count: 0.
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-007 CHK-008">
T-006 implemented `patina promote`: locates the `ExpectedTarget` in the latest
commit, refuses with exit 1 and a typed error on symlink-mode targets (message names
the target and explains "symlink targets share content with their source") and on
`.tmpl` sources (message names the template path and "template"), copies the current
target bytes to the repository source, then re-journals under `LockPolicy::Held` so
the new `<ts>.COMMIT` records the updated blake3 hash and `patina status` reports
CLEAN. Retry count: 0.
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-010">
T-007 introduced `patina-core::windows` with `dev_mode_status()`, `is_elevated()`,
`windows_build_supports_dev_mode()`, `is_unc_path()`, `plan_has_symlink_op()`, and
a `DevModeProbe` injectable seam for CI testability on non-Windows hosts. T-010
implemented `patina doctor`: acquires the shared lock, evaluates all four v1.0
findings (`DOC-WIN-UNC`, `DOC-WIN-DEVMODE`, `DOC-WIN-OSOLD`, `DOC-NO-DEFAULT-REPO`),
emits warnings to stderr, exits 0 on warning/info-only findings. `--json` emits a
deterministic `findings` array with `{code, level, message, path?}` fields; two
consecutive runs against unchanged state are byte-identical (CHK-018). Cloud-sync
detection is deferred per DEC-004; the `docs/USER_GUIDE.md` callout is verified by
T-012. Retry count: 0.
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-011">
T-011 extended `doctor` with `--fix`: acquires the exclusive lock, prompts per
fixable finding (`DOC-WIN-DEVMODE` -> elevation flow; `DOC-NO-DEFAULT-REPO` -> write
CWD's canonical path via `write_persisted_default`), refuses non-fixable findings
with a brief explanation, exits 1 in non-TTY without `--yes`, auto-accepts all
fixable prompts with `--fix --yes`. Each remediation emits a structured `tracing`
event recording finding code, remediation chosen, and outcome. Retry count: 0.
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-012 CHK-013">
T-007 provided the detection capability (registry read, elevation query, OS build
check) in `patina-core::windows` as IO-free functions per DEC-008. T-008 added the
`patina-elevate` crate. T-009 wired the full flow: `patina-core/src/windows/elevate.rs`
adds `launch_elevate_helper` (`ShellExecuteEx`/`runas`, re-reads flag after exit);
the engine gate in `execute` blocks a symlink-bearing plan when dev mode is disabled
and the process is not elevated, surfacing a typed signal to the CLI; `patina-cli`
renders the one-time UAC prompt via `Reporter`, maps decline to exit 5 naming
`Developer Mode` and `patina doctor --fix` (CHK-012), and calls `launch_elevate_helper`
on accept. Elevated invocations emit a `tracing` warning against running Patina
elevated. macOS/Linux never enter the code path. Windows CHK-012/CHK-013 are gated
`#[cfg(windows)]` `#[ignore]`; the gate decision branches are unit-tested on Linux
via the `DevModeProbe` seam. Retry count: 0.
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-014 CHK-015">
T-008 created `patina-elevate/`: no dependency on `patina-core` or `patina-cli`,
`[lints] workspace = true` (same panic-free invariant), `clap`-derived surface with
exactly one subcommand `enable-developer-mode`. The `windows` Cargo feature gates
the binary so non-Windows builds emit no `patina-elevate` artifact (CHK-015, verified
by a build-artifact-absence test). The real elevated registry-write path
(`patina-elevate/src/devmode.rs`) is `#[cfg(windows)]`; CHK-014 is
`#[cfg(windows)]` `#[ignore]`. Cross-platform exit-2 (unknown subcommand) and
exit-1 (non-elevated) paths are tested on all CI hosts. Retry count: 0.
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-016">
All five mutating commands (`init`, `add`, `remove`, `promote`, `doctor --fix`)
acquire the exclusive lock at `<state>/patina/lock` before any filesystem mutation.
`doctor` (read-only) acquires only the shared lock and proceeds with a warning after
the 5-second read timeout. `remove` and `promote` drive their re-apply under
`LockPolicy::Held` (supplying their already-acquired guard) so the engine re-apply
does not self-contend. Two concurrent `patina add` invocations serialize without
journal interleaving (CHK-016). Retry count: 0.
</coverage>

<coverage req="REQ-010" result="satisfied" scenarios="CHK-017 CHK-018">
All five SPEC-0002 commands accept `--json` and emit a single deterministic JSON
document on stdout with no wall-clock timestamps, PIDs, or random IDs. Telemetry,
warnings, and prompts go to stderr regardless of `--json`. CHK-017 verifies that
the second and third `patina init T --json` runs (both failures) produce
byte-identical stdout. CHK-018 verifies that two consecutive `patina doctor --json`
runs against unchanged state are byte-identical. Retry count: 0.
</coverage>

</report>

## Notes

Vet invocation 1 (2026-05-31) cleared drift on round 1 with no code fixes required.
The simplifier applied two behavior-preserving cleanups: `detect_tty()` helper
extracted in `main.rs` (eliminating six identical TTY-detection blocks); single-use
`ManagedMatch` newtype inlined in `cmd/add.rs` (returning `Option<String>` directly).

Two non-blocking observations from the drift reviewer, neither requiring code changes:
(1) SPEC prose says "moves" but implementation correctly copies (CHK-003 is the
authoritative contract; T-004 journal documents the resolution); prose alignment is a
v1.1 SPEC hygiene item. (2) `patina-elevate.exe` requires `--features windows` at
build time; the "no release/packaging pipeline" non-goal covers this - the release
tooling owns passing the feature flag.
