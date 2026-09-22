pub(super) struct Collapsible {
    body: String,
    expanded: bool,
    pinned: bool,
}

impl Collapsible {
    pub(super) fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            expanded: true,
            pinned: false,
        }
    }

    pub(super) fn append(&mut self, text: &str) {
        self.body.push_str(text);
    }

    pub(super) fn replace(&mut self, body: impl Into<String>) {
        self.body = body.into();
    }

    #[cfg(test)]
    pub(super) fn body(&self) -> &str {
        &self.body
    }

    pub(super) fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.pinned = self.expanded;
    }

    /// Body window for a row that is still streaming: only the newest `limit`
    /// lines render, so a growing row cannot scroll the view. `None` renders the
    /// full body. Returns the lines dropped from the head and the tail.
    pub(super) fn visible_body(
        &self,
        limit: Option<usize>,
    ) -> (usize, impl Iterator<Item = &str> + '_) {
        let skipped = limit.map_or(0, |max| self.body.lines().count().saturating_sub(max));
        (skipped, self.body.lines().skip(skipped))
    }

    /// Returns whether it was expanded before collapsing.
    pub(super) fn collapse(&mut self) -> bool {
        if self.pinned || !self.expanded {
            return false;
        }
        self.expanded = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Collapsible;

    const LIMIT: usize = 8;

    fn grown(lines: usize) -> Collapsible {
        let mut item = Collapsible::new("l1\n");
        for i in 2..=lines {
            item.append(&format!("l{i}\n"));
        }
        item
    }

    fn visible(item: &Collapsible, limit: Option<usize>) -> (usize, Vec<&str>) {
        let (skipped, lines) = item.visible_body(limit);
        (skipped, lines.collect())
    }

    #[test]
    fn new_starts_expanded() {
        assert!(Collapsible::new("body").is_expanded());
    }

    #[test]
    fn limited_body_keeps_the_tail() {
        let item = grown(LIMIT + 4);
        let (skipped, lines) = visible(&item, Some(LIMIT));
        assert_eq!(skipped, 4);
        assert_eq!(lines, ["l5", "l6", "l7", "l8", "l9", "l10", "l11", "l12"]);
    }

    #[test]
    fn limited_body_keeps_short_bodies_whole() {
        let item = grown(LIMIT - 2);
        assert_eq!(visible(&item, Some(LIMIT)).0, 0);
        assert_eq!(visible(&item, Some(LIMIT)).1.len(), LIMIT - 2);
        let item = grown(LIMIT);
        assert_eq!(visible(&item, Some(LIMIT)).0, 0);
        assert_eq!(visible(&item, Some(LIMIT)).1.len(), LIMIT);
    }

    #[test]
    fn unlimited_body_renders_everything() {
        let item = grown(LIMIT + 2);
        assert_eq!(visible(&item, None).0, 0);
        assert_eq!(visible(&item, None).1.len(), LIMIT + 2);
    }

    #[test]
    fn collapse_closes_unpinned() {
        let mut item = Collapsible::new("a");
        assert!(item.collapse());
        assert!(!item.is_expanded());
        assert!(!item.collapse());
    }

    #[test]
    fn collapse_skips_pinned() {
        let mut pinned = Collapsible::new("keep");
        pinned.toggle();
        pinned.toggle();
        assert!(!pinned.collapse());
        assert!(pinned.is_expanded());
    }
}
