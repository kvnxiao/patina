//! Column alignment for the CLI's multi-row human surfaces.
//!
//! Every table-shaped listing buffers `\t`-separated cells into one block, runs
//! it through [`align`], and prints it with [`emit_aligned`]. One setting
//! decides the padding for all of them, and no command module counts spaces.
//!
//! The `patina debug` dumps in `patina_core` are not clients. They render a
//! developer post-mortem, and `patina_core` cannot reach the CLI's palette.

use crate::output::reporter::Reporter;
use std::io::Write;
use tabwriter::TabWriter;

/// Align tab-separated cells into columns.
///
/// ANSI mode measures a cell by printable width, so a painted cell pads
/// exactly as its stripped form does. Writing to a `Vec` cannot fail, so the
/// unaligned fallback is unreachable; it exists because a print path must not
/// panic.
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

/// Join `cells` into one tab-separated, newline-terminated row.
///
/// [`align`] pads a cell only when a tab follows it, so a row ends after its
/// last cell and trails no padding.
#[must_use = "the row is what gets buffered into the table"]
pub fn row(cells: &[&str]) -> String {
    let mut row = cells.join("\t");
    row.push('\n');
    row
}

/// Align a buffered block and print it to the out stream in one write.
///
/// Every row a caller buffers ends in `\n`, and alignment preserves those
/// terminators, so one write reproduces the whole listing. That keeps a long
/// listing to a single stdout lock and flush. An empty block prints nothing.
pub fn emit_aligned(table: &str, reporter: &mut impl Reporter) {
    reporter.out_block(&align(table));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;

    /// The widest cell in a column sets that column's width, so every row's
    /// next cell begins at the same offset. A row that padded to its own
    /// width would leave the block ragged.
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

    /// Writing the block in one write is only safe while alignment keeps every
    /// terminator a caller buffered. A dropped final `\n` runs a listing into
    /// whatever the command prints next. An extra one opens a blank line no
    /// plain-output test expects.
    #[test]
    fn emitting_a_table_terminates_every_row_and_adds_no_blank_line() {
        let table = [row(&["a", "one"]), row(&["longer", "two"])].concat();
        let mut reporter = BufferReporter::new();
        emit_aligned(&table, &mut reporter);
        assert_eq!(
            reporter.out.lines().count(),
            2,
            "two rows in, two rows out: {:?}",
            reporter.out
        );
        assert!(
            reporter.out.ends_with("two\n"),
            "the last row must keep its terminator: {:?}",
            reporter.out
        );
    }

    /// A listing with no rows must produce no output at all. A stray
    /// terminator would leave a blank line where the table would have gone.
    #[test]
    fn emitting_an_empty_table_prints_nothing() {
        let mut reporter = BufferReporter::new();
        emit_aligned("", &mut reporter);
        assert_eq!(reporter.out, "");
    }

    /// The `.ansi(true)` setting makes color additive over an aligned block.
    /// A cell wrapped in escapes must pad by its printable width, so
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
