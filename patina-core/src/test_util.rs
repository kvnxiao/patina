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
