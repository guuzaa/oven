use std::fmt::Write;

const SEPARATOR: char = '/';
const DASH: char = '-';
const EMPTY_KEY: &str = "-";

/// One path segment naming a workspace root under `~/.oven/projects`.
///
/// The encoding is reversible, so two roots never share a key:
/// `-` doubles, `/` becomes a single `-`, and every other byte that is not
/// `[A-Za-z0-9_]` (including `.` and `\`) becomes `%XX`. The key therefore
/// never contains `.`: it cannot be `.` or `..`, and it never ends in a dot.
/// `/home/me/oven` reads as `home-me-oven`.
pub fn project_key(root: &str) -> String {
    let root = root.strip_prefix(SEPARATOR).unwrap_or(root);
    let mut key = String::with_capacity(root.len());
    for c in root.chars() {
        match c {
            DASH => key.push_str("--"),
            SEPARATOR => key.push(DASH),
            c if c.is_ascii_alphanumeric() || c == '_' => key.push(c),
            c => push_escaped(&mut key, c),
        }
    }
    if key.is_empty() {
        key.push_str(EMPTY_KEY);
    }
    key
}

fn push_escaped(key: &mut String, c: char) {
    let mut bytes = [0; 4];
    for byte in c.encode_utf8(&mut bytes).bytes() {
        let _ = write!(key, "%{byte:02X}");
    }
}

#[cfg(test)]
mod tests {
    use super::project_key;

    #[test]
    fn a_root_reads_as_its_path() {
        assert_eq!(project_key("/home/me/code/oven"), "home-me-code-oven");
        assert_eq!(project_key("/home/me/v1_2"), "home-me-v1_2");
    }

    #[test]
    fn dashes_and_separators_never_collide() {
        assert_eq!(project_key("/a-b"), "a--b");
        assert_eq!(project_key("/a/b"), "a-b");
        assert_ne!(project_key("/a-b"), project_key("/a/b"));
        assert_ne!(project_key("/a/b"), project_key("/a\\b"));
    }

    #[test]
    fn dots_are_escaped_so_no_key_is_a_traversal() {
        assert_eq!(project_key("/a/v1.2"), "a-v1%2E2");
        assert_eq!(project_key("/.."), "%2E%2E");
        assert_eq!(project_key("/a/."), "a-%2E");
        assert!(!project_key("/a/b.").ends_with('.'));
        assert_ne!(project_key("/a.b"), project_key("/a%2Eb"));
    }

    #[test]
    fn other_bytes_are_escaped() {
        assert_eq!(project_key("/w/代码"), "w-%E4%BB%A3%E7%A0%81");
        assert_eq!(project_key("C:\\w x"), "C%3A%5Cw%20x");
        assert_eq!(project_key("/w/%"), "w-%25");
    }

    #[test]
    fn the_filesystem_root_is_still_a_segment() {
        assert_eq!(project_key("/"), "-");
        assert_eq!(project_key("/-x"), "--x");
        assert_ne!(project_key("/-x"), project_key("/x"));
    }
}
