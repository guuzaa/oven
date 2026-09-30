use crossterm::event::KeyEvent;
use oven_app::{AnswerResponse, ApprovalDecision, LoopLimitDecision, UserRequestId, UserResponse};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::widgets::choice_popup::{ChoicePopup, ChoicePopupAction};
use crate::widgets::question_prompt::{QuestionPrompt, QuestionPromptAction};

use super::Ui;

pub(super) enum OverlayPrompt {
    Approval {
        request_id: UserRequestId,
        popup: ChoicePopup,
    },
    LoopLimit {
        request_id: UserRequestId,
        popup: ChoicePopup,
    },
    Question {
        request_id: UserRequestId,
        popup: QuestionPrompt,
    },
}

impl OverlayPrompt {
    /// Rows the prompt wants; the question needs the width to wrap into them.
    pub(super) fn height(&self, width: u16) -> u16 {
        match self {
            Self::Approval { popup, .. } | Self::LoopLimit { popup, .. } => popup.height(),
            Self::Question { popup, .. } => popup.height(width),
        }
    }

    pub(super) fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        match self {
            Self::Approval { popup, .. } | Self::LoopLimit { popup, .. } => popup.draw(f, area),
            Self::Question { popup, .. } => popup.draw(f, area),
        }
    }

    /// Whether the open question expects its answer typed into the input box.
    pub(super) fn awaits_typed_answer(&self) -> bool {
        matches!(self, Self::Question { popup, .. } if popup.awaits_typed_answer())
    }

    pub(super) fn request_id(&self) -> UserRequestId {
        match self {
            Self::Approval { request_id, .. }
            | Self::LoopLimit { request_id, .. }
            | Self::Question { request_id, .. } => *request_id,
        }
    }

    /// The keys the open prompt answers.
    pub(super) fn hint(&self) -> &'static str {
        match self {
            Self::Approval { popup, .. } | Self::LoopLimit { popup, .. } => popup.hint(),
            Self::Question { .. } => QuestionPrompt::HINT,
        }
    }
}

/// What an open overlay prompt does with a key.
pub(super) enum PromptFlow {
    /// No prompt is open.
    Free,
    /// The prompt kept the key and stays open.
    Kept,
    /// The prompt is finished with: it approved, answered, declined or
    /// cancelled, so it closes.
    Closed,
    /// The open question expects its answer typed into the input box, which
    /// therefore owns the key.
    Typing,
}

impl Ui {
    /// Gives the open overlay prompt first refusal on `key`.
    ///
    /// The prompt is taken out of `self` so the decision it produces can be
    /// sent with `&mut self`, then put back unless it is finished with.
    pub(super) fn handle_prompt_key(&mut self, key: KeyEvent) -> PromptFlow {
        let Some(mut prompt) = self.prompt.take() else {
            return PromptFlow::Free;
        };
        let flow = match &mut prompt {
            OverlayPrompt::Approval { request_id, popup } => match popup.handle_key(key) {
                ChoicePopupAction::Handled => PromptFlow::Kept,
                ChoicePopupAction::Confirm(row) => {
                    let decision = if row == 0 {
                        ApprovalDecision::Approved
                    } else {
                        ApprovalDecision::Rejected
                    };
                    self.app
                        .respond(*request_id, UserResponse::Approval(decision));
                    PromptFlow::Closed
                }
                ChoicePopupAction::Cancel => {
                    self.send_cancel();
                    PromptFlow::Closed
                }
            },
            OverlayPrompt::LoopLimit { request_id, popup } => match popup.handle_key(key) {
                ChoicePopupAction::Handled => PromptFlow::Kept,
                ChoicePopupAction::Confirm(row) => {
                    let decision = if row == 0 {
                        LoopLimitDecision::Continue
                    } else {
                        LoopLimitDecision::Exit
                    };
                    self.app
                        .respond(*request_id, UserResponse::LoopLimit(decision));
                    PromptFlow::Closed
                }
                ChoicePopupAction::Cancel => {
                    self.send_cancel();
                    PromptFlow::Closed
                }
            },
            OverlayPrompt::Question { request_id, popup } => match popup.handle_key(key) {
                QuestionPromptAction::Handled => PromptFlow::Kept,
                QuestionPromptAction::Typing => PromptFlow::Typing,
                QuestionPromptAction::Answered(answer) => {
                    self.respond_question(*request_id, AnswerResponse::Answered { answer });
                    PromptFlow::Closed
                }
                QuestionPromptAction::Declined => {
                    self.respond_question(*request_id, AnswerResponse::Declined);
                    PromptFlow::Closed
                }
                QuestionPromptAction::Cancelled => {
                    self.send_cancel();
                    PromptFlow::Closed
                }
            },
        };
        if matches!(flow, PromptFlow::Kept | PromptFlow::Typing) {
            self.prompt = Some(prompt);
        }
        flow
    }

    /// Keeps the composer's completion popups shut while an answer is being
    /// typed: they draw under the question prompt, and Tab would otherwise
    /// complete command text straight into the answer.
    pub(super) fn suppress_completions(&mut self) {
        if self
            .prompt
            .as_ref()
            .is_some_and(OverlayPrompt::awaits_typed_answer)
        {
            self.input.close_popups();
        }
    }

    /// Routes submitted text to the open question when it is waiting for a
    /// typed answer, returning whether the text was consumed as that answer.
    pub(super) fn answer_question_with(&mut self, text: &str) -> bool {
        let Some(OverlayPrompt::Question { request_id, popup }) = self.prompt.as_ref() else {
            return false;
        };
        if !popup.awaits_typed_answer() {
            return false;
        }
        let request_id = *request_id;
        self.prompt = None;
        self.respond_question(
            request_id,
            AnswerResponse::Answered {
                answer: text.to_string(),
            },
        );
        true
    }
}
