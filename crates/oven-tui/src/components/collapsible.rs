pub(super) struct Collapsible {
    body: String,
    expanded: bool,
}

impl Collapsible {
    pub(super) fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            expanded: false,
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
    }
}
