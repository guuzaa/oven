//! Names that come from outside and end up in a path.

/// How many characters the short form of an id has: enough to tell two
/// ids apart in a listing.
pub const SHORT_ID_LEN: usize = 8;

/// Whether an id from the outside can be part of a path.
///
/// A session id or a trace id is a name, not a path: it may not hold a
/// separator, and it may not be the directory above the one it is looked
/// up in.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/') && !id.contains('\\') && id != "." && id != ".."
}

/// The short form of an id: the last few characters, the way a session is
/// named in a log line.
pub fn short_session_id(id: &str) -> &str {
    match id.char_indices().nth_back(SHORT_ID_LEN.saturating_sub(1)) {
        Some((i, _)) => &id[i..],
        None => id,
    }
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

    #[test]
    fn short_session_id_takes_last_eight_chars() {
        assert_eq!(
            short_session_id("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678"),
            "3e4f5678"
        );
        assert_eq!(short_session_id("12345678"), "12345678");
        assert_eq!(short_session_id("123456789"), "23456789");
        assert_eq!(short_session_id("abc"), "abc");
        assert_eq!(short_session_id(""), "");
        assert_eq!(short_session_id("x会话abcdef"), "会话abcdef");
    }
}
