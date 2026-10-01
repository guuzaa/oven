use ratatui::Frame;

use crate::core::component::Component;
use crate::core::hint::{self, Prompt};
use crate::core::layout;
use crate::widgets::input::InputView;
use crate::widgets::queue;

use super::Ui;
use super::prompt::OverlayPrompt;

impl Ui {
    pub(super) fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        let overlay_height = match self.prompt.as_ref() {
            Some(prompt) => prompt.height(area.width),
            None => self.input.overlay_height(),
        };
        let input_h = match self.views.viewer_rows() {
            Some(rows) => rows,
            None => self.input.height(area.width),
        };
        let regions = layout::split(
            area,
            input_h,
            queue::height(self.pending.len()),
            self.views.height(),
            self.todos.height(),
            overlay_height,
        );
        self.agents_area = regions.agents;

        match self.views.focused() {
            Some(view) => view.draw(f, regions.transcript, &self.state),
            None => self.transcript.draw(f, regions.transcript, &self.state),
        }
        if let Some(queue) = regions.queue {
            queue::draw(f, queue, &self.pending[0].text, self.pending.len() - 1);
        }
        if let Some(agents) = regions.agents {
            self.views.draw_strip(f, agents);
        }
        if let Some(todos) = regions.todos {
            self.todos.draw(f, todos);
        }
        if self.views.focused_id().is_some() {
            self.views.draw_hint(f, regions.input);
            self.status.draw_bar(f, regions.status, &self.state);
            return;
        }
        self.input.draw_composer(
            f,
            regions.input,
            &self.state,
            composer_hint(
                &self.input,
                self.state.busy,
                self.prompt.as_ref(),
                self.esc_armed(),
            ),
        );
        if let Some(overlay) = regions.overlay {
            match self.prompt.as_ref() {
                Some(prompt) => prompt.draw(f, overlay),
                None => self.input.draw_overlay(f, overlay),
            }
        }
        self.status.draw_bar(f, regions.status, &self.state);
        self.status.draw_reply_overlay(f, regions.transcript);
    }
}

/// The keys that apply right now, drawn on the composer border: whichever box
/// owns the keyboard states them, and the composer falls back to its own.
pub(super) fn composer_hint(
    input: &InputView,
    busy: bool,
    prompt: Option<&OverlayPrompt>,
    esc_armed: bool,
) -> Option<&'static str> {
    let prompt = prompt.map(|prompt| {
        if prompt.awaits_typed_answer() {
            Prompt::Answer
        } else {
            Prompt::Keys(prompt.hint())
        }
    });
    hint::composer(input.overlay_hint(), prompt, busy, esc_armed)
}
