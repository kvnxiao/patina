//! Helpers shared by unit-test modules across the crate.

use camino::Utf8Path;

/// Create a file symlink with the right platform primitive.
#[cfg(unix)]
pub(crate) fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::unix::fs::symlink(source.as_std_path(), link.as_std_path()).expect("create symlink");
}

/// Create a file symlink with the right platform primitive.
#[cfg(windows)]
pub(crate) fn symlink_file(source: &Utf8Path, link: &Utf8Path) {
    std::os::windows::fs::symlink_file(source.as_std_path(), link.as_std_path())
        .expect("create symlink");
}

#[cfg(unix)]
pub(crate) fn symlink_dir(source: &Utf8Path, link: &Utf8Path) {
    std::os::unix::fs::symlink(source.as_std_path(), link.as_std_path())
        .expect("create dir symlink");
}

#[cfg(windows)]
pub(crate) fn symlink_dir(source: &Utf8Path, link: &Utf8Path) {
    std::os::windows::fs::symlink_dir(source.as_std_path(), link.as_std_path())
        .expect("create dir symlink");
}

pub(crate) fn source_as<'a, T: std::error::Error + 'static>(
    err: &'a (dyn std::error::Error + 'static),
) -> &'a T {
    err.source()
        .and_then(|source| source.downcast_ref::<T>())
        .expect("the error's source has the expected type")
}

pub(crate) fn assert_source_rendered_once(err: &(dyn std::error::Error + 'static)) {
    let source = err.source().expect("the error has a source").to_string();
    let rendered = crate::error::chain_message(err);
    assert_eq!(rendered.matches(&source).count(), 1, "{rendered}");
}
