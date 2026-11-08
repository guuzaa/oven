//! A line diff: what changed between two texts, and how a line names its number.

use similar::{ChangeTag, TextDiff};

/// A line of a file with its 1-based number: `<line>:<text>`.
///
/// The number is what lets a reader find the line in the file, so it comes
/// first and the text follows it.
pub fn numbered(line: usize, text: &str) -> String {
    format!("{line}:{text}")
}

/// The lines that changed between two texts, each marked `-` or `+` and
/// numbered: the removed lines in the file as it was, the added ones in the
/// file as it now is.
///
/// The lines that stayed are left out: what changed is the answer, not the
/// text again. A line that was replaced shows as removed and then added.
pub fn changed_lines(before: &str, after: &str) -> String {
    let diff = TextDiff::from_lines(before, after);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let (sign, index) = match change.tag() {
            ChangeTag::Equal => continue,
            ChangeTag::Delete => ('-', change.old_index()),
            ChangeTag::Insert => ('+', change.new_index()),
        };
        let Some(index) = index else {
            continue;
        };
        let text = change.value().trim_end_matches(['\n', '\r']);
        out.push(sign);
        out.push(' ');
        out.push_str(&numbered(index + 1, text));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{changed_lines, numbered};

    #[test]
    fn a_line_that_stayed_is_left_out() {
        assert_eq!(changed_lines("one\ntwo\n", "one\ntwo\n"), "");
    }

    #[test]
    fn a_changed_line_shows_as_removed_and_added() {
        assert_eq!(
            changed_lines("one\ntwo\n", "one\nthree\n"),
            "- 2:two\n+ 2:three\n"
        );
    }

    #[test]
    fn an_added_line_shows_as_added() {
        assert_eq!(changed_lines("one\n", "one\ntwo\n"), "+ 2:two\n");
    }

    #[test]
    fn a_removed_line_shows_as_removed() {
        assert_eq!(changed_lines("one\ntwo\n", "one\n"), "- 2:two\n");
    }

    #[test]
    fn the_lines_keep_their_order() {
        assert_eq!(
            changed_lines("one\ntwo\nthree\n", "one\nthree\nfour\n"),
            "- 2:two\n+ 3:four\n"
        );
    }

    #[test]
    fn a_text_without_a_trailing_newline_still_diffs() {
        assert_eq!(changed_lines("one", "two"), "- 1:one\n+ 1:two\n");
    }

    #[test]
    fn a_line_names_its_number() {
        assert_eq!(numbered(7, "text"), "7:text");
    }
}
