//! Golden-output coverage for the human diff body (REQ-010, CHK-014).
//!
//! REQ-010: in a partial apply the rendered diff emits a per-entry block only
//! for `Create` / `Update` targets; `Unchanged` targets produce no block and
//! are reported by exactly one deterministic summary count line.
//!
//! The diff is exercised end-to-end through the real `patina apply` binary on
//! the non-interactive path: with no `--yes` and a non-TTY stdin, `apply`
//! renders the diff to stdout and then previews-only (exit 0) without writing
//! anything. The captured stdout is therefore exactly the rendered diff body,
//! which the snapshot pins.
//!
//! The per-run tempdir home prefix is redacted to `[HOME]` so the snapshot is
//! stable across runs and machines while still proving the path-naming shape
//! (one `copy [HOME]/...` line for the single Update block).

mod common;

use common::Fixture;
use common::code;

/// CHK-014: a plan with one Update target and three Unchanged targets renders
/// exactly one per-entry block (the Update) plus one summary line stating three
/// unchanged.
#[test]
fn partial_apply_diff_omits_unchanged_bodies_and_summarizes_the_count() {
    let f = Fixture::new();
    // Four `copy` entries. After the first apply all four targets match their
    // source bytes. We then drift exactly one (`b_out`) so the next plan
    // classifies `b` as Update and the other three as Unchanged — the CHK-014
    // shape of one Update + three Unchanged.
    let m = f.module(
        "m",
        r#"
[[file]]
source = "a_src"
target = "~/a_out"
mode = "copy"

[[file]]
source = "b_src"
target = "~/b_out"
mode = "copy"

[[file]]
source = "c_src"
target = "~/c_out"
mode = "copy"

[[file]]
source = "d_src"
target = "~/d_out"
mode = "copy"
"#,
    );
    fs_err::write(m.join("a_src"), b"a-bytes\n").expect("write a_src");
    fs_err::write(m.join("b_src"), b"b-source\n").expect("write b_src");
    fs_err::write(m.join("c_src"), b"c-bytes\n").expect("write c_src");
    fs_err::write(m.join("d_src"), b"d-bytes\n").expect("write d_src");

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    // Drift exactly one target's bytes: `b_out` now differs from `b_src`, so
    // the next plan classifies it Update while a/c/d stay Unchanged.
    let b_out = f.home.join("b_out");
    fs_err::write(&b_out, b"b-drifted\n").expect("drift b_out");

    // No `--yes`, non-TTY stdin (subprocess): `apply` renders the diff and
    // previews-only (exit 0), so stdout is exactly the rendered diff body.
    let preview = f.apply(&[]);
    assert_eq!(
        code(&preview),
        0,
        "the non-interactive preview must exit 0; stderr: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let stdout = String::from_utf8(preview.stdout).expect("apply stdout is UTF-8");

    // Redact the per-run tempdir home prefix to a stable token so the snapshot
    // is reproducible across runs and machines while still proving the single
    // Update block names its target path (`copy [HOME]/b_out`). The renderer
    // prints the target resolved through `resolve_location`, which canonicalizes
    // the parent (this home dir) and strips the Windows verbatim prefix — so the
    // printed prefix is the *canonical* home, not the raw env value: on macOS the
    // tempdir's `/var/...` resolves to `/private/var/...`, and on Windows a
    // junction / short-name / `\\?\` form can differ from the env string (Linux
    // `/tmp` canonicalizes to a no-op, which is why only it matched the raw form).
    // Canonicalize the home the same way (`dunce::canonicalize` mirrors the
    // engine's `canonicalize`) before redacting, and cover both separator
    // spellings. A literal string replace (not a regex) avoids re-enabling
    // insta's `filters` feature for this substitution.
    let canon_home = camino::Utf8PathBuf::from_path_buf(
        dunce::canonicalize(f.home.as_std_path()).expect("canonicalize fixture home"),
    )
    .expect("canonical home is utf8")
    .into_string();
    let home_fwd = canon_home.replace('\\', "/");
    let home_back = home_fwd.replace('/', "\\");
    let redacted = stdout
        .replace(&format!("{home_fwd}/"), "[HOME]/")
        .replace(&format!("{home_back}\\"), "[HOME]/");

    insta::assert_snapshot!(redacted);
}
