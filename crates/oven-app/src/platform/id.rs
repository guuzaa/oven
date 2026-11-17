//! Names that come from outside and end up in a path.

/// Whether an id from the outside can be part of a path.
///
/// A session id or a trace id is a name, not a path: it may not hold a
/// separator, and it may not be the directory above the one it is looked
/// up in.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/') && !id.contains('\\') && id != "." && id != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_not_a_path() {
        assert!(is_safe_id("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678"));
        assert!(is_safe_id("a3f9c2b1d4e7"));
    }

    #[test]
    fn a_path_is_not_a_name() {
        assert!(!is_safe_id(""));
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("."));
        assert!(!is_safe_id("../../etc/passwd"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id("a\\b"));
    }
}
