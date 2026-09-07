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

    #[test]
    fn new_starts_expanded() {
        assert!(Collapsible::new("body").is_expanded());
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
