//! How the agent's state becomes one provider request, and how the reply
//! arrives: streamed when the model streams, as one shot otherwise.

use std::time::Instant;

use futures::StreamExt;
use oven_llm::{
    CallOptions, Client, ContentBlock, Delta, Message, Provider, Request, Response, Role,
    StreamCollector, StreamEvent as LlmStreamEvent,
};

use oven_host::as_ms;

use super::Agent;
use super::notify::{ThinkingSpan, thinking_span};
use crate::core::prompt_template;
use oven_proto::AgentError;
use oven_proto::EventSink;
use oven_proto::{AgentEvent, StreamEvent};
use oven_proto::{AgentMode, ToolAccess};

/// Some models write a tool call as text instead of a structured call. The
/// markup is not prose, so the transcript drops it; the call itself still
/// fails as a tool result.
const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";

impl Agent {
    fn llm_tools(&self, mode: AgentMode) -> Vec<oven_llm::Tool> {
        self.tools
            .iter()
            .filter(|t| !t.caps().plan_only || mode == AgentMode::Plan)
            .filter(|t| !matches!(mode.tool_access(t.caps().permission), ToolAccess::Hidden))
            .map(|t| oven_llm::Tool {
                name: t.name().to_string(),
                description: Some(t.description().to_string()),
                input_schema: t.schema(),
            })
            .collect()
    }

    /// The system prompt of the next request: the configured one, or the
    /// first system message in the history when there is none, with the mode
    /// overlay on top. The checklist stays off this prefix.
    fn system_prompt(&self, mode: AgentMode) -> Option<String> {
        let history_system = self.history.system_message();
        let base = self.system.as_deref().or(history_system.as_deref());
        prompt_template::compose_system(base, mode)
    }

    /// Plan mode asks for a todo update after several tool rounds skipped the
    /// checklist. The note is appended to that request only.
    fn wants_plan_reminder(&self, mode: AgentMode) -> bool {
        mode == AgentMode::Plan
            && self.todo_misses >= prompt_template::PLAN_REMINDER_AFTER_MISSES
            && !self.todos.is_empty()
    }

    /// The options this turn runs with: the model it goes to, the reasoning
    /// effort the selection carries, and the sampling defaults.
    pub(crate) fn build_options(&self) -> CallOptions {
        let (model, reasoning_effort) = self.selection.model();
        let mut options = CallOptions::new(model);
        options.reasoning_effort = reasoning_effort;
        options
    }

    pub(crate) fn build_request(&self) -> Request {
        let mode = self.selection.mode();
        let mut messages: Vec<Message> = self
            .history
            .messages()
            .filter(|message| message.role != Role::System)
            .cloned()
            .collect();
        let todo_notice = self.todo_notice && !self.todos.is_empty();
        let plan_reminder = self.wants_plan_reminder(mode);
        if todo_notice {
            messages.push(Message::user_text(self.todos.render_todo_block()));
            tracing::debug!(
                mode = mode.label(),
                todo_items = self.todos.items.len(),
                "todo notice appended"
            );
        }
        if plan_reminder {
            messages.push(Message::user_text(prompt_template::PLAN_REMINDER));
            tracing::debug!(
                mode = mode.label(),
                todo_misses = self.todo_misses,
                todo_items = self.todos.items.len(),
                "plan reminder appended"
            );
        }
        let mut builder = Request::builder()
            .messages(messages)
            .tools(self.llm_tools(mode));
        if let Some(system) = self.system_prompt(mode) {
            builder = builder.system(system);
        }
        builder.build()
    }

    pub(super) async fn complete_response(
        &mut self,
        sink: &mut impl EventSink,
    ) -> Result<(Response, Option<(u64, u64)>), AgentError> {
        let started = Instant::now();
        let result = self.complete_or_stream(sink).await;
        tracing::debug!(
            model = %self.model(),
            duration_ms = as_ms(started.elapsed()),
            "llm request complete"
        );
        result
    }

    pub(super) async fn complete_or_stream(
        &mut self,
        sink: &mut impl EventSink,
    ) -> Result<(Response, Option<(u64, u64)>), AgentError> {
        let req = self.build_request();
        let options = self.build_options();
        let router = self.router();
        if router
            .resolve_model(&options.model)
            .is_some_and(|info| !info.capabilities.supports_streaming)
        {
            return complete_unstreamed(&req, &options, router.as_ref(), sink).await;
        }

        match router.stream(&req, &options).await {
            Ok(mut stream) => {
                let mut collector = StreamCollector::new();
                let mut thinking = ThinkingSpan::default();
                let mut calls = ToolCallGate::default();
                while let Some(event) = stream.next().await {
                    match event {
                        Err(e) => return Err(e.into()),
                        Ok(event) => {
                            if let LlmStreamEvent::ContentBlockDelta { delta, .. } = &event {
                                match delta {
                                    Delta::ThinkingDelta { thinking: text } if !text.is_empty() => {
                                        thinking.note();
                                        sink.emit(AgentEvent::Stream(StreamEvent::ThinkingDelta {
                                            text: text.clone(),
                                        }));
                                    }
                                    Delta::TextDelta { text } if !text.is_empty() => {
                                        thinking.close(sink);
                                        if let Some(text) = calls.push(text) {
                                            sink.emit(AgentEvent::Stream(StreamEvent::TextDelta {
                                                text,
                                            }));
                                        }
                                    }
                                    // A streamed tool call ends the reasoning phase.
                                    Delta::InputJsonDelta { .. } => thinking.close(sink),
                                    _ => {}
                                }
                            }
                            collector.push(&event);
                        }
                    }
                }
                let span = thinking.finish(sink);
                if let Some(text) = calls.finish() {
                    sink.emit(AgentEvent::Stream(StreamEvent::TextDelta { text }));
                }
                let mut response = collector.finish()?;
                strip_tool_call_text(&mut response);
                Ok((response, span))
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Whether `text` is a whole tool call written as markup rather than prose.
fn is_tool_call_envelope(text: &str) -> bool {
    let text = text.trim();
    text.starts_with(TOOL_CALL_OPEN) && text.ends_with(TOOL_CALL_CLOSE)
}

/// Removes envelope text blocks from the reply before it reaches the history,
/// so a reloaded session shows what the live one did.
fn strip_tool_call_text(response: &mut Response) {
    response.content.retain(
        |block| !matches!(block, ContentBlock::Text { text } if is_tool_call_envelope(text)),
    );
}

/// Holds the head of a streamed text block until it is clear whether the block
/// is a tool call envelope. Ordinary prose is held for at most the length of
/// the open tag; an envelope is held until it closes, then dropped.
#[derive(Default)]
struct ToolCallGate {
    held: String,
    /// The held text begins with [`TOOL_CALL_OPEN`].
    envelope: bool,
}

impl ToolCallGate {
    /// Returns the text to draw, `None` while the head is still undecided or
    /// the block is an envelope.
    fn push(&mut self, text: &str) -> Option<String> {
        if self.envelope {
            self.held.push_str(text);
            return self.settle();
        }
        if self.held.is_empty() {
            let trimmed = text.trim_start();
            if !TOOL_CALL_OPEN.starts_with(trimmed) && !trimmed.starts_with(TOOL_CALL_OPEN) {
                return Some(text.to_string());
            }
        }
        self.held.push_str(text);
        let trimmed = self.held.trim_start();
        if TOOL_CALL_OPEN.starts_with(trimmed) {
            return None;
        }
        if trimmed.starts_with(TOOL_CALL_OPEN) {
            self.envelope = true;
            return self.settle();
        }
        self.flush()
    }

    /// Decides a held envelope: text after its close tag makes the block prose,
    /// which has to be drawn.
    fn settle(&mut self) -> Option<String> {
        let after_close = self
            .held
            .find(TOOL_CALL_CLOSE)
            .map(|end| &self.held[end + TOOL_CALL_CLOSE.len()..]);
        if after_close.is_some_and(|rest| !rest.trim().is_empty()) {
            return self.flush();
        }
        None
    }

    fn finish(self) -> Option<String> {
        (!self.held.is_empty() && !is_tool_call_envelope(&self.held)).then_some(self.held)
    }

    fn flush(&mut self) -> Option<String> {
        self.envelope = false;
        (!self.held.is_empty()).then(|| std::mem::take(&mut self.held))
    }
}

async fn complete_unstreamed(
    req: &Request,
    options: &CallOptions,
    router: &Client,
    sink: &mut impl EventSink,
) -> Result<(Response, Option<(u64, u64)>), AgentError> {
    let started = Instant::now();
    let mut response = router.complete(req, options).await?;
    strip_tool_call_text(&mut response);
    let reasoning = response.thinking();
    let span = (!reasoning.is_empty()).then(|| thinking_span(started.elapsed()));
    if !reasoning.is_empty() {
        sink.emit(AgentEvent::Stream(StreamEvent::ThinkingDelta {
            text: reasoning,
        }));
        if let Some((_, duration_ms)) = span {
            sink.emit(AgentEvent::Stream(StreamEvent::ThinkingDone {
                duration_ms,
            }));
        }
    }
    let text = response.text();
    if !text.is_empty() {
        sink.emit(AgentEvent::Stream(StreamEvent::TextDelta { text }));
    }
    Ok((response, span))
}

#[cfg(test)]
mod tests {
    use super::{Agent, ToolCallGate, is_tool_call_envelope};
    use oven_llm::{ModelId, ReasoningEffort, Router};

    const ENVELOPE: &str = "<tool_call>\n<function=bash>\n<parameter=command>\nls\n</parameter>\n</function>\n</tool_call>";

    /// The model and the effort this turn goes out with are the ones that
    /// were selected: the model is no longer dropped, and the effort is no
    /// longer read apart from the model — a switch moves both.
    #[test]
    fn the_options_carry_the_selected_model_and_effort() {
        let agent = Agent::new(Router::new(), Vec::new());
        agent
            .selection()
            .switch_model(ModelId::new("kimi-k3"), Some(ReasoningEffort::High));

        let options = agent.build_options();

        assert_eq!(options.model.as_str(), "kimi-k3");
        assert_eq!(options.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn prose_passes_without_waiting() {
        let mut gate = ToolCallGate::default();
        assert_eq!(gate.push("hello"), Some("hello".into()));
        assert_eq!(gate.finish(), None);
    }

    #[test]
    fn a_prefix_of_the_open_tag_waits_for_more_text() {
        let mut gate = ToolCallGate::default();
        assert_eq!(gate.push("<tool"), None);
        assert_eq!(gate.push("?"), Some("<tool?".into()));
        assert_eq!(gate.finish(), None);
    }

    #[test]
    fn a_split_envelope_is_dropped() {
        let mut gate = ToolCallGate::default();
        let (head, tail) = ENVELOPE.split_at(12);
        assert_eq!(gate.push(head), None);
        assert_eq!(gate.push(tail), None);
        assert_eq!(gate.finish(), None);
    }

    #[test]
    fn text_after_an_envelope_is_kept() {
        let mut gate = ToolCallGate::default();
        let answer = format!("{ENVELOPE}\ndone");
        assert_eq!(gate.push(&answer), Some(answer.clone()));
        assert_eq!(gate.finish(), None);
    }

    #[test]
    fn an_incomplete_envelope_is_kept() {
        let mut gate = ToolCallGate::default();
        assert_eq!(gate.push("<tool_call>\n<function=bash>"), None);
        assert_eq!(gate.finish(), Some("<tool_call>\n<function=bash>".into()));
    }

    #[test]
    fn only_a_whole_markup_block_is_an_envelope() {
        assert!(is_tool_call_envelope(ENVELOPE));
        assert!(is_tool_call_envelope(&format!("\n{ENVELOPE}\n")));
        assert!(!is_tool_call_envelope(
            ENVELOPE.trim_end_matches("</tool_call>")
        ));
        assert!(!is_tool_call_envelope("see <tool_call> for an example"));
    }
}
