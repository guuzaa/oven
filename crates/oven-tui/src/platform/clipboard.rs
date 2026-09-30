//! Copying a selection out of the terminal: the system clipboard where there
//! is one, and the OSC52 escape sequence where there is not.

#[cfg(not(test))]
use std::sync::mpsc;
#[cfg(not(test))]
use std::time::Duration;

/// A clipboard that never answers — an unreachable X server, a manager that
/// hangs — must not freeze the UI: the copy runs on its own thread and the
/// caller waits for it only this long before falling back to OSC52.
#[cfg(not(test))]
const COPY_TIMEOUT: Duration = Duration::from_millis(250);

/// Whether the text reached a clipboard.
#[cfg(not(test))]
pub fn copy(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let body = text.to_string();
    let (tx, rx) = mpsc::channel();
    let copied = std::thread::Builder::new()
        .name("oven-clipboard".into())
        .spawn(move || {
            let _ = tx.send(host_copy(&body));
        })
        .is_ok_and(|_| rx.recv_timeout(COPY_TIMEOUT).unwrap_or(false));
    copied || osc52(text)
}

/// Tests must not reach the clipboard of the machine running them: selecting
/// text in a rendered buffer succeeds as far as the transcript is concerned.
#[cfg(test)]
pub fn copy(_text: &str) -> bool {
    true
}

#[cfg(not(test))]
fn host_copy(text: &str) -> bool {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text))
        .is_ok()
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
