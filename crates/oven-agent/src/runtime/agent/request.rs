//! How the agent's state becomes one provider request, and how the reply
//! arrives: streamed when the model streams, as one shot otherwise.

use std::time::Instant;

use futures::StreamExt;
use oven_llm::{
    Delta, Provider, ReasoningEffort, Request, Response, Role, SamplingParams, StreamCollector,
    StreamEvent as LlmStreamEvent, ThinkingMode, ToolChoice,
};

use oven_host::as_ms;

use super::Agent;
use super::notify::{ThinkingSpan, thinking_span};
use crate::core::error::AgentError;
use crate::core::event::{AgentEvent, StreamEvent};
use crate::core::mode::{AgentMode, ToolAccess};
use crate::core::prompt_template;
use crate::core::sink::EventSink;

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
    /// and todo overlays composed on top of it.
    fn system_prompt(&self, mode: AgentMode) -> Option<String> {
        let history_system = self.history.system_message();
        let base = self.system.as_deref().or(history_system.as_deref());
        prompt_template::compose_todo_system(
            base,
            mode,
            &self.todos,
            self.wants_plan_reminder(mode),
        )
    }

    /// Plan mode asks for a todo update when the previous step used tools
    /// without writing to the list.
    fn wants_plan_reminder(&self, mode: AgentMode) -> bool {
        mode == AgentMode::Plan && self.todo_dirty && !self.todos.is_empty()
    }

    pub(crate) fn build_request(&self) -> Request {
        let mode = self.selection.mode();
        let (model, reasoning_effort) = self.selection.model();
        Request {
            model,
            system: self.system_prompt(mode),
            messages: self
                .history
                .messages()
                .filter(|m| m.role != Role::System)
                .cloned()
                .collect(),
            tools: self.llm_tools(mode),
            tool_choice: ToolChoice::Auto,
            sampling: SamplingParams {
                temperature: Some(1.0),
                max_tokens: None,
                ..Default::default()
            },
            thinking: Some(
                if reasoning_effort.is_some_and(|effort| effort != ReasoningEffort::None) {
                    ThinkingMode::Enabled
                } else {
                    ThinkingMode::Disabled
                },
            ),
            reasoning_effort,
            provider_options: serde_json::Map::default(),
        }
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
        let router = self.router();

        match router.stream(&req).await {
            Ok(mut stream) => {
                let mut collector = StreamCollector::new();
                let mut thinking = ThinkingSpan::default();
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
                                        sink.emit(AgentEvent::Stream(StreamEvent::TextDelta {
                                            text: text.clone(),
                                        }));
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
                Ok((collector.finish()?, span))
            }
            Err(error) => {
                tracing::warn!(error = %error, model = %self.model(), "stream start failed, falling back to complete");
                let started = Instant::now();
                let response = Provider::complete(&*router, &req).await?;
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
        }
    }
}
