---
date: 2026-11-14
questions:
  - Which command, output, and consent boundaries are observable to users and scripts?
  - Which repository-management and Windows integration claims differ from the intended contract?
  - What do the isolated command probes establish, and which platform conditions remain untested?
observed-in:
  - Patina source and tests at 4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc
  - Windows debug CLI built from that revision, with temporary repository, home, and state directories
recheck-when: Command dispatch, JSON branches, repository writers, elevation, Defender ownership, or the build toolchain changes.
---

# Commands and platform evidence

Patina has distinct consent and persistence policies for deployment, repository edits, diagnostics, and Windows host settings. JSON selects presentation; it does not universally mean preview. The specification defines intended outcomes, while this document records the inspected implementation and bounded observations.

The baseline was inspected with the user's untracked `DISCUSS_VACUOUS_TEST_AUDIT.md` preserved. Runtime probes used the Windows debug executable built from that revision. No probe changed real deployment targets, Developer Mode, or Defender settings. macOS and Linux behavior was inspected through source and test assertions, not a native execution matrix.

## CLI and output contracts

[`cli.rs`](../../patina-cli/src/cli.rs) defines the command and flag surface. [`main.rs`](../../patina-cli/src/main.rs) routes generic errors to stderr, while command modules implement typed JSON results and refusals. [`output/reporter.rs`](../../patina-cli/src/output/reporter.rs) deliberately discards output write and flush errors. As a result, successful command exit does not prove output delivery.

The independent command-combination checks found these structured-output defects:

- `doctor --fix --yes --json` printed human output when recording a default repository and when there were no findings. [`doctor.rs`](../../patina-cli/src/cmd/doctor.rs) dispatches its fix path without a JSON-specific result path. Tests cover JSON and remediation separately. The intended spec defines a remediation envelope and requires their combination to remain structured.
- `remote update --json --yes` with no declared remotes exited 0 and printed `No remotes are declared.`. The empty-registry branch in [`remote.rs`](../../patina-cli/src/cmd/remote.rs) precedes JSON reporting. The intended result is an empty JSON array.
- Source inspection of [`apply.rs`](../../patina-cli/src/cmd/apply.rs), `run_json`, `drive_dev_mode_gate`, and `drive_elevation`, shows that a Windows Developer Mode request prints ordinary prose to stdout before JSON. No actual elevation was launched. The intended stream contract routes that explanation to stderr.

Several combinations have deliberate exceptions. `remote check --hook --json` is silent because hook mode wins. Remove and promote without required consent can exit 5 with empty stdout. Rollback with no history and generic planning failures can exit 1 with stderr diagnostics rather than JSON. These exceptions do not authorize mixing human prose into a completed JSON result.

The intended remote observation fields, doctor remediation results, and Defender `to_adopt` list are concrete interface designs for the confirmed behaviors. They are not claims about fields present in the baseline executable.

## Repository-management boundaries

[`init.rs`](../../patina-cli/src/cmd/init.rs), [`add.rs`](../../patina-cli/src/cmd/add.rs), and [`promote.rs`](../../patina-cli/src/cmd/promote.rs) perform multiple writes without a whole-command transaction. Init writes the root manifest before the default pointer. Add copies content before editing its manifest. Promote writes source content before publishing its ownership record. The confirmed intended guarantee is complete old-or-new individual files under process interruption, with partial command completion reported honestly. It does not add transaction-wide rollback to these commands.

Add preserves target files until apply deploys the new declaration. It reads file-symlink content during adoption and refuses occupied repository source destinations, including dangling links. Explicit `--module` is joined to the root in the baseline without validating that it is an immediate child name; a path such as `../outside` can escape the module layout. The intended immediate-child module constraint applies to this writer as well as discovery. This defect was source-traced, not runtime-tested.

[`config/writer.rs`](../../patina-core/src/config/writer.rs), `remove_file_entry`, matches scalar `target` only. A file declared with `targets` can be applied but cannot be edited by this removal path. The confirmed removal contract supports one destination from either form, preserves siblings and their order, and deletes the declaration when empty. Directory declarations and tree leaves remain manual manifest edits. This limitation was identified by tracing a shared declaration form across operations; it is not an intentional incompatibility to preserve.

[`remove.rs`](../../patina-cli/src/cmd/remove.rs) reconstructs ordinary removal output from current recorded source content, using current variables for local templates. It does not preserve live target edits or historical rendered bytes. Its suffix-only template detection incorrectly treats remote `.tmpl` files as templates; [remote research](remote-sources.md) records that trust-boundary defect. [`promote.rs`](../../patina-cli/src/cmd/promote.rs) refuses remote and template sources, symlink targets, and targets affected by just-completed recovery. Removal recovery and ownership-history protections are covered by [transaction research](transactions-and-recovery.md).

## Doctor and Windows integration

[`doctor.rs`](../../patina-cli/src/cmd/doctor.rs) reports observations and offers individual remedies. Skipping an optional remedy does not fail the diagnostic session. `--fix` without a TTY or `--yes` fails rather than silently accepting repairs. A successful doctor report is not a certificate that every configuration or security property was checked: the ignored-target query can suppress errors, and the baseline symlink declaration scan examines file entries without traversing directory declarations. That scan can miss a directory-only symlink deployment needing Developer Mode. The intended diagnostic scope includes declared symbolic-link operations.

Windows apply and doctor use a separate elevation helper. Source inspection establishes requested actions and receipt handling, but does not establish successful UAC interaction on every supported Windows setup. Existing privilege checks were observed read-only; no setting was toggled during research.

Defender paths are derived from the selected repository and declared target roots, with safety checks excluding broad or protected paths. The helper independently validates requested paths. Live reads can be withheld or fail, so [`defender.rs`](../../patina-cli/src/cmd/defender.rs) distinguishes live data from its ownership ledger. A missing or inconclusive helper receipt does not prove that external settings remained unchanged.

The baseline human `defender apply` can record already-present exclusions as Patina-owned without prompting when the external setting diff is empty. JSON preview does not make the same ownership change. A subsequent clear can remove adopted paths. The confirmed design requires adoption to appear in the preview and receive confirmation or `--yes`, including when no external setting needs an addition. Defender commands do not participate in the deployment journal and do not acquire its lock merely because they use the same state directory. No Defender mutation was executed in this investigation.

## Isolated command observations

The coordinator used Python 3.15.1 on Windows to invoke `target/debug/patina.exe` in temporary fixtures, setting `PATINA_REPO`, `HOME`, `USERPROFILE`, `LOCALAPPDATA`, and `XDG_STATE_HOME` to isolated locations and clearing `PATINA_PROFILE`. Each subprocess had a 30-second bound. The fixtures used a root-marked manifest and a `shell` module with ordinary copy entries.

| Condition and action | Observed result | Limit |
| --- | --- | --- |
| Apply source `version one`, then change source to `version two` and target to `target edit`; run `remove --yes --json`. | Exit 0; target becomes `version two`. | One regular-file copy case on Windows; not every template, link, or I/O failure. |
| Preview an entry with `when = "not missing"`. | Exit 0; the target is planned for creation. | Demonstrates that this condition accepts an undefined variable; preview does not prove execution. |
| Close the stdout pipe's read end before `apply --yes --json`. | Exit 0; target has current source bytes; stderr is empty. | Demonstrates this broken-pipe path on Windows, not every output failure or OS. |

Separate isolated fixtures reproduced the doctor-fix and empty-update JSON defects above, without host remediation. Other probes distinguished JSON error results from stderr-only failures. The Windows elevation-output finding remains source inspection only.

## Baseline verification limits

The baseline `just check` stopped in Clippy at `patina-elevate/src/lib.rs:116`, where `clippy::double_must_use` rejects a redundant attribute on a function returning `ExitCode`. A separate `just test` built the debug CLI but stopped in `add_serializes_behind_a_held_exclusive_lock`. That test derived the helper executable's path from an assumed directory layout that differed from the generated `target/debug/build/.../out/` location. These baseline failures prevent a claim that the full suite passed. They do not invalidate the separately executed bounded probes.

These observations apply to the research baseline, before the Rust 1.100 update and CI fixes. Existing test assertions cited by the research establish intended checked conditions; only explicitly reported executions establish observed results in this environment.
