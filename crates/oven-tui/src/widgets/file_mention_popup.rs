use crossterm::event::{KeyCode, KeyEvent};
use oven_app::FileMentions;
use ratatui::Frame;
use ratatui::layout::Rect;

use super::list::{self, MAX_LIST_ROWS};

const DIR_SUFFIX: char = '/';
const EMPTY_HINT: &str = "no matching file";

pub(crate) enum FileMentionPopupAction {
    Handled,
    Fill {
        before: String,
        insert: String,
        after: String,
    },
}

struct MentionToken {
    start: usize,
    end: usize,
    query: String,
}

pub(crate) struct FileMentionPopup {
    text: String,
    matches: Vec<String>,
    selected: usize,
    token: Option<MentionToken>,
}

impl FileMentionPopup {
    pub(crate) const HINT: &str = "tab insert · enter fill · esc close";

    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
            matches: Vec::new(),
            selected: 0,
            token: None,
        }
    }

    #[inline]
    pub(crate) fn is_open(&self) -> bool {
        self.token.is_some()
    }

    pub(crate) fn close(&mut self) {
        self.selected = 0;
        self.matches.clear();
        self.token = None;
    }

    #[cfg(test)]
    pub(crate) fn matches(&self) -> &[String] {
        &self.matches
    }

    pub(crate) fn has_token(text: &str, cursor: usize) -> bool {
        mention_bounds(text, cursor).is_some()
    }

    pub(crate) fn refresh(
        &mut self,
        text: &str,
        cursor: usize,
        mentions: &mut FileMentions,
        rescan: bool,
    ) {
        let was_open = self.token.is_some();
        self.text = text.to_string();
        self.token = mention_token(text, cursor);
        let Some(token) = self.token.as_ref() else {
            self.selected = 0;
            self.matches.clear();
            return;
        };
        if !was_open && rescan {
            mentions.rescan();
            self.selected = 0;
        }
        self.matches = mentions.search(&token.query);
        self.matches.truncate(MAX_LIST_ROWS);
        list::clamp_selected(&mut self.selected, self.matches.len());
    }

    pub(crate) fn height(&self) -> u16 {
        if !self.is_open() {
            return 0;
        }
        list::bounded_rows(self.matches.len())
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        if !self.is_open() {
            return;
        }
        list::draw_choice_list(
            f,
            area,
            EMPTY_HINT,
            self.matches
                .iter()
                .map(|path| (format!("@{path}"), String::new())),
            self.selected,
        );
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<FileMentionPopupAction> {
        match key.code {
            KeyCode::Esc => {
                self.close();
                Some(FileMentionPopupAction::Handled)
            }
            KeyCode::Tab => {
                if let Some(fill) = self.fill_selected() {
                    Some(fill)
                } else {
                    Some(FileMentionPopupAction::Handled)
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let path = self.matches.get(self.selected)?;
                let query = &self.token.as_ref()?.query;
                if query.trim_end_matches(DIR_SUFFIX) == path.trim_end_matches(DIR_SUFFIX) {
                    None
                } else {
                    self.fill_selected()
                }
            }
            _ if list::cycle_key(key, &mut self.selected, self.matches.len()) => {
                Some(FileMentionPopupAction::Handled)
            }
            _ => None,
        }
    }

    fn fill_selected(&self) -> Option<FileMentionPopupAction> {
        let path = self.matches.get(self.selected)?;
        let token = self.token.as_ref()?;
        if token.end > self.text.len() || token.start > token.end {
            return None;
        }
        let before = self.text[..token.start].to_string();
        let after = self.text[token.end..].to_string();
        let spaced = !path.ends_with(DIR_SUFFIX) && !after.starts_with(|c: char| c.is_whitespace());
        let insert = if spaced {
            format!("@{path} ")
        } else {
            format!("@{path}")
        };
        Some(FileMentionPopupAction::Fill {
            before,
            insert,
            after,
        })
    }
}

fn mention_token(text: &str, cursor: usize) -> Option<MentionToken> {
    let (start, end) = mention_bounds(text, cursor)?;
    Some(MentionToken {
        start,
        end,
        query: text[start + 1..end].to_string(),
    })
}

fn mention_bounds(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    let start = before.rfind('@')?;
    if start > 0 {
        let prev = before[..start].chars().next_back()?;
        if !prev.is_whitespace() {
            return None;
        }
    }
    let after_at = &text[start + 1..];
    let query_len = after_at.find(char::is_whitespace).unwrap_or(after_at.len());
    let end = start + 1 + query_len;
    (cursor <= end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_at_start() {
        let t = mention_token("@lib", 4).unwrap();
        assert_eq!(t.query, "lib");
        assert_eq!(t.start, 0);
        assert_eq!(t.end, 4);
    }

    #[test]
    fn token_mid_line() {
        let t = mention_token("see @src/li", 11).unwrap();
        assert_eq!(t.query, "src/li");
        assert_eq!(t.start, 4);
    }

    #[test]
    fn token_after_japanese() {
        let text = "見て @src";
        let t = mention_token(text, text.len()).unwrap();
        assert_eq!(t.query, "src");
        assert_eq!(&text[..t.start], "見て ");
    }

    #[test]
    fn token_after_spanish() {
        let text = "¿dónde está @src";
        let t = mention_token(text, text.len()).unwrap();
        assert_eq!(t.query, "src");
        assert_eq!(&text[..t.start], "¿dónde está ");
    }

    #[test]
    fn token_at_sign_only() {
        let t = mention_token("@", 1).unwrap();
        assert_eq!(t.query, "");
        assert_eq!(t.start, 0);
        assert_eq!(t.end, 1);
    }

    #[test]
    fn email_is_not_a_mention() {
        assert!(mention_token("foo@bar", 7).is_none());
    }

    #[test]
    fn cursor_after_token_is_closed() {
        assert!(mention_token("see @li please", 14).is_none());
    }

    #[test]
    fn cursor_on_query_stays_open() {
        let t = mention_token("see @li please", 7).unwrap();
        assert_eq!(t.query, "li");
    }
}
