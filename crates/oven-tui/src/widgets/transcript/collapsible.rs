use super::kinds::LineKind;

/// One piece of a collapsible body: plain lines, or a nested collapsible that
/// carries its own title and marker.
pub(super) enum Section {
    Text(String),
    Item {
        kind: LineKind,
        title: String,
        detail: Collapsible,
    },
}

pub(super) struct Collapsible {
    sections: Vec<Section>,
    expanded: bool,
    pinned: bool,
}

impl Collapsible {
    pub(super) fn new(body: impl Into<String>) -> Self {
        Self::from_sections(vec![Section::Text(body.into())])
    }

    pub(super) fn from_sections(sections: Vec<Section>) -> Self {
        Self {
            sections,
            expanded: true,
            pinned: false,
        }
    }

    pub(super) fn append(&mut self, text: &str) {
        match self.sections.last_mut() {
            Some(Section::Text(last)) => last.push_str(text),
            _ => self.sections.push(Section::Text(text.to_string())),
        }
    }

    pub(super) fn replace_sections(&mut self, sections: Vec<Section>) {
        self.sections = sections;
    }

    /// The nested collapsible at `idx`, for a header that points into one.
    pub(super) fn item_mut(&mut self, idx: usize) -> Option<&mut Collapsible> {
        match self.sections.get_mut(idx)? {
            Section::Item { detail, .. } => Some(detail),
            Section::Text(_) => None,
        }
    }

    #[cfg(test)]
    pub(super) fn body(&self) -> String {
        self.sections
            .iter()
            .filter_map(|section| match section {
                Section::Text(text) => Some(text.as_str()),
                Section::Item { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.pinned = self.expanded;
    }

    /// Every nested item and text block, in order; a nested path indexes this.
    pub(super) fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Returns whether it was expanded before collapsing.
    pub(super) fn collapse(&mut self) -> bool {
        if self.pinned || !self.expanded {
            return false;
        }
        self.expanded = false;
        for section in &mut self.sections {
            if let Section::Item { detail, .. } = section {
                detail.collapse();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Collapsible;

    #[test]
    fn new_starts_expanded() {
        let item = Collapsible::new("body");
        assert!(item.is_expanded());
        assert_eq!(item.body(), "body");
    }

    #[test]
    fn appended_text_extends_the_last_section() {
        let mut item = Collapsible::new("one");
        item.append(" two");
        assert_eq!(item.body(), "one two");
        assert_eq!(item.sections().len(), 1);
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
