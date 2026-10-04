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

    pub(super) fn collapsed(mut self) -> Self {
        self.expanded = false;
        self
    }

    pub(super) fn append(&mut self, text: &str) {
        match self.sections.last_mut() {
            Some(Section::Text(last)) => last.push_str(text),
            _ => self.sections.push(Section::Text(text.to_string())),
        }
    }

    pub(super) fn open_state(&self) -> OpenState {
        OpenState {
            expanded: self.expanded,
            pinned: self.pinned,
        }
    }

    pub(super) fn restore_open_state(&mut self, state: OpenState) {
        self.expanded = state.expanded;
        self.pinned = state.pinned;
    }

    pub(super) fn with_open_state(mut self, state: OpenState) -> Self {
        self.restore_open_state(state);
        self
    }
}

impl Section {
    pub(super) fn with_open(self, open: OpenState) -> Self {
        match self {
            Self::Item {
                kind,
                title,
                detail,
            } => Self::Item {
                kind,
                title,
                detail: detail.with_open_state(open),
            },
            other => other,
        }
    }
}

impl Collapsible {
    /// Appends `text` to the nested item at `idx` and sets its title.
    pub(super) fn append_item_text(&mut self, idx: usize, title: &str, text: &str) -> bool {
        match self.sections.get_mut(idx) {
            Some(Section::Item {
                title: current,
                detail,
                ..
            }) => {
                *current = title.to_string();
                detail.append(text);
                true
            }
            _ => false,
        }
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
        self.collapse_children();
        true
    }

    /// Closes every nested item. A pinned item stays as the user left it.
    pub(super) fn collapse_children(&mut self) {
        for section in &mut self.sections {
            if let Section::Item { detail, .. } = section {
                detail.collapse_tree();
            }
        }
    }

    /// Closes this block and every unpinned descendant. A pinned block stays
    /// open, children included, until the user closes it.
    pub(super) fn collapse_tree(&mut self) {
        if self.pinned {
            return;
        }
        self.expanded = false;
        self.collapse_children();
    }

    #[cfg(test)]
    pub(super) fn is_fully_collapsed(&self) -> bool {
        !self.expanded
            && self.sections.iter().all(|section| match section {
                Section::Text(_) => true,
                Section::Item { detail, .. } => detail.is_fully_collapsed(),
            })
    }
}

#[derive(Clone, Copy)]
pub(super) struct OpenState {
    pub expanded: bool,
    pub pinned: bool,
}

impl OpenState {
    pub(super) fn expanded() -> Self {
        Self {
            expanded: true,
            pinned: false,
        }
    }

    pub(super) fn collapsed() -> Self {
        Self {
            expanded: false,
            pinned: false,
        }
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
