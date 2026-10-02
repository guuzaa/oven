const FENCE: &str = "---";
const BOM: char = '\u{feff}';

/// Split a markdown document into its frontmatter and body. Both fences must
/// sit on a line of their own, so a `---` inside a value or the body is text.
pub fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw
        .trim_start_matches(BOM)
        .trim_start()
        .strip_prefix(FENCE)?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == FENCE {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::split_frontmatter;

    #[test]
    fn splits_front_and_body() {
        assert_eq!(
            split_frontmatter("---\ndescription: x\n---\nbody\n"),
            Some(("description: x\n", "body\n"))
        );
    }

    #[test]
    fn inline_dashes_are_not_a_fence() {
        assert_eq!(
            split_frontmatter("---\ndescription: a --- b\n---\nc --- d\n---\n"),
            Some(("description: a --- b\n", "c --- d\n---\n"))
        );
    }

    #[test]
    fn handles_crlf_and_bom() {
        assert_eq!(
            split_frontmatter("\u{feff}---\r\nkind: fact\r\n---\r\nbody"),
            Some(("kind: fact\r\n", "body"))
        );
    }

    #[test]
    fn closing_fence_at_end_of_file_leaves_empty_body() {
        assert_eq!(split_frontmatter("---\nk: v\n---"), Some(("k: v\n", "")));
    }

    #[test]
    fn empty_frontmatter() {
        assert_eq!(split_frontmatter("---\n---\nbody"), Some(("", "body")));
    }

    #[test]
    fn rejects_missing_fences() {
        assert_eq!(split_frontmatter("no frontmatter"), None);
        assert_eq!(split_frontmatter("---\nk: v\nno close\n"), None);
        assert_eq!(split_frontmatter("----\nk: v\n---\n"), None);
        assert_eq!(split_frontmatter("--- k: v\n---\n"), None);
    }
}
