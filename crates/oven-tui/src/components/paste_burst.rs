//! Windows console input has no bracketed paste (crossterm#737): a paste
//! arrives as a burst of individual key events, one Enter per newline, so a
//! multi-line paste would submit once per line. When a key event has more
//! events already queued behind it, coalesce the run of printable keys back
//! into a single paste.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Grace period for straggler events once a burst is underway, so a paste
/// split across console writes still coalesces into one event.
const BURST_CONTINUE_TIMEOUT: Duration = Duration::from_millis(3);

pub(crate) enum Burst {
    Key(KeyEvent),
    Paste(String),
}

/// Coalesces a possible paste burst starting at `first`. Returns the burst
/// plus a trailing event when draining consumed one that is not part of it.
pub(crate) fn coalesce(first: KeyEvent) -> io::Result<(Burst, Option<Event>)> {
    if cfg!(windows) {
        coalesce_from(first, &mut TerminalSource)
    } else {
        Ok((Burst::Key(first), None))
    }
}

trait EventSource {
    /// Returns the next event if one arrives within `timeout`.
    fn next_ready(&mut self, timeout: Duration) -> io::Result<Option<Event>>;
}

struct TerminalSource;

impl EventSource for TerminalSource {
    fn next_ready(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
        if event::poll(timeout)? {
            event::read().map(Some)
        } else {
            Ok(None)
        }
    }
}

fn coalesce_from(
    first: KeyEvent,
    source: &mut impl EventSource,
) -> io::Result<(Burst, Option<Event>)> {
    let Some(ch) = burst_char(&first) else {
        return Ok((Burst::Key(first), None));
    };
    let mut text = String::from(ch);
    let mut trailing = None;
    let mut timeout = Duration::ZERO;
    while let Some(ev) = source.next_ready(timeout)? {
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => match burst_char(&key) {
                Some(ch) => {
                    text.push(ch);
                    timeout = BURST_CONTINUE_TIMEOUT;
                }
                None => {
                    trailing = Some(Event::Key(key));
                    break;
                }
            },
            Event::Key(_) => {}
            other => {
                trailing = Some(other);
                break;
            }
        }
    }
    if text.chars().count() > 1 {
        Ok((Burst::Paste(text), trailing))
    } else {
        Ok((Burst::Key(first), trailing))
    }
}

fn burst_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            Some(ch)
        }
        KeyCode::Enter if key.modifiers.is_empty() => Some('\n'),
        KeyCode::Tab if key.modifiers.is_empty() => Some('\t'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crossterm::event::MouseEvent;

    use super::*;

    struct FakeSource(VecDeque<Event>);

    impl EventSource for FakeSource {
        fn next_ready(&mut self, _timeout: Duration) -> io::Result<Option<Event>> {
            Ok(self.0.pop_front())
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn release(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Release)
    }

    fn mouse_event() -> Event {
        Event::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn run(first: KeyEvent, queued: Vec<Event>) -> (Burst, Option<Event>) {
        coalesce_from(first, &mut FakeSource(queued.into())).unwrap()
    }

    #[test]
    fn lone_key_stays_a_key() {
        let (burst, trailing) = run(press(KeyCode::Enter), vec![]);
        assert!(matches!(burst, Burst::Key(k) if k.code == KeyCode::Enter));
        assert!(trailing.is_none());
    }

    #[test]
    fn non_printable_first_key_is_untouched() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut source = FakeSource(VecDeque::from([Event::Key(press(KeyCode::Char('x')))]));
        let (burst, trailing) = coalesce_from(ctrl_c, &mut source).unwrap();
        assert!(matches!(burst, Burst::Key(k) if k.modifiers == KeyModifiers::CONTROL));
        assert!(trailing.is_none());
        assert_eq!(source.0.len(), 1, "queued events must not be drained");
    }

    #[test]
    fn multiline_burst_becomes_single_paste() {
        let queued = vec![
            Event::Key(press(KeyCode::Char('i'))),
            Event::Key(press(KeyCode::Enter)),
            Event::Key(press(KeyCode::Char('h'))),
            Event::Key(press(KeyCode::Char('i'))),
        ];
        let (burst, trailing) = run(press(KeyCode::Char('h')), queued);
        assert!(matches!(burst, Burst::Paste(text) if text == "hi\nhi"));
        assert!(trailing.is_none());
    }

    #[test]
    fn release_events_are_skipped_inside_burst() {
        let queued = vec![
            Event::Key(release(KeyCode::Char('a'))),
            Event::Key(press(KeyCode::Char('b'))),
        ];
        let (burst, _) = run(press(KeyCode::Char('a')), queued);
        assert!(matches!(burst, Burst::Paste(text) if text == "ab"));
    }

    #[test]
    fn tab_and_enter_map_to_whitespace() {
        let queued = vec![
            Event::Key(press(KeyCode::Tab)),
            Event::Key(press(KeyCode::Enter)),
            Event::Key(press(KeyCode::Char('x'))),
        ];
        let (burst, _) = run(press(KeyCode::Char('a')), queued);
        assert!(matches!(burst, Burst::Paste(text) if text == "a\t\nx"));
    }

    #[test]
    fn foreign_event_ends_burst_and_is_returned() {
        let queued = vec![Event::Key(press(KeyCode::Char('b'))), mouse_event()];
        let (burst, trailing) = run(press(KeyCode::Char('a')), queued);
        assert!(matches!(burst, Burst::Paste(text) if text == "ab"));
        assert!(matches!(trailing, Some(Event::Mouse(_))));
    }

    #[test]
    fn immediate_foreign_event_keeps_key_and_returns_it() {
        let (burst, trailing) = run(press(KeyCode::Enter), vec![mouse_event()]);
        assert!(matches!(burst, Burst::Key(k) if k.code == KeyCode::Enter));
        assert!(matches!(trailing, Some(Event::Mouse(_))));
    }

    #[test]
    fn modified_key_ends_burst_and_is_returned() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let queued = vec![Event::Key(press(KeyCode::Enter)), Event::Key(ctrl_c)];
        let (burst, trailing) = run(press(KeyCode::Char('a')), queued);
        assert!(matches!(burst, Burst::Paste(text) if text == "a\n"));
        assert!(matches!(trailing, Some(Event::Key(k)) if k.modifiers == KeyModifiers::CONTROL));
    }
}
