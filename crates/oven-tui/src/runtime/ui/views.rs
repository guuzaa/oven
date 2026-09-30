use std::collections::BTreeMap;

use oven_app::{AgentId, NodeInfo};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::core::hint;
use crate::widgets::agents;
use crate::widgets::transcript::Transcript;

/// The viewer replaces the composer with a single hint row.
const VIEWER_ROWS: u16 = 1;

/// Every subagent the app reports, the transcript each one builds from its own
/// events, and the one whose transcript has taken over the screen.
///
/// The registry, the strip and the viewer are three faces of one thing, so
/// they live together: a subagent nobody can reach any more has no view left
/// to open.
pub(super) struct Views {
    registry: Vec<NodeInfo>,
    transcripts: BTreeMap<AgentId, Transcript>,
    focus: Option<AgentId>,
}

impl Views {
    pub(super) fn new() -> Self {
        Self {
            registry: Vec::new(),
            transcripts: BTreeMap::new(),
            focus: None,
        }
    }

    /// Mirrors the app's registry, and returns how many subagents in it are
    /// still working — the count that keeps the frame ticking.
    pub(super) fn mirror(&mut self, registry: &[NodeInfo]) -> usize {
        self.registry.clear();
        self.registry.extend_from_slice(registry);
        let active = self
            .registry
            .iter()
            .filter(|agent| agent.status.is_active())
            .count();
        self.transcripts
            .retain(|id, _| self.registry.iter().any(|agent| agent.id == *id));
        if self
            .focus
            .is_some_and(|id| !self.transcripts.contains_key(&id))
        {
            self.focus = None;
        }
        active
    }

    /// Opens a subagent's transcript, building it on first view.
    ///
    /// A subagent that has not said anything yet — one waiting for a slot, or
    /// one just spawned — has no transcript of its own, and opening nothing
    /// is worse than opening a page that says what it was asked to do. The
    /// task's label is that page: its events fill in under it as they arrive.
    pub(super) fn focus(&mut self, id: AgentId) {
        if !self.transcripts.contains_key(&id) {
            let label = self
                .registry
                .iter()
                .find(|agent| agent.id == id)
                .map(|agent| agent.label.as_str())
                .filter(|label| !label.is_empty())
                .map(str::to_owned);
            let mut view = Transcript::new();
            if let Some(label) = label {
                view.start_user_turn(&label);
            }
            self.transcripts.insert(id, view);
        }
        self.focus = Some(id);
    }

    /// The subagent whose transcript is on screen, if any.
    pub(super) fn focused_id(&self) -> Option<AgentId> {
        self.focus
    }

    pub(super) fn focused(&mut self) -> Option<&mut Transcript> {
        self.focus.and_then(|id| self.transcripts.get_mut(&id))
    }

    pub(super) fn close(&mut self) {
        self.focus = None;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    pub(super) fn transcript(&mut self, id: AgentId) -> &mut Transcript {
        self.transcripts.entry(id).or_insert_with(Transcript::new)
    }

    pub(super) fn height(&self) -> u16 {
        agents::height(&self.registry)
    }

    pub(super) fn row_at(&self, area: Rect, row: u16) -> Option<AgentId> {
        agents::row_at(area, &self.registry, row)
    }

    pub(super) fn draw_strip(&self, f: &mut Frame<'_>, area: Rect) {
        agents::draw(f, area, &self.registry);
    }

    /// While a subagent's transcript owns the screen the composer has no
    /// work to do, so its row states what the viewer answers to instead.
    pub(super) fn draw_hint(&self, f: &mut Frame<'_>, area: Rect) {
        let Some(agent) = self
            .focus
            .and_then(|id| self.registry.iter().find(|agent| agent.id == id))
        else {
            return;
        };
        let text = format!(
            "{} · {} · {}",
            agent.name,
            agent.status.label(),
            hint::VIEWER
        );
        agents::draw_hint(f, area, &text);
    }

    /// Rows the viewer takes from the composer, leaving one row for the hint.
    pub(super) fn viewer_rows(&self) -> Option<u16> {
        self.focus.map(|_| VIEWER_ROWS)
    }
}
