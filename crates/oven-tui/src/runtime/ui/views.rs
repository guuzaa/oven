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
    main: AgentId,
    registry: Vec<NodeInfo>,
    transcripts: BTreeMap<AgentId, Transcript>,
    focus: Option<AgentId>,
    /// The strip row ↑↓ last landed on. The driver until a subagent is
    /// chosen; Enter opens a subagent. Cleared when the strip goes away.
    picked: Option<AgentId>,
    /// A subagent was opened or highlighted, so a finished strip stays until
    /// Esc or the next message.
    hold: bool,
}

impl Views {
    pub(super) fn new(main: AgentId) -> Self {
        Self {
            main,
            registry: Vec::new(),
            transcripts: BTreeMap::new(),
            focus: None,
            picked: None,
            hold: false,
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
        if self
            .picked
            .is_some_and(|id| id != self.main && !self.registry.iter().any(|agent| agent.id == id))
        {
            self.picked = None;
        }
        if self.height() == 0 {
            self.picked = None;
            self.hold = false;
        } else if self.picked.is_none() {
            self.picked = Some(self.main);
        }
        active
    }

    /// The strip stays up after every subagent has settled when one was opened
    /// or highlighted, so Esc can come back to it. Opening and highlighting
    /// set `hold`; the driver's own row does not.
    fn show_settled(&self) -> bool {
        self.hold
    }

    /// True when the strip is up only because a transcript can still be reopened.
    fn held_settled(&self) -> bool {
        self.show_settled() && !self.registry.iter().any(|agent| agent.status.is_active())
    }

    #[cfg(test)]
    pub(super) fn picked_id(&self) -> Option<AgentId> {
        self.picked
    }

    /// What the composer border should say about the strip. `navigable` is
    /// false when the composer or a popup already owns ↑↓ and Enter.
    pub(super) fn strip_band(&self, navigable: bool) -> hint::Strip {
        if !navigable {
            return hint::Strip::Off;
        }
        if self.picked.is_none_or(|id| id == self.main) {
            return hint::Strip::Select;
        }
        hint::Strip::Open {
            settled: self.held_settled(),
        }
    }

    /// Shows the next agent's transcript. The driver row leaves the viewer.
    /// No-op once the strip is gone.
    pub(super) fn switch(&mut self, up: bool) {
        let next = agents::cycle(
            self.main,
            &self.registry,
            self.picked.or(self.focus),
            up,
            self.show_settled(),
        );
        match next {
            Some(id) if id != self.main => self.focus(id),
            Some(_) => self.close(),
            None => {}
        }
    }

    /// Moves the strip highlight. No-op once the strip is gone.
    pub(super) fn nudge(&mut self, up: bool) {
        self.picked = agents::cycle(
            self.main,
            &self.registry,
            self.picked,
            up,
            self.show_settled(),
        );
        if self.picked.is_some_and(|id| id != self.main) {
            self.hold = true;
        }
    }

    /// Opens the highlighted subagent. False when the driver is highlighted,
    /// so Enter still belongs to the composer.
    pub(super) fn confirm(&mut self) -> bool {
        let show_settled = self.show_settled();
        let Some(id) = self.picked.filter(|id| {
            *id != self.main && agents::on_strip(self.main, &self.registry, *id, show_settled)
        }) else {
            return false;
        };
        self.focus(id);
        true
    }

    pub(super) fn main_id(&self) -> AgentId {
        self.main
    }

    /// Drops the highlight that was keeping a finished strip on screen.
    /// False while a subagent is still running.
    pub(super) fn dismiss_settled(&mut self) -> bool {
        if !self.hold || self.registry.iter().any(|agent| agent.status.is_active()) {
            return false;
        }
        self.hold = false;
        self.picked = None;
        true
    }

    /// The next message is a new task: a finished strip no longer has to stay.
    pub(super) fn release(&mut self) {
        self.hold = false;
        self.picked = None;
        if self.height() > 0 {
            self.picked = Some(self.main);
        }
    }

    /// Opens a subagent's transcript, building it on first view.
    ///
    /// A subagent that has not said anything yet — one waiting for a slot, or
    /// one just spawned — has no transcript of its own, and opening nothing
    /// is worse than opening a page that says what it was asked to do. The
    /// task's label is that page: its events fill in under it as they arrive.
    pub(super) fn focus(&mut self, id: AgentId) {
        if id == self.main {
            self.close();
            return;
        }
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
        self.hold = true;
        if self.registry.iter().any(|agent| agent.id == id) {
            self.picked = Some(id);
        }
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
        if self.height() > 0 {
            self.picked = Some(self.main);
        } else {
            self.picked = None;
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    pub(super) fn transcript(&mut self, id: AgentId) -> &mut Transcript {
        self.transcripts.entry(id).or_insert_with(Transcript::new)
    }

    pub(super) fn height(&self) -> u16 {
        agents::height(&self.registry, self.show_settled())
    }

    pub(super) fn row_at(&self, area: Rect, row: u16) -> Option<AgentId> {
        agents::row_at(area, self.main, &self.registry, row, self.show_settled())
    }

    pub(super) fn draw_strip(&self, f: &mut Frame<'_>, area: Rect, busy: bool) {
        agents::draw(
            f,
            area,
            self.main,
            busy,
            &self.registry,
            self.show_settled(),
            self.picked,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::agents::running;
    use oven_app::AgentId;

    #[test]
    fn leaving_the_viewer_selects_the_main_agent() {
        let main = AgentId::next();
        let alpha = running("alpha");
        let mut views = Views::new(main);
        views.mirror(std::slice::from_ref(&alpha));
        assert_eq!(views.picked_id(), Some(main), "the driver starts selected");

        views.focus(alpha.id);
        assert_eq!(views.picked_id(), Some(alpha.id));

        views.close();
        assert_eq!(
            views.picked_id(),
            Some(main),
            "leaving the viewer shows the driver as the agent on screen"
        );
        assert!(views.focused_id().is_none());
        assert!(!views.confirm());
    }

    #[test]
    fn focusing_the_main_agent_leaves_the_viewer() {
        let main = AgentId::next();
        let alpha = running("alpha");
        let mut views = Views::new(main);
        views.mirror(std::slice::from_ref(&alpha));
        views.focus(alpha.id);

        views.focus(main);

        assert!(views.focused_id().is_none());
        assert_eq!(views.picked_id(), Some(main));
    }

    #[test]
    fn mirror_returns_the_highlight_to_main_when_a_subagent_leaves() {
        let main = AgentId::next();
        let alpha = running("alpha");
        let beta = running("beta");
        let mut views = Views::new(main);
        views.mirror(&[alpha.clone(), beta.clone()]);
        views.nudge(false);
        assert_eq!(views.picked_id(), Some(alpha.id));

        views.mirror(std::slice::from_ref(&beta));
        assert_eq!(views.picked_id(), Some(main));
        assert!(!views.confirm());
    }

    #[test]
    fn arrows_switch_an_open_transcript_back_to_the_driver() {
        let main = AgentId::next();
        let alpha = running("alpha");
        let beta = running("beta");
        let mut views = Views::new(main);
        views.mirror(&[alpha.clone(), beta.clone()]);
        views.focus(beta.id);

        views.switch(true);
        assert_eq!(views.focused_id(), Some(alpha.id));
        assert_eq!(views.picked_id(), Some(alpha.id));

        views.switch(true);
        assert!(views.focused_id().is_none());
        assert_eq!(views.picked_id(), Some(main));

        views.switch(false);
        assert_eq!(views.focused_id(), Some(alpha.id));
    }
}
