//! Column alignment for the CLI's multi-row human surfaces.
//!
//! Every table-shaped listing buffers `\t`-separated cells into one block and
//! runs it through [`align`], so a single setting decides the padding for all
//! of them and no surface hand-counts spaces.

use std::io::Write;
use tabwriter::TabWriter;

/// Align tab-separated cells into columns.
///
/// ANSI mode measures a cell by printable width, so a painted cell pads exactly
/// as its stripped form does; that is what keeps piped, `--color never`, and
/// `NO_COLOR` output aligned identically to a terminal's. Writing to a `Vec`
/// cannot fail, so the unaligned fallback is unreachable and exists only
/// because a print path must not carry a panic.
#[must_use = "the aligned block is what gets printed"]
pub fn align(table: &str) -> String {
    let mut aligned: Vec<u8> = Vec::new();
    let mut writer = TabWriter::new(&mut aligned)
        .minwidth(0)
        .padding(2)
        .ansi(true);
    if writer.write_all(table.as_bytes()).is_err() || writer.flush().is_err() {
        return table.to_owned();
    }
    drop(writer);
    String::from_utf8(aligned).unwrap_or_else(|_| table.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The widest cell in a column sets that column's width, so every row's
    /// next cell begins at the same offset. Hand-padding is what this replaces,
    /// and a row that drove its own width would defeat the point.
    #[test]
    fn every_row_starts_its_second_column_at_one_offset() {
        let aligned = align("a\tone\nlonger\ttwo\n");
        let mut lines = aligned.lines();
        let narrow = lines.next().expect("the narrow row");
        let wide = lines.next().expect("the wide row");
        let column = narrow.find("one").expect("the narrow row keeps its cell");
        assert_eq!(
            wide.find("two"),
            Some(column),
            "both second cells must start at column {column}: {aligned:?}"
        );
    }

    /// The `.ansi(true)` setting is what makes color additive over an aligned
    /// block: a cell wrapped in escapes must pad by its printable width, so
    /// stripping the escapes gives back the plain alignment byte for byte. Byte
    /// measurement would pad the colored form short and misalign piped output.
    #[test]
    fn a_painted_cell_pads_by_printable_width() {
        let plain = align("a\tone\nlonger\ttwo\n");
        let painted = align("\u{1b}[32ma\u{1b}[0m\tone\nlonger\ttwo\n");
        assert_ne!(plain, painted, "the painted block must carry its escapes");
        assert_eq!(
            anstream::adapter::strip_str(&painted).to_string(),
            plain,
            "stripping color must leave the plain alignment untouched"
        );
    }
}
