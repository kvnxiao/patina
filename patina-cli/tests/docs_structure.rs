#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Docs-structure integration test (REQ-027, CHK-056 / CHK-057 / CHK-058).
//!
//! Parses `docs/ARCHITECTURE.md` and `docs/USER_GUIDE.md` as `CommonMark` and
//! asserts that each carries its required set of `##`-level headings by exact
//! text, and that the `## State directory` section of the user guide lists the
//! six cloud-sync providers the per-machine state directory must not live on.
//!
//! The test gates *structure* only — heading existence by exact text and
//! bullet membership in a named section. It never substring-matches the prose
//! around the headings, per the test-hygiene rule in AGENTS.md prohibiting
//! tests over editorial choices. A heading rename or a bullet-text change (even
//! a prefix-preserving one like `Dropbox` → `Dropbox (via Smart Sync)`) fails
//! the test naming the missing literal.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use pulldown_cmark::Event;
use pulldown_cmark::HeadingLevel;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use std::collections::BTreeSet;

/// Absolute path to a file under the workspace `docs/` directory.
/// `CARGO_MANIFEST_DIR` is the `patina-cli` crate dir; the workspace root is
/// its parent.
fn docs_path(file: &str) -> Utf8PathBuf {
    let manifest_dir = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .expect("patina-cli has a workspace-root parent");
    root.join("docs").join(file)
}

/// The exact text of every `##`-level (H2) heading in `markdown`, in document
/// order collapsed into a set. Heading text is the concatenation of the inline
/// text spans between the heading start and end events, which strips the `##`
/// markers and any surrounding markdown but preserves the literal words.
fn h2_headings(markdown: &str) -> BTreeSet<String> {
    let mut headings = BTreeSet::new();
    let mut current: Option<String> = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            }) => current = Some(String::new()),
            Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                if let Some(text) = current.take() {
                    headings.insert(text);
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(buf) = current.as_mut() {
                    buf.push_str(&text);
                }
            }
            _ => {}
        }
    }
    headings
}

/// The exact text of every top-level list item appearing in the body of the
/// `## <heading>` section — that is, after the named H2 heading and before the
/// next H2 heading (or end of document). Item text is the concatenation of the
/// inline text spans inside each `<li>`, which strips list markers but
/// preserves the literal words, so a prefix-extended bullet
/// (`Dropbox (via Smart Sync)`) is a distinct entry from `Dropbox`.
fn section_list_items(markdown: &str, heading: &str) -> BTreeSet<String> {
    let mut items = BTreeSet::new();
    let mut in_section = false;
    let mut collecting_heading: Option<String> = None;
    let mut current_item: Option<String> = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            }) => {
                in_section = false;
                collecting_heading = Some(String::new());
            }
            Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                if let Some(text) = collecting_heading.take() {
                    in_section = text == heading;
                }
            }
            Event::Start(Tag::Item) if in_section => current_item = Some(String::new()),
            Event::End(TagEnd::Item) if in_section => {
                if let Some(text) = current_item.take() {
                    items.insert(text);
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(buf) = collecting_heading.as_mut() {
                    buf.push_str(&text);
                } else if let Some(buf) = current_item.as_mut() {
                    buf.push_str(&text);
                }
            }
            _ => {}
        }
    }
    items
}

fn read_doc(file: &str) -> String {
    let path = docs_path(file);
    fs_err::read_to_string(&path).expect("read docs file")
}

#[test]
fn architecture_has_required_h2_headings() {
    // CHK-056: docs/ARCHITECTURE.md carries its four structural anchors by
    // exact text. A renamed or deleted heading drops out of the set and fails
    // here naming the missing literal.
    let headings = h2_headings(&read_doc("ARCHITECTURE.md"));
    for required in [
        "Engine layers",
        "Journal format",
        "Apply phases",
        "Recovery",
    ] {
        assert!(
            headings.contains(required),
            "docs/ARCHITECTURE.md missing required `## {required}` heading; found {headings:?}"
        );
    }
}

#[test]
fn user_guide_has_required_h2_headings() {
    // CHK-057: docs/USER_GUIDE.md carries its six structural anchors by exact
    // text.
    let headings = h2_headings(&read_doc("USER_GUIDE.md"));
    for required in [
        "Installation",
        "Declaring dotfiles",
        "Apply flow",
        "State directory",
        "Recovery",
        "Troubleshooting",
    ] {
        assert!(
            headings.contains(required),
            "docs/USER_GUIDE.md missing required `## {required}` heading; found {headings:?}"
        );
    }
}

#[test]
fn user_guide_state_directory_lists_cloud_sync_providers() {
    // CHK-058: the `## State directory` section lists each of the six
    // cloud-sync providers as a literal bullet entry. Membership is by exact
    // text, so a prefix-extended bullet (`Dropbox (via Smart Sync)`) does NOT
    // satisfy the `Dropbox` requirement.
    let items = section_list_items(&read_doc("USER_GUIDE.md"), "State directory");
    for required in [
        "iCloud Drive",
        "OneDrive",
        "Dropbox",
        "Box",
        "Google Drive",
        "Syncthing",
    ] {
        assert!(
            items.contains(required),
            "docs/USER_GUIDE.md `## State directory` section missing bullet `{required}`; found {items:?}"
        );
    }
}
