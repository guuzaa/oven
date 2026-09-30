use oven_app::{AgentEvent, AppEvent, AppEventKind, Input, SubagentEvent, ToolEvent, TurnEvent};

use crate::core::component::Component;
use crate::widgets::choice_popup::ChoicePopup;
use crate::widgets::input::display_user_input;
use crate::widgets::question_prompt::QuestionPrompt;
use crate::widgets::transcript::Transcript;

use super::Ui;
use super::prompt::OverlayPrompt;

impl Ui {
    /// The backend went away mid-turn: close the response so an unfinished
    /// answer is settled before the next prompt archives the pinned one.
    pub(super) fn disconnected(&mut self) {
        self.transcript.finish_response();
        self.state.busy = false;
    }

    pub(super) fn apply_event(&mut self, ev: &AppEvent) {
        match &ev.kind {
            AppEventKind::Exited => self.quit = true,
            AppEventKind::Subagent(SubagentEvent::Focus { id }) => self.focus_agent(*id),
            // A subagent's turn is not the driver's: its events feed its own
            // transcript and nothing else. Only the notification that it
            // finished reaches the composer and the status bar.
            AppEventKind::Agent(env) if env.agent_id != self.main_agent => {
                self.views
                    .entry(env.agent_id)
                    .or_insert_with(Transcript::new)
                    .on_event(ev);
                return;
            }
            AppEventKind::Agent(env) => match &env.event {
                AgentEvent::Turn(TurnEvent::LoopLimitReached {
                    request_id,
                    max_iters,
                }) => {
                    self.prompt = Some(OverlayPrompt::LoopLimit {
                        request_id: *request_id,
                        popup: ChoicePopup::loop_limit(*max_iters),
                    });
                }
                AgentEvent::Tool(ToolEvent::ApprovalRequested {
                    request_id,
                    name,
                    view,
                    ..
                }) => {
                    self.prompt = Some(OverlayPrompt::Approval {
                        request_id: *request_id,
                        popup: ChoicePopup::approval(name, &view.summary),
                    });
                }
                AgentEvent::Tool(ToolEvent::QuestionAsked {
                    request_id,
                    question,
                }) => {
                    self.prompt = Some(OverlayPrompt::Question {
                        request_id: *request_id,
                        popup: QuestionPrompt::new(
                            question.question.clone(),
                            question.options.clone(),
                        ),
                    });
                }
                _ => {}
            },
            AppEventKind::RequestResolved { request_id } => {
                if self
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.request_id() == *request_id)
                {
                    self.prompt = None;
                }
            }
            AppEventKind::HistoryChanged { .. } => self.reload_history(),
            AppEventKind::Shell(_)
            | AppEventKind::Compaction(_)
            | AppEventKind::Notification { .. }
            | AppEventKind::Error { .. } => {}
        }
        self.transcript.on_event(ev);
        self.status.on_event(ev);
        self.todos.on_event(ev);
    }

    /// A submitted turn enters the transcript before response events, so its
    /// prompt and response always share one scroll coordinate system.
    pub(super) fn push_submitted(&mut self, input: &Input) {
        match input {
            Input::Chat(text) => self.transcript.start_user_turn(&display_user_input(text)),
            Input::Shell(command) if !command.is_empty() => {
                self.transcript.start_shell_turn(command);
            }
            Input::Shell(_) | Input::Slash { .. } | Input::Rewind => {}
        }
    }
}
