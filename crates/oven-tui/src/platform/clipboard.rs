//! Copying a selection out of the terminal: the system clipboard where there
//! is one, and the OSC52 escape sequence where there is not.

/// Whether the text reached a clipboard.
#[cfg(not(test))]
pub fn copy(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text))
        .is_ok()
        || osc52(text)
}

/// Tests must not reach the clipboard of the machine running them: selecting
/// text in a rendered buffer succeeds as far as the transcript is concerned.
#[cfg(test)]
pub fn copy(_text: &str) -> bool {
    true
}

#[cfg(not(test))]
fn osc52(text: &str) -> bool {
    use std::io::{self, Write};

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let encoded = STANDARD.encode(text.as_bytes());
    write!(io::stdout(), "\x1b]52;c;{encoded}\x07")
        .and_then(|()| io::stdout().flush())
        .is_ok()
}
