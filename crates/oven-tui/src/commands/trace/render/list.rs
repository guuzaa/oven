//! The runs of the workspace, as a table.

use std::fmt::Write;

use oven_app::trace::TraceSummary;

use super::format::{age, bytes, short_id, width_of};

const NO_TRACES: &str = "no traces";

/// The runs of the workspace, one per line: which run, which session, how
/// much it wrote, and when it was last written.
pub fn list(traces: &[TraceSummary]) -> String {
    if traces.is_empty() {
        return NO_TRACES.to_owned();
    }
    let rows: Vec<Row> = traces.iter().map(Row::of).collect();
    let widths = Widths::of(&rows);

    let mut out = String::new();
    write_row(&mut out, ["run", "session", "size", "age"], &widths);
    for row in &rows {
        write_row(&mut out, row.cells(), &widths);
    }
    out.trim_end().to_owned()
}

/// One run of the workspace, as the list shows it.
struct Row {
    id: String,
    session: String,
    size: String,
    age: String,
}

impl Row {
    fn of(trace: &TraceSummary) -> Self {
        Self {
            id: trace.id.clone(),
            session: short_id(trace.session_id.as_deref()),
            size: bytes(trace.bytes),
            age: age(trace.modified),
        }
    }

    fn cells(&self) -> [&str; 4] {
        [&self.id, &self.session, &self.size, &self.age]
    }
}

/// The widths of the columns of the list: as wide as the widest cell, or as
/// the header that names them.
struct Widths {
    id: usize,
    session: usize,
    size: usize,
}

impl Widths {
    fn of(rows: &[Row]) -> Self {
        Self {
            id: width_of(rows.iter().map(|row| row.id.as_str()), "run"),
            session: width_of(rows.iter().map(|row| row.session.as_str()), "session"),
            size: width_of(rows.iter().map(|row| row.size.as_str()), "size"),
        }
    }
}

/// One line of the list: the three columns of a width, and the age.
fn write_row(out: &mut String, cells: [&str; 4], widths: &Widths) {
    let _ = writeln!(
        out,
        "{:<id$}  {:<session$}  {:>size$}  {}",
        cells[0],
        cells[1],
        cells[2],
        cells[3],
        id = widths.id,
        session = widths.session,
        size = widths.size,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_without_runs_says_so() {
        assert_eq!(list(&[]), NO_TRACES);
    }

    #[test]
    fn the_list_has_a_header_and_the_short_ids() {
        let traces = vec![TraceSummary {
            id: "a3f9c2b1d4e7".to_owned(),
            session_id: Some("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678".to_owned()),
            started_at: None,
            bytes: 2048,
            modified: None,
        }];

        let list = list(&traces);

        assert!(list.starts_with("run"), "{list}");
        assert!(list.contains("a3f9c2b1d4e7"), "{list}");
        assert!(list.contains("3e4f5678"), "{list}");
        assert!(list.contains("2.0 KB"), "{list}");
    }
}
