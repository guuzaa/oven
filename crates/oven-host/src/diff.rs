//! A line diff: what changed between two texts.

use similar::{ChangeTag, TextDiff};

/// The lines that changed between two texts, each marked `-` or `+`.
///
/// The lines that stayed are left out: what changed is the answer, not the
/// text again. A line that was replaced shows as removed and then added.
pub fn changed_lines(before: &str, after: &str) -> String {
    let diff = TextDiff::from_lines(before, after);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Equal => continue,
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
        };
        out.push(sign);
        out.push(' ');
        out.push_str(change.value().trim_end_matches(['\n', '\r']));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::changed_lines;

    #[test]
    fn a_line_that_stayed_is_left_out() {
        assert_eq!(changed_lines("one\ntwo\n", "one\ntwo\n"), "");
    }

    #[test]
    fn a_changed_line_shows_as_removed_and_added() {
        assert_eq!(
            changed_lines("one\ntwo\n", "one\nthree\n"),
            "- two\n+ three\n"
        );
    }

    #[test]
    fn an_added_line_shows_as_added() {
        assert_eq!(changed_lines("one\n", "one\ntwo\n"), "+ two\n");
    }

    #[test]
    fn a_removed_line_shows_as_removed() {
        assert_eq!(changed_lines("one\ntwo\n", "one\n"), "- two\n");
    }

    #[test]
    fn the_lines_keep_their_order() {
        assert_eq!(
            changed_lines("one\ntwo\nthree\n", "one\nthree\nfour\n"),
            "- two\n+ four\n"
        );
    }

    #[test]
    fn a_text_without_a_trailing_newline_still_diffs() {
        assert_eq!(changed_lines("one", "two"), "- one\n+ two\n");
    }
}
