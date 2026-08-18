//! Integration tests for `source_kind_validation`.
mod common;

use common::Fixture;
use common::code;

fn current_os_family() -> &'static str {
    std::env::consts::OS
}

fn assert_no_journal_artifacts(f: &Fixture) {
    let journal = f.state_root().join("journal");
    let Ok(entries) = fs_err::read_dir(&journal) else {
        return;
    };
    let artifacts: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            let path = std::path::Path::new(name);
            path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("plan") || ext.eq_ignore_ascii_case("COMMIT")
            })
        })
        .collect();
    assert!(
        artifacts.is_empty(),
        "a plan-time source-kind failure must write no journal plan/COMMIT, found: {artifacts:?}"
    );
}

#[test]
fn file_entry_with_directory_source_fails_and_directs_to_directory_table() {
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"confdir\"\ntarget = \"~/.config/app\"\n",
    );
    fs_err::create_dir(module.join("confdir")).expect("create directory source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "a `[[file]]` pointing at a directory source must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("confdir"),
        "stderr must include the offending source `confdir`, got: {stderr}"
    );
    assert!(
        stderr.contains("[[directory]]"),
        "stderr must direct the author to the `[[directory]]` table, got: {stderr}"
    );
    assert!(
        !f.home.join(".config/app").exists(),
        "a mismatched entry must materialize no target"
    );
    assert_no_journal_artifacts(&f);
}

#[test]
fn directory_entry_with_file_source_fails_and_directs_to_file_table() {
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[directory]]\nsource = \"gitconfig\"\ntarget = \"~/.config/git\"\n",
    );
    fs_err::write(module.join("gitconfig"), "[user]\n").expect("write file source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "a `[[directory]]` pointing at a file source must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[[file]]"),
        "stderr must direct the author to the `[[file]]` table, got: {stderr}"
    );
    assert_no_journal_artifacts(&f);
}

#[test]
fn when_true_entry_with_absent_source_fails_as_source_not_found() {
    let f = Fixture::new();
    f.module(
        "shell",
        "[[file]]\nsource = \"ghost\"\ntarget = \"~/.ghostrc\"\n",
    );

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "an entry whose source is absent must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ghost"),
        "stderr must include `ghost` as the missing source, got: {stderr}"
    );
    assert!(
        !f.home.join(".ghostrc").exists(),
        "a missing-source entry must materialize no target"
    );
    assert_no_journal_artifacts(&f);
}

#[test]
fn when_false_entry_with_absent_wrong_kind_source_is_not_validated() {
    let f = Fixture::new();
    f.module(
        "wm",
        "[[directory]]\nsource = \"only-on-other-os\"\ntarget = \"~/.config/wm\"\n\
         when = \"patina.os == 'definitely-not-this-os'\"\n",
    );

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a `when`-false entry with an absent source must not fail the apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("does not exist") && !stderr.contains("[[file]]"),
        "a gated-off entry must raise no missing-source or kind error, got: {stderr}"
    );
    assert!(
        !f.home.join(".config/wm").exists(),
        "a `when`-false entry must materialize no target"
    );
}

#[test]
fn when_true_entry_with_present_source_does_apply() {
    let f = Fixture::new();
    let when = format!("patina.os == '{}'", current_os_family());
    let module = f.module(
        "shell",
        &format!(
            "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"copy\"\nwhen = \"{when}\"\n"
        ),
    );
    fs_err::write(module.join("zshrc"), "export EDITOR=vim\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a `when`-true entry with a matching-kind present source must apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.home.join(".zshrc").exists(),
        "the entry's target must be materialized"
    );
}
