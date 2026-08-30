use futures::StreamExt;
use oven_llm::{
    ContentBlock, Delta, Message, ModelId, Provider, ReasoningEffort, Request, Response, Role,
    Router, SamplingParams, StreamCollector, StreamEvent as LlmStreamEvent, ThinkingMode,
    ToolChoice, Usage,
};

use crate::error::AgentError;
use crate::event::{AgentEvent, StreamEvent, ToolEvent, ToolResult, TurnEvent};
use crate::history::{History, Record};
use crate::identity::{AgentId, ToolCallId};
use crate::mode::AgentMode;
use crate::sink::EventSink;
use crate::todo::{self, TodoList};
use crate::tools::Tool;
use crate::turn::{TurnContext, TurnOutput};

/// The conversation driver. Holds tools and dispatches tool calls returned by
/// the provider until the provider replies without tool calls.
pub struct Agent {
    id: AgentId,
    pub(crate) router: Router,
    pub(crate) tools: Vec<Box<dyn Tool>>,
    pub(crate) history: History,
    model: ModelId,
    system: Option<String>,
    mode: AgentMode,
    todos: TodoList,
    reasoning_effort: Option<ReasoningEffort>,
    max_iters: usize,
    todo_written_this_turn: bool,
    todo_dirty: bool,
}

impl Agent {
    pub fn new(router: Router, tools: Vec<Box<dyn Tool>>) -> Self {
        Self {
            id: AgentId::next(),
            router,
            tools,
            history: History::new(),
            model: ModelId::new("default"),
            system: None,
            mode: AgentMode::Default,
            todos: TodoList::default(),
            reasoning_effort: None,
            max_iters: 100,
            todo_written_this_turn: false,
            todo_dirty: false,
        }
    }

    pub fn with_id(mut self, id: AgentId) -> Self {
        self.id = id;
        self
    }

    pub fn id(&self) -> AgentId {
        self.id
    }

    pub fn with_model(mut self, model: impl Into<ModelId>) -> Self {
        self.model = model.into();
        self
    }

    pub fn model(&self) -> &ModelId {
        &self.model
    }

    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
    }

    pub fn set_model(&mut self, model: impl Into<ModelId>) {
        self.model = model.into();
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.reasoning_effort = effort;
    }

    pub fn set_system(&mut self, content: impl Into<String>) {
        self.system = Some(content.into());
    }

    pub fn set_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    pub fn set_todos(&mut self, todos: TodoList) {
        self.todos = todos;
    }

    pub fn todos(&self) -> &TodoList {
        &self.todos
    }

    pub fn todo_written_this_turn(&self) -> bool {
        self.todo_written_this_turn
    }

    /// Set the reasoning effort for provider calls.
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    pub fn with_max_iters(mut self, n: usize) -> Self {
        self.max_iters = n;
        self
    }

    pub fn history(&self) -> impl ExactSizeIterator<Item = &Message> + '_ {
        self.history.messages()
    }

    pub fn history_revision(&self) -> u64 {
        self.history.revision()
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.todo_dirty = false;
    }

    /// Replace the entire history with records loaded from a persisted
    /// session (messages plus a `TokenUsage` record after each turn's final
    /// assistant message). The cumulative total is rebuilt from the restored
    /// usage.
    pub fn restore_history(&mut self, records: Vec<Record>) {
        self.history.set_messages_with_records(records);
    }

    /// Append a message to the history. Used by the App layer to preload a
    /// persisted session.
    pub fn push_history(&mut self, message: Message) {
        self.history.push(message);
    }

    /// Record the session's workspace root if it is not already known (a
    /// resumed session keeps its original root and creation time). Persisted
    /// as the first record of the session file.
    pub fn ensure_session_meta(&mut self, root: String) {
        self.history.ensure_session_meta(root);
    }

    /// The history as persistence-ready records: messages plus a `TokenUsage`
    /// record after each turn's final assistant message. The App layer
    /// persists these as JSONL lines.
    pub fn history_records(&self) -> Vec<Record> {
        self.history.records()
    }

    /// Remove the last user turn from the conversation history, returning
    /// the removed user message, or `None` when there is nothing to rewind.
    /// The removed turn's cumulative token usage is rolled back out of the
    /// running total.
    pub fn rewind_last_turn(&mut self) -> Option<Message> {
        let removed = self.history.rewind_last_turn();
        if removed.is_some() {
            self.todo_dirty = false;
        }
        removed
    }

    fn llm_tools(&self) -> Vec<oven_llm::Tool> {
        let mode = self.mode;
        self.tools
            .iter()
            .filter(|t| !t.caps().plan_only || mode == AgentMode::Plan)
            .map(|t| oven_llm::Tool {
                name: t.name().to_string(),
                description: Some(t.description().to_string()),
                input_schema: t.schema(),
            })
            .collect()
    }

    fn build_request(&self) -> Request {
        let tools = self.llm_tools();
        let mut system = self.system.clone();
        let todos = &self.todos;
        let mode = self.mode;
        let mut messages = Vec::with_capacity(self.history.len());
        for m in self.history.messages() {
            if m.role == Role::System {
                if system.is_none() {
                    system = m.system_prompt();
                }
            } else {
                messages.push(m.clone());
            }
        }
        system = todo::compose_system(system.as_deref(), mode);
        if !todos.is_empty() {
            match system.as_mut() {
                Some(s) => {
                    s.push_str("\n\n");
                    s.push_str(&todos.render_prompt_block());
                }
                None => system = Some(todos.render_prompt_block()),
            }
        }
        if let Some(s) = system.as_mut()
            && mode == AgentMode::Plan
            && self.todo_dirty
            && !todos.is_empty()
        {
            s.push_str("\n\n");
            s.push_str(todo::PLAN_REMINDER);
        }
        Request {
            model: self.model.clone(),
            system,
            messages,
            tools,
            tool_choice: ToolChoice::Auto,
            sampling: SamplingParams {
                temperature: Some(1.0),
                max_tokens: None,
                ..Default::default()
            },
            thinking: Some(
                if self
                    .reasoning_effort
                    .is_some_and(|effort| effort != ReasoningEffort::None)
                {
                    ThinkingMode::Enabled
                } else {
                    ThinkingMode::Disabled
                },
            ),
            reasoning_effort: self.reasoning_effort,
            provider_options: Default::default(),
        }
    }

    async fn dispatch(
        &self,
        name: &str,
        args: &serde_json::Value,
        ctx: &TurnContext,
    ) -> Result<String, AgentError> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| AgentError::from(format!("unknown tool: {name}")))?;
        tool.run(args, Some(&ctx.cancellation)).await
    }

    async fn complete_response(
        &mut self,
        sink: &mut impl EventSink,
    ) -> Result<Response, AgentError> {
        let req = self.build_request();

        match self.router.stream(&req).await {
            Ok(mut stream) => {
                let mut collector = StreamCollector::new();
                while let Some(event) = stream.next().await {
                    match event {
                        Err(e) => return Err(e.into()),
                        Ok(event) => {
                            if let LlmStreamEvent::ContentBlockDelta { delta, .. } = &event {
                                match delta {
                                    Delta::ThinkingDelta { thinking } if !thinking.is_empty() => {
                                        sink.emit(AgentEvent::Stream(StreamEvent::ThinkingDelta {
                                            text: thinking.clone(),
                                        }));
                                    }
                                    Delta::TextDelta { text } if !text.is_empty() => {
                                        sink.emit(AgentEvent::Stream(StreamEvent::TextDelta {
                                            text: text.clone(),
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                            collector.push(&event);
                        }
                    }
                }
                Ok(collector.finish()?)
            }
            Err(_) => {
                let response = Provider::complete(&self.router, &req).await?;
                if !response.has_tool_use() {
                    let thinking = response.thinking();
                    if !thinking.is_empty() {
                        sink.emit(AgentEvent::Stream(StreamEvent::ThinkingDelta {
                            text: thinking,
                        }));
                    }
                    let text = response.text();
                    if !text.is_empty() {
                        sink.emit(AgentEvent::Stream(StreamEvent::TextDelta { text }));
                    }
                }
                Ok(response)
            }
        }
    }

    async fn step(
        &mut self,
        sink: &mut impl EventSink,
        ctx: &TurnContext,
    ) -> Result<Option<String>, AgentError> {
        self.mode = ctx.mode();
        let response = self.complete_response(sink).await?;

        self.history
            .push(Message::assistant(response.content.clone()));
        if let Some(usage) = &response.usage {
            self.history.record_usage(usage);
        }

        if !response.has_tool_use() {
            let text = response.text();
            return Ok(Some(text));
        }

        let mut wrote_todo = false;
        for block in response.tool_uses() {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            let (view, writes_todos) = match self.tools.iter().find(|t| t.name() == name) {
                Some(t) => (t.view(input), t.caps().writes_todos),
                None => (crate::tools::present_tool(name, input), false),
            };
            let call_id = ToolCallId::next();
            sink.emit(AgentEvent::Tool(ToolEvent::Started {
                call_id,
                name: name.clone(),
                view,
            }));
            let result = match self.dispatch(name, input, ctx).await {
                Ok(r) => {
                    if writes_todos && let Ok(list) = TodoList::parse(input) {
                        self.todos = list.clone();
                        self.todo_written_this_turn = true;
                        wrote_todo = true;
                        sink.emit(AgentEvent::TodosChanged { todos: list });
                    }
                    ToolResult::Success {
                        output: truncate(&r, 1_500_000),
                    }
                }
                Err(e) => {
                    let output = truncate(&format!("error: {e}"), 1_500_000);
                    ToolResult::Failed {
                        error: e.to_string(),
                        output: Some(output),
                    }
                }
            };
            let summary = result.output().to_string();
            let is_error = !result.is_success();
            sink.emit(AgentEvent::Tool(ToolEvent::Finished { call_id, result }));
            self.history
                .push(Message::tool_result(id.clone(), summary, is_error));
        }
        self.todo_dirty = !wrote_todo;
        Ok(None)
    }

    pub async fn run(
        &mut self,
        input: impl Into<String>,
        ctx: &TurnContext,
        sink: &mut impl EventSink,
    ) -> Result<TurnOutput, AgentError> {
        let input: String = input.into();
        sink.emit(AgentEvent::Turn(TurnEvent::Started));

        self.todo_written_this_turn = false;
        let turn = async {
            self.history.push(Message::user_text(input));

            for _ in 0..self.max_iters {
                match self.step(sink, ctx).await {
                    Ok(Some(final_text)) => {
                        let usage = self.last_turn_usage();
                        sink.emit(AgentEvent::Turn(TurnEvent::Completed { usage }));
                        return Ok(TurnOutput {
                            response: Message::assistant_text(final_text),
                            usage,
                        });
                    }
                    Ok(None) => continue,
                    Err(e) => return Err(e),
                }
            }
            Err(AgentError::from("agent loop exceeded max iterations"))
        };

        let result = {
            tokio::pin!(turn);
            tokio::select! {
                biased;
                _ = ctx.cancellation.cancelled() => Err(AgentError::cancelled()),
                res = &mut turn => res,
            }
        };
        self.mode = ctx.mode();

        match &result {
            Ok(_) => {}
            Err(e) if e.is_cancelled() => {
                sink.emit(AgentEvent::Turn(TurnEvent::Cancelled));
            }
            Err(e) => {
                sink.emit(AgentEvent::Turn(TurnEvent::Failed { error: e.clone() }));
            }
        }
        result
    }

    /// Token usage of the last user turn.
    #[inline]
    pub fn last_turn_usage(&self) -> Usage {
        self.history.last_turn_usage()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}\n...[truncated]", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TurnId;
    use crate::identity::ToolCallId;
    use crate::sink::{NullSink, VecEventSink};
    use crate::tools::{FileReadTool, FileWriteTool};
    use crate::turn::TurnContext;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{
        ModelInfo, ProviderError, ProviderName, Result as LlmResult, StopReason,
        StreamEvent as LlmStreamEvent,
    };
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    fn turn_ctx() -> TurnContext {
        TurnContext::new(TurnId::next(), CancellationToken::new(), AgentMode::Default)
    }

    async fn run_text(agent: &mut Agent, input: &str) -> String {
        let mut sink = NullSink;
        let ctx = TurnContext::new(TurnId::next(), CancellationToken::new(), agent.mode());
        agent.run(input, &ctx, &mut sink).await.unwrap().text()
    }

    async fn run_with(
        agent: &mut Agent,
        input: &str,
        ctx: &TurnContext,
        sink: &mut impl EventSink,
    ) -> Result<TurnOutput, AgentError> {
        agent.run(input, ctx, sink).await
    }

    fn is_terminal(event: &AgentEvent) -> bool {
        matches!(
            event,
            AgentEvent::Turn(
                TurnEvent::Completed { .. } | TurnEvent::Cancelled | TurnEvent::Failed { .. }
            )
        )
    }

    fn assert_valid_event_sequence(events: &[AgentEvent]) {
        assert!(
            matches!(events.first(), Some(AgentEvent::Turn(TurnEvent::Started))),
            "turn must start with Started: {events:?}"
        );
        let started = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Turn(TurnEvent::Started)))
            .count();
        assert_eq!(started, 1, "exactly one Started: {events:?}");
        let terminals = events.iter().filter(|e| is_terminal(e)).count();
        assert_eq!(terminals, 1, "exactly one terminal: {events:?}");
        assert!(
            is_terminal(events.last().unwrap()),
            "last event must be terminal: {events:?}"
        );

        let mut open: Vec<ToolCallId> = Vec::new();
        for event in events {
            match event {
                AgentEvent::Tool(ToolEvent::Started { call_id, .. }) => open.push(*call_id),
                AgentEvent::Tool(ToolEvent::Finished { call_id, .. }) => {
                    assert!(
                        open.iter().any(|id| id == call_id),
                        "Finished without Started: {call_id:?}"
                    );
                    open.retain(|id| id != call_id);
                }
                AgentEvent::Tool(ToolEvent::OutputDelta { call_id, .. }) => {
                    assert!(
                        open.iter().any(|id| id == call_id),
                        "OutputDelta without Started: {call_id:?}"
                    );
                }
                _ => {}
            }
        }
    }

    fn router_with(provider: Box<dyn Provider>) -> Router {
        let mut router = Router::new();
        router.register(provider);
        router
    }

    struct MockProvider {
        responses: Mutex<VecDeque<Response>>,
    }

    impl MockProvider {
        fn new(responses: Vec<Response>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn complete(&self, _req: &Request) -> LlmResult<Response> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ProviderError::Api {
                    status: 500,
                    body: "no more mock responses".into(),
                })
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> LlmResult<BoxStream<'static, LlmResult<LlmStreamEvent>>> {
            Err(ProviderError::Api {
                status: 500,
                body: "stream disabled in mock".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("mock".into())
        }
    }

    fn text_response(text: &str) -> Response {
        Response {
            id: "resp".into(),
            model: "mock".into(),
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            }),
        }
    }

    fn tool_response(id: &str, name: &str, input: serde_json::Value) -> Response {
        Response {
            id: "resp".into(),
            model: "mock".into(),
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            }),
        }
    }

    fn content_has(m: &Message, needle: &str) -> bool {
        m.content.iter().any(|b| match b {
            ContentBlock::Text { text } => text.contains(needle),
            ContentBlock::ToolResult { content, .. } => content.iter().any(|c| match c {
                ContentBlock::Text { text } => text.contains(needle),
                _ => false,
            }),
            _ => false,
        })
    }

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-test-agent").unwrap()
    }

    #[tokio::test]
    async fn agent_loop_executes_tool_then_finishes() {
        let tmp = tmp_dir();
        let root = tmp.path();
        std::fs::write(root.join("note.txt"), "hello world").unwrap();

        let mock = MockProvider::new(vec![
            tool_response("call_1", "file_read", json!({"path": "note.txt"})),
            text_response("done"),
        ]);

        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool::new(root)),
            Box::new(FileWriteTool::new(root)),
        ];
        let mut agent = Agent::new(router_with(Box::new(mock)), tools).with_max_iters(4);
        let result = run_text(&mut agent, "read note.txt").await;
        assert_eq!(result, "done");
        assert!(agent.history.iter().any(|m| content_has(m, "hello world")));
    }

    #[tokio::test]
    async fn event_sequence_for_tool_calling_turn() {
        let tmp = tmp_dir();
        let root = tmp.path();
        std::fs::write(root.join("note.txt"), "hello world").unwrap();

        let mock = MockProvider::new(vec![
            tool_response("call_1", "file_read", json!({"path": "note.txt"})),
            text_response("all good"),
        ]);

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FileReadTool::new(root))];
        let mut agent = Agent::new(router_with(Box::new(mock)), tools)
            .with_id(AgentId(7))
            .with_max_iters(4);

        let mut sink = VecEventSink::default();
        let result = run_with(&mut agent, "read it", &turn_ctx(), &mut sink)
            .await
            .unwrap();
        assert_eq!(result.text(), "all good");

        let events = sink.events;
        assert_valid_event_sequence(&events);
        assert!(matches!(
            events.first(),
            Some(AgentEvent::Turn(TurnEvent::Started))
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Tool(ToolEvent::Started {
                name,
                view,
                ..
            }) if name == "file_read" && view.summary == "Read note.txt"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Tool(ToolEvent::Finished {
                result: ToolResult::Success { output },
                ..
            }) if output == "file: note.txt\nlines: 1-1\n\nL1→hello world"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Stream(StreamEvent::TextDelta { text }) if text == "all good"
        )));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Turn(TurnEvent::Completed { .. }))
        ));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Turn(_)))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn cancel_before_run_emits_cancelled() {
        let mock = MockProvider::new(vec![]);
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new()).with_id(AgentId(1));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut sink = VecEventSink::default();
        let err = agent
            .run(
                "hi",
                &TurnContext::new(TurnId::next(), cancel, AgentMode::Default),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert!(err.is_cancelled());
        assert_valid_event_sequence(&sink.events);
        assert_eq!(
            sink.events,
            vec![
                AgentEvent::Turn(TurnEvent::Started),
                AgentEvent::Turn(TurnEvent::Cancelled),
            ]
        );
    }

    #[tokio::test]
    async fn successful_turn_has_valid_lifecycle() {
        let mock = MockProvider::new(vec![text_response("ok")]);
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
        let mut sink = VecEventSink::default();
        let out = run_with(&mut agent, "hi", &turn_ctx(), &mut sink)
            .await
            .unwrap();
        assert_eq!(out.text(), "ok");
        assert_valid_event_sequence(&sink.events);
        assert!(matches!(
            sink.events.last(),
            Some(AgentEvent::Turn(TurnEvent::Completed { .. }))
        ));
    }

    #[tokio::test]
    async fn completed_usage_is_the_last_turn_not_session_total() {
        let mock = MockProvider::new(vec![text_response("one"), text_response("two")]);
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
        let mut sink = VecEventSink::default();
        let out1 = run_with(&mut agent, "first", &turn_ctx(), &mut sink)
            .await
            .unwrap();
        assert_eq!(out1.usage.input_tokens, 10);

        sink.events.clear();
        let out2 = run_with(&mut agent, "second", &turn_ctx(), &mut sink)
            .await
            .unwrap();
        assert_eq!(out2.usage.input_tokens, 10);
        assert_eq!(out2.usage.output_tokens, 5);
        assert_eq!(agent.last_turn_usage().input_tokens, 10);
        match sink.events.last() {
            Some(AgentEvent::Turn(TurnEvent::Completed { usage })) => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 5);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancelled_turn_has_one_terminal_event() {
        let mock = MockProvider::new(vec![]);
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut sink = VecEventSink::default();
        let err = agent
            .run(
                "hi",
                &TurnContext::new(TurnId::next(), cancel, AgentMode::Default),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert!(err.is_cancelled());
        assert_valid_event_sequence(&sink.events);
        assert!(matches!(
            sink.events.last(),
            Some(AgentEvent::Turn(TurnEvent::Cancelled))
        ));
    }

    #[tokio::test]
    async fn failed_turn_has_one_terminal_event() {
        let mock = MockProvider::new(vec![]);
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
        let mut sink = VecEventSink::default();
        let err = run_with(&mut agent, "hi", &turn_ctx(), &mut sink)
            .await
            .unwrap_err();
        assert!(!err.is_cancelled());
        assert_valid_event_sequence(&sink.events);
        assert!(matches!(
            sink.events.last(),
            Some(AgentEvent::Turn(TurnEvent::Failed { .. }))
        ));
    }

    struct CaptureSystem {
        system: Arc<Mutex<Option<Option<String>>>>,
    }

    impl CaptureSystem {
        fn new() -> (Self, Arc<Mutex<Option<Option<String>>>>) {
            let system = Arc::new(Mutex::new(None));
            (
                Self {
                    system: Arc::clone(&system),
                },
                system,
            )
        }
    }

    #[async_trait]
    impl Provider for CaptureSystem {
        async fn complete(&self, req: &Request) -> LlmResult<Response> {
            *self.system.lock().unwrap() = Some(req.system.clone());
            Ok(text_response("ok"))
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> LlmResult<BoxStream<'static, LlmResult<LlmStreamEvent>>> {
            Err(ProviderError::Api {
                status: 500,
                body: "stream disabled in mock".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("capture".into())
        }
    }

    #[tokio::test]
    async fn set_system_is_reflected_in_request() {
        let (mock, seen) = CaptureSystem::new();
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
        agent.set_system("hello system");
        let result = run_text(&mut agent, "hi").await;
        assert_eq!(result, "ok");
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some(Some("hello system".into()))
        );
    }

    #[tokio::test]
    async fn history_system_used_when_no_base_system() {
        let (mock, seen) = CaptureSystem::new();
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
        agent.push_history(Message::system("from history"));
        let result = run_text(&mut agent, "hi").await;
        assert_eq!(result, "ok");
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some(Some("from history".into()))
        );
    }

    struct CaptureTools {
        names: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl CaptureTools {
        fn new() -> (Self, Arc<Mutex<Vec<Vec<String>>>>) {
            let names = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    names: Arc::clone(&names),
                },
                names,
            )
        }
    }

    #[async_trait]
    impl Provider for CaptureTools {
        async fn complete(&self, req: &Request) -> LlmResult<Response> {
            self.names
                .lock()
                .unwrap()
                .push(req.tools.iter().map(|t| t.name.clone()).collect());
            Ok(text_response("ok"))
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> LlmResult<BoxStream<'static, LlmResult<LlmStreamEvent>>> {
            Err(ProviderError::Api {
                status: 500,
                body: "stream disabled in mock".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("capture-tools".into())
        }
    }

    fn agent_with_todo_write(provider: Box<dyn Provider>) -> Agent {
        Agent::new(
            router_with(provider),
            vec![Box::new(crate::tools::TodoWriteTool)],
        )
    }

    fn agent_with_file_and_todo(provider: Box<dyn Provider>, root: &std::path::Path) -> Agent {
        Agent::new(
            router_with(provider),
            vec![
                Box::new(FileReadTool::new(root)),
                Box::new(crate::tools::TodoWriteTool),
            ],
        )
        .with_max_iters(4)
    }

    #[tokio::test]
    async fn default_request_omits_todo_write_tool() {
        let (mock, names) = CaptureTools::new();
        let mut agent = agent_with_todo_write(Box::new(mock));
        run_text(&mut agent, "hi").await;
        let seen = names.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(
            !seen[0].iter().any(|n| n == "todo_write"),
            "Default must hide todo_write: {:?}",
            seen[0]
        );
    }

    #[tokio::test]
    async fn plan_request_includes_todo_write_tool() {
        let (mock, names) = CaptureTools::new();
        let mut agent = agent_with_todo_write(Box::new(mock));
        agent.set_mode(AgentMode::Plan);
        run_text(&mut agent, "hi").await;
        let seen = names.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].iter().any(|n| n == "todo_write"),
            "Plan must include todo_write: {:?}",
            seen[0]
        );
    }

    #[tokio::test]
    async fn todo_write_updates_sot_and_emits_todo_updated() {
        let todos = json!({"todos":[{"id":"a","content":"one","status":"in_progress"}]});
        let mock = MockProvider::new(vec![
            tool_response("c1", "todo_write", todos.clone()),
            text_response("done"),
        ]);
        let mut agent = agent_with_todo_write(Box::new(mock)).with_max_iters(4);
        let mut sink = VecEventSink::default();
        let result = run_with(&mut agent, "plan it", &turn_ctx(), &mut sink)
            .await
            .unwrap();
        assert_eq!(result.text(), "done");
        assert_eq!(agent.todos().items.len(), 1);
        assert_eq!(agent.todos().items[0].id, "a");
        assert!(agent.todo_written_this_turn());

        let events = sink.events;
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::TodosChanged { todos } if todos.items[0].id == "a"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Tool(ToolEvent::Finished {
                result: ToolResult::Success { output },
                ..
            }) if output.contains("1 todos")
        )));
    }

    #[tokio::test]
    async fn todo_write_error_leaves_sot_unchanged() {
        let mock = MockProvider::new(vec![
            tool_response(
                "c1",
                "todo_write",
                json!({"todos":[
                    {"id":"a","content":"one","status":"in_progress"},
                    {"id":"b","content":"two","status":"in_progress"}
                ]}),
            ),
            text_response("done"),
        ]);
        let mut agent = agent_with_todo_write(Box::new(mock)).with_max_iters(4);
        agent.set_todos(crate::todo::TodoList {
            items: vec![crate::todo::TodoItem {
                id: "keep".into(),
                content: "old".into(),
                status: crate::todo::TodoStatus::Pending,
            }],
        });
        let mut sink = VecEventSink::default();
        run_with(&mut agent, "bad write", &turn_ctx(), &mut sink)
            .await
            .unwrap();
        assert_eq!(agent.todos().items[0].id, "keep");
        assert!(!agent.todo_written_this_turn());

        let events = sink.events;
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Tool(ToolEvent::Finished {
                result: ToolResult::Failed { output: Some(output), .. },
                ..
            }) if output.starts_with("error: agent error: todo_write:")
        )));
    }

    struct CaptureRequests {
        seen: Arc<Mutex<Vec<Request>>>,
        responses: Mutex<VecDeque<Response>>,
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl CaptureRequests {
        fn new(responses: Vec<Response>) -> (Self, Arc<Mutex<Vec<Request>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    seen: Arc::clone(&seen),
                    responses: Mutex::new(responses.into()),
                    entered: Mutex::new(None),
                    release: Mutex::new(None),
                },
                seen,
            )
        }
    }

    #[async_trait]
    impl Provider for CaptureRequests {
        async fn complete(&self, req: &Request) -> LlmResult<Response> {
            self.seen.lock().unwrap().push(req.clone());
            if let Some(tx) = self.entered.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ProviderError::Api {
                    status: 500,
                    body: "no more mock responses".into(),
                })
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> LlmResult<BoxStream<'static, LlmResult<LlmStreamEvent>>> {
            Err(ProviderError::Api {
                status: 500,
                body: "stream disabled in mock".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("capture-requests".into())
        }
    }

    fn system_of(req: &Request) -> &str {
        req.system.as_deref().unwrap_or("")
    }

    fn tool_names(req: &Request) -> Vec<&str> {
        req.tools.iter().map(|t| t.name.as_str()).collect()
    }

    fn pending_item() -> crate::todo::TodoItem {
        crate::todo::TodoItem {
            id: "a".into(),
            content: "one".into(),
            status: crate::todo::TodoStatus::Pending,
        }
    }

    #[tokio::test]
    async fn plan_first_request_has_plan_prompt_and_todo_write() {
        let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
        let mut agent = agent_with_todo_write(Box::new(mock));
        agent.set_system("base");
        agent.set_mode(AgentMode::Plan);
        run_text(&mut agent, "hi").await;
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        assert!(system_of(&reqs[0]).contains("## Plan Mode"));
        assert!(system_of(&reqs[0]).contains("base"));
        assert!(!system_of(&reqs[0]).contains("## Current TODO list"));
        assert!(tool_names(&reqs[0]).contains(&"todo_write"));
    }

    #[tokio::test]
    async fn default_request_omits_plan_section_and_todo_write() {
        let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
        let mut agent = agent_with_todo_write(Box::new(mock));
        agent.set_system("base");
        run_text(&mut agent, "hi").await;
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        assert!(!system_of(&reqs[0]).contains("## Plan Mode"));
        assert!(!system_of(&reqs[0]).contains("## Plan reminder"));
        assert!(!tool_names(&reqs[0]).contains(&"todo_write"));
    }

    #[tokio::test]
    async fn default_still_injects_nonempty_list() {
        let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
        let mut agent = agent_with_todo_write(Box::new(mock));
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        run_text(&mut agent, "hi").await;
        let reqs = seen.lock().unwrap().clone();
        assert!(system_of(&reqs[0]).contains("## Current TODO list"));
        assert!(!system_of(&reqs[0]).contains("## Plan Mode"));
        assert!(!system_of(&reqs[0]).contains("## Plan reminder"));
        assert!(!tool_names(&reqs[0]).contains(&"todo_write"));
    }

    #[tokio::test]
    async fn in_flight_set_mode_applies_to_next_step() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (mock, seen) = CaptureRequests::new(vec![
            tool_response("c1", "file_read", json!({"path": "note.txt"})),
            text_response("done"),
        ]);
        *mock.entered.lock().unwrap() = Some(entered_tx);
        *mock.release.lock().unwrap() = Some(release_rx);

        let mut agent = agent_with_file_and_todo(Box::new(mock), tmp.path());
        assert_eq!(agent.mode(), AgentMode::Default);

        let mut sink = NullSink;
        let ctx = turn_ctx();
        let run = agent.run("read it", &ctx, &mut sink);
        tokio::pin!(run);
        tokio::select! {
            biased;
            _ = entered_rx => {}
            _ = &mut run => panic!("turn finished before first complete awaited"),
        }
        ctx.set_mode(AgentMode::Plan);
        drop(release_tx);
        assert_eq!(run.await.unwrap().text(), "done");

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        assert!(!system_of(&reqs[0]).contains("## Plan Mode"));
        assert!(!tool_names(&reqs[0]).contains(&"todo_write"));
        assert!(system_of(&reqs[1]).contains("## Plan Mode"));
        assert!(tool_names(&reqs[1]).contains(&"todo_write"));
    }

    #[tokio::test]
    async fn plan_tool_without_todo_write_injects_reminder() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();

        let (mock, seen) = CaptureRequests::new(vec![
            tool_response("c1", "file_read", json!({"path": "note.txt"})),
            text_response("done"),
        ]);
        let mut agent = agent_with_file_and_todo(Box::new(mock), tmp.path());
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        run_text(&mut agent, "read it").await;

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        assert!(system_of(&reqs[0]).contains("## Plan Mode"));
        assert!(!system_of(&reqs[0]).contains("## Plan reminder"));
        assert!(system_of(&reqs[1]).contains("## Plan reminder"));
        assert!(system_of(&reqs[1]).contains("## Current TODO list"));
        assert!(!agent.history().any(|m| content_has(m, "## Plan reminder")));
    }

    #[tokio::test]
    async fn reminder_clears_after_successful_todo_write() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();

        let todos = json!({"todos":[{"id":"a","content":"one","status":"completed"}]});
        let (mock, seen) = CaptureRequests::new(vec![
            tool_response("c1", "file_read", json!({"path": "note.txt"})),
            tool_response("c2", "todo_write", todos),
            text_response("done"),
        ]);
        let mut agent = agent_with_file_and_todo(Box::new(mock), tmp.path()).with_max_iters(6);
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        run_text(&mut agent, "do it").await;

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 3);
        assert!(!system_of(&reqs[0]).contains("## Plan reminder"));
        assert!(system_of(&reqs[1]).contains("## Plan reminder"));
        assert!(!system_of(&reqs[2]).contains("## Plan reminder"));
        assert!(system_of(&reqs[2]).contains("## Current TODO list"));
    }

    #[tokio::test]
    async fn plan_uses_history_system_when_no_base_system() {
        let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
        let mut agent = agent_with_todo_write(Box::new(mock));
        agent.push_history(Message::system("from history"));
        agent.set_mode(AgentMode::Plan);
        run_text(&mut agent, "hi").await;
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        assert!(system_of(&reqs[0]).contains("from history"));
        assert!(system_of(&reqs[0]).contains("## Plan Mode"));
        assert!(tool_names(&reqs[0]).contains(&"todo_write"));
    }

    #[tokio::test]
    async fn leaving_plan_keeps_list_drops_prompt_and_reminder() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();

        let (mock, seen) = CaptureRequests::new(vec![
            tool_response("c1", "file_read", json!({"path": "note.txt"})),
            text_response("done"),
            text_response("later"),
        ]);
        let mut agent = agent_with_file_and_todo(Box::new(mock), tmp.path());
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        run_text(&mut agent, "read it").await;
        agent.set_mode(AgentMode::Default);
        run_text(&mut agent, "next").await;

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 3);
        assert!(system_of(&reqs[1]).contains("## Plan reminder"));
        assert!(system_of(&reqs[2]).contains("## Current TODO list"));
        assert!(!system_of(&reqs[2]).contains("## Plan Mode"));
        assert!(!system_of(&reqs[2]).contains("## Plan reminder"));
        assert!(!tool_names(&reqs[2]).contains(&"todo_write"));
    }

    #[tokio::test]
    async fn rewind_clears_todo_dirty_reminder() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();

        let (mock, seen) = CaptureRequests::new(vec![
            tool_response("c1", "file_read", json!({"path": "note.txt"})),
            text_response("done"),
            text_response("again"),
        ]);
        let mut agent = agent_with_file_and_todo(Box::new(mock), tmp.path());
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        run_text(&mut agent, "read it").await;
        assert!(agent.rewind_last_turn().is_some());
        run_text(&mut agent, "again").await;

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 3);
        assert!(system_of(&reqs[1]).contains("## Plan reminder"));
        assert!(!system_of(&reqs[2]).contains("## Plan reminder"));
        assert!(system_of(&reqs[2]).contains("## Current TODO list"));
        assert!(system_of(&reqs[2]).contains("## Plan Mode"));
    }

    #[tokio::test]
    async fn clear_history_clears_todo_dirty() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();

        let (mock, seen) = CaptureRequests::new(vec![
            tool_response("c1", "file_read", json!({"path": "note.txt"})),
            text_response("done"),
            text_response("fresh"),
        ]);
        let mut agent = agent_with_file_and_todo(Box::new(mock), tmp.path());
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        run_text(&mut agent, "read it").await;
        agent.clear_history();
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        run_text(&mut agent, "fresh").await;

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 3);
        assert!(system_of(&reqs[1]).contains("## Plan reminder"));
        assert!(!system_of(&reqs[2]).contains("## Plan reminder"));
    }
}
