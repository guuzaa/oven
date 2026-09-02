//! Decode subprocess output as UTF-8, falling back to the Windows ANSI code page.
//!
//! Windows shells commonly write the system ANSI/OEM code page rather than UTF-8.
//! Valid UTF-8 remains preferred; invalid output is decoded through ACP on Windows
//! and lossily as UTF-8 elsewhere.

pub fn decode_command_output(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    #[cfg(windows)]
    if let Some(text) = decode_acp(bytes) {
        return text;
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(windows)]
fn decode_acp(bytes: &[u8]) -> Option<String> {
    const CP_ACP: u32 = 0;

    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            bytes: *const u8,
            nbytes: i32,
            wide: *mut u16,
            nwide: i32,
        ) -> i32;
    }

    let nbytes = i32::try_from(bytes.len()).ok()?;
    // SAFETY: `bytes` points to a valid slice of `nbytes`; a null output pointer
    // with length zero is the documented Windows length-query form.
    let nwide =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), nbytes, std::ptr::null_mut(), 0) };
    if nwide <= 0 {
        return None;
    }
    let mut wide = vec![0u16; nwide as usize];
    // SAFETY: `wide` contains exactly the number of UTF-16 code units returned
    // by the preceding length query, so the Windows call can write at most nwide.
    let written =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), nbytes, wide.as_mut_ptr(), nwide) };
    if written <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&wide[..written as usize]))
}

#[cfg(test)]
mod tests {
    use super::decode_command_output;

    #[test]
    fn empty_bytes_are_empty_string() {
        assert_eq!(decode_command_output(&[]), "");
    }

    #[test]
    fn valid_utf8_is_kept() {
        assert_eq!(decode_command_output("ok 你好".as_bytes()), "ok 你好");
    }

    #[cfg(windows)]
    #[test]
    fn non_utf8_acp_bytes_are_not_replacement_chars() {
        let bytes = [0xC4, 0xE3, 0xBA, 0xC3];
        let text = decode_command_output(&bytes);
        assert!(!text.is_empty(), "{text:?}");
        assert!(!text.contains('\u{FFFD}'), "{text:?}");
    }

    #[cfg(not(windows))]
    #[test]
    fn invalid_utf8_is_lossy_off_windows() {
        let text = decode_command_output(&[0xC4, 0xE3, 0xBA, 0xC3]);
        assert!(text.contains('\u{FFFD}'), "{text:?}");
    }
}
