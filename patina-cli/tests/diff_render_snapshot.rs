//! Integration tests for `diff_render_snapshot`.
mod common;

use common::Fixture;
use common::code;

#[test]
fn partial_apply_diff_omits_unchanged_bodies_and_summarizes_the_count() {
    let f = Fixture::new();
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

    let b_out = f.home.join("b_out");
    fs_err::write(&b_out, b"b-drifted\n").expect("drift b_out");

    let preview = f.apply(&["--color", "never"]);
    assert_eq!(
        code(&preview),
        0,
        "the non-interactive preview must exit 0; stderr: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let stdout = String::from_utf8(preview.stdout).expect("apply stdout is UTF-8");

    insta::assert_snapshot!(redact_home(&stdout, &f.home));
}

#[test]
fn dropped_entry_renders_as_a_remove_block_in_the_preview() {
    let f = Fixture::new();
    let m = f.module(
        "m",
        r#"
[[file]]
source = "keep_src"
target = "~/keep_out"
mode = "copy"

[[file]]
source = "drop_src"
target = "~/drop_out"
mode = "copy"
"#,
    );
    fs_err::write(m.join("keep_src"), b"keep-bytes\n").expect("write keep_src");
    fs_err::write(m.join("drop_src"), b"drop-bytes\n").expect("write drop_src");

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    fs_err::write(
        m.join("patina.toml"),
        "[[file]]\nsource = \"keep_src\"\ntarget = \"~/keep_out\"\nmode = \"copy\"\n",
    )
    .expect("rewrite manifest without the dropped entry");

    let preview = f.apply(&["--color", "never"]);
    assert_eq!(
        code(&preview),
        0,
        "the non-interactive preview must exit 0; stderr: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let stdout = String::from_utf8(preview.stdout).expect("apply stdout is UTF-8");

    insta::assert_snapshot!(redact_home(&stdout, &f.home));
}

#[expect(
    clippy::expect_used,
    reason = "The helper runs outside #[test] functions and #[cfg(test)] modules; allow-expect-in-tests does not cover free helpers in integration crates."
)]
fn redact_home(stdout: &str, home: &camino::Utf8Path) -> String {
    let canon_home = camino::Utf8PathBuf::from_path_buf(
        dunce::canonicalize(home.as_std_path()).expect("canonicalize fixture home"),
    )
    .expect("canonical home is utf8")
    .into_string();
    let home_fwd = canon_home.replace('\\', "/");
    let home_back = home_fwd.replace('/', "\\");
    stdout
        .replace(&format!("{home_fwd}/"), "[HOME]/")
        .replace(&format!("{home_back}\\"), "[HOME]/")
}
