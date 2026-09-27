use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolView};
use crate::error::AgentError;
use crate::question::{AnswerResponse, Question, QuestionOption};

const USER_ANSWER_PREFIX: &str = "the user answered: ";
const USER_SKIPPED: &str = "the user skipped the question without answering";
const TRUNCATION_MARK: &str = "…";
/// The question is read in a prompt on screen, so it stays within a screenful.
const MAX_QUESTION_CHARS: usize = 1000;
const MAX_OPTIONS: usize = 4;
const MAX_LABEL_CHARS: usize = 60;
const MAX_DESCRIPTION_CHARS: usize = 200;
/// The answer becomes a tool result in the conversation, and the user can paste
/// into it, so it is capped as well.
const MAX_ANSWER_CHARS: usize = 4000;

#[derive(Deserialize)]
struct AnswerArgs {
    question: String,
    #[serde(default)]
    options: Vec<QuestionOption>,
}

/// Puts a question to the user and returns their answer, blocking the turn
/// until they reply.
pub struct AnswerTool;

impl AnswerTool {
    pub const NAME: &'static str = "answer";

    pub fn view_input(input: &Value) -> ToolView {
        super::labeled(Self::NAME, "Ask", input, "question")
    }

    /// Text is clamped to the limits above rather than rejected: a verbose
    /// question must still reach the user, since failing the call shows them
    /// nothing at all. Only a structural mistake is worth a retry.
    fn parse(value: &Value) -> Result<Question, String> {
        let args = AnswerArgs::deserialize(value).map_err(|e| format!("answer: {e}"))?;
        let question = args.question.trim();
        if question.is_empty() {
            return Err("answer: empty question".into());
        }
        if args.options.len() > MAX_OPTIONS {
            return Err(format!("answer: too many options (max {MAX_OPTIONS})"));
        }
        let mut options = Vec::with_capacity(args.options.len());
        for option in args.options {
            let label = option.label.trim();
            if label.is_empty() {
                return Err("answer: empty option label".into());
            }
            options.push(QuestionOption {
                label: clamp(label, MAX_LABEL_CHARS),
                description: option
                    .description
                    .map(|text| clamp(text.trim(), MAX_DESCRIPTION_CHARS))
                    .filter(|text| !text.is_empty()),
            });
        }
        Ok(Question {
            question: clamp(question, MAX_QUESTION_CHARS),
            options,
        })
    }
}

#[async_trait]
impl Tool for AnswerTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }

    fn description(&self) -> &'static str {
        "Ask the user a question and wait for their answer. Use it when the request\n\
         is ambiguous, when a choice changes what you are about to build, or before\n\
         something hard to undo. Offer the two to four concrete answers you are\n\
         choosing between; the user can always type a different one. Keep the\n\
         question short and specific. Do not use it to report progress or to ask\n\
         for permission to run a tool."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["question"],
            "additionalProperties": false,
            "properties": {
                "question": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_QUESTION_CHARS,
                    "description": "The question, shown to the user verbatim."
                },
                "options": {
                    "type": "array",
                    "maxItems": MAX_OPTIONS,
                    "description": "Concrete answers to choose from. Omit to ask for free text.",
                    "items": {
                        "type": "object",
                        "required": ["label"],
                        "additionalProperties": false,
                        "properties": {
                            "label": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": MAX_LABEL_CHARS
                            },
                            "description": {
                                "type": "string",
                                "maxLength": MAX_DESCRIPTION_CHARS
                            }
                        }
                    }
                }
            }
        })
    }

    async fn run(&self, args: &Value, ctx: &ToolContext<'_>) -> Result<String, AgentError> {
        let question = Self::parse(args).map_err(AgentError::from)?;
        match ctx.ask(question).await? {
            AnswerResponse::Answered { answer } => Ok(format!(
                "{USER_ANSWER_PREFIX}{}",
                clamp(&answer, MAX_ANSWER_CHARS)
            )),
            AnswerResponse::Declined => Ok(USER_SKIPPED.to_string()),
        }
    }
}

/// `text` cut to `max` characters, marked by a trailing ellipsis when it was.
fn clamp(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clamped: String = text.chars().take(max.saturating_sub(1)).collect();
    clamped.push_str(TRUNCATION_MARK);
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{QuestionRequest, QuestionSender};
    use crate::tools::NO_USER_TO_ANSWER;
    use serde_json::json;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    const QUESTION: &str = "which database?";
    const ANSWER: &str = "postgres";

    fn parsed(value: Value) -> Question {
        AnswerTool::parse(&value).unwrap()
    }

    async fn run_with(
        tx: &QuestionSender,
        args: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let ctx = ToolContext::new(cancel, Some(tx));
        AnswerTool.run(&args, &ctx).await
    }

    #[test]
    fn question_without_options_is_free_form() {
        let question = parsed(json!({ "question": "  which database?  " }));
        assert_eq!(question.question, "which database?");
        assert!(question.options.is_empty());
    }

    #[test]
    fn options_keep_labels_and_trim_descriptions() {
        let question = parsed(json!({
            "question": "which database?",
            "options": [
                { "label": " postgres ", "description": " relational " },
                { "label": "sqlite" }
            ]
        }));
        assert_eq!(question.options.len(), 2);
        assert_eq!(question.options[0].label, "postgres");
        assert_eq!(
            question.options[0].description.as_deref(),
            Some("relational")
        );
        assert_eq!(question.options[1].description, None);
    }

    #[test]
    fn blank_description_becomes_none() {
        let question = parsed(json!({
            "question": "q",
            "options": [{ "label": "yes", "description": "   " }]
        }));
        assert_eq!(question.options[0].description, None);
    }

    #[test]
    fn missing_question_is_rejected() {
        assert!(AnswerTool::parse(&json!({ "options": [] })).is_err());
    }

    #[test]
    fn blank_question_is_rejected() {
        assert!(AnswerTool::parse(&json!({ "question": "   " })).is_err());
    }

    #[test]
    fn blank_label_is_rejected() {
        let error = AnswerTool::parse(&json!({
            "question": "q",
            "options": [{ "label": " " }]
        }))
        .unwrap_err();
        assert_eq!(error, "answer: empty option label");
    }

    #[test]
    fn too_many_options_are_rejected() {
        let options: Vec<Value> = (0..=MAX_OPTIONS)
            .map(|i| json!({ "label": format!("option {i}") }))
            .collect();
        assert!(AnswerTool::parse(&json!({ "question": "q", "options": options })).is_err());
    }

    #[test]
    fn oversized_text_is_clamped_not_rejected() {
        let long = "x".repeat(MAX_QUESTION_CHARS * 2);
        let question = parsed(json!({ "question": long }));
        assert_eq!(question.question.chars().count(), MAX_QUESTION_CHARS);
        assert!(question.question.ends_with(TRUNCATION_MARK));

        let long_label = "y".repeat(MAX_LABEL_CHARS * 2);
        let long_description = "z".repeat(MAX_DESCRIPTION_CHARS * 2);
        let question = parsed(json!({
            "question": "q",
            "options": [{ "label": long_label, "description": long_description }]
        }));
        assert_eq!(question.options[0].label.chars().count(), MAX_LABEL_CHARS);
        assert_eq!(
            question.options[0]
                .description
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            MAX_DESCRIPTION_CHARS
        );
    }

    #[test]
    fn text_within_the_limits_is_untouched() {
        let at_limit = "q".repeat(MAX_QUESTION_CHARS);
        let expected = at_limit.clone();
        assert_eq!(parsed(json!({ "question": at_limit })).question, expected);
        assert!(
            !parsed(json!({ "question": "short" }))
                .question
                .ends_with(TRUNCATION_MARK)
        );
    }

    #[tokio::test]
    async fn returns_what_the_user_picked() {
        let (tx, mut rx) = mpsc::unbounded_channel::<QuestionRequest>();
        let asker = tokio::spawn(async move {
            let request = rx.recv().await.unwrap();
            assert_eq!(request.question.question, QUESTION);
            request
                .responder
                .send(AnswerResponse::Answered {
                    answer: ANSWER.into(),
                })
                .unwrap();
        });
        let output = run_with(&tx, json!({ "question": QUESTION }), None)
            .await
            .unwrap();
        assert_eq!(output, format!("{USER_ANSWER_PREFIX}{ANSWER}"));
        asker.await.unwrap();
    }

    #[tokio::test]
    async fn a_skipped_question_is_not_an_error() {
        let (tx, mut rx) = mpsc::unbounded_channel::<QuestionRequest>();
        let asker = tokio::spawn(async move {
            rx.recv()
                .await
                .unwrap()
                .responder
                .send(AnswerResponse::Declined)
                .unwrap();
        });
        let output = run_with(&tx, json!({ "question": QUESTION }), None)
            .await
            .unwrap();
        assert_eq!(output, USER_SKIPPED);
        asker.await.unwrap();
    }

    #[tokio::test]
    async fn a_verbose_question_still_reaches_the_user() {
        let (tx, mut rx) = mpsc::unbounded_channel::<QuestionRequest>();
        let asker = tokio::spawn(async move {
            let request = rx.recv().await.unwrap();
            assert_eq!(
                request.question.question.chars().count(),
                MAX_QUESTION_CHARS
            );
            request
                .responder
                .send(AnswerResponse::Answered {
                    answer: ANSWER.into(),
                })
                .unwrap();
        });
        let verbose = "context ".repeat(MAX_QUESTION_CHARS);
        let output = run_with(&tx, json!({ "question": verbose }), None)
            .await
            .unwrap();
        assert_eq!(output, format!("{USER_ANSWER_PREFIX}{ANSWER}"));
        asker.await.unwrap();
    }

    #[tokio::test]
    async fn a_pasted_answer_is_capped_before_it_reaches_the_model() {
        const TAIL: &str = "hidden tail";
        let (tx, mut rx) = mpsc::unbounded_channel::<QuestionRequest>();
        let answer = format!("{}{TAIL}", "x".repeat(MAX_ANSWER_CHARS * 2));
        let asker = tokio::spawn(async move {
            rx.recv()
                .await
                .unwrap()
                .responder
                .send(AnswerResponse::Answered { answer })
                .unwrap();
        });
        let output = run_with(&tx, json!({ "question": QUESTION }), None)
            .await
            .unwrap();
        assert_eq!(
            output.chars().count(),
            USER_ANSWER_PREFIX.chars().count() + MAX_ANSWER_CHARS
        );
        assert!(output.ends_with(TRUNCATION_MARK));
        assert!(!output.contains(TAIL), "the pasted tail must be cut");
        asker.await.unwrap();
    }

    #[tokio::test]
    async fn without_a_frontend_the_tool_fails() {
        let ctx = ToolContext::new(None, None);
        let error = AnswerTool
            .run(&json!({ "question": QUESTION }), &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.message, NO_USER_TO_ANSWER);
    }

    #[tokio::test]
    async fn a_cancelled_turn_stops_waiting() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = run_with(&tx, json!({ "question": QUESTION }), Some(&cancel))
            .await
            .unwrap_err();
        assert!(error.is_cancelled());
    }

    #[tokio::test]
    async fn malformed_arguments_never_reach_the_user() {
        let (tx, mut rx) = mpsc::unbounded_channel::<QuestionRequest>();
        assert!(
            run_with(&tx, json!({ "question": "   " }), None)
                .await
                .is_err()
        );
        assert!(rx.try_recv().is_err());
    }
}
