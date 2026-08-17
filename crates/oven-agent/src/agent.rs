use std::sync::{Arc, Mutex};

use futures::StreamExt;
use oven_llm::{
    ContentBlock, Delta, Message, ModelId, Provider, ReasoningEffort, Request, Response, Role,
    SamplingParams, StreamCollector, StreamEvent, ThinkingMode, ToolChoice, Usage,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::event::{AgentEvent, AgentId};
use crate::history::{History, Record};
use crate::live::{AgentLive, LiveHandle};
use crate::mode::AgentMode;
use crate::todo::{self, TodoList};
use crate::tools::Tool;

/// The conversation driver. Holds tools and dispatches tool calls returned by
/// the provider until the provider replies without tool calls.
pub struct Agent {
    id: AgentId,
    pub(crate) provider: Box<dyn Provider>,
    pub(crate) tools: Vec<Box<dyn Tool>>,
    pub(crate) history: History,
    model: ModelId,
    live: LiveHandle,
    reasoning_effort: Option<ReasoningEffort>,
    max_iters: usize,
    /// Soft budget on conversation tokens; oldest turns are dropped to stay
    /// under it before each provider call.
    budget: usize,
    todo_written_this_turn: bool,
    todo_dirty: bool,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Self {
        Self::new_with_live(provider, tools, Arc::new(Mutex::new(AgentLive::new(None))))
    }

    pub fn new_with_live(
        provider: Box<dyn Provider>,
        tools: Vec<Box<dyn Tool>>,
        live: LiveHandle,
    ) -> Self {
        Self {
            id: AgentId(0),
            provider,
            tools,
            history: History::new(),
            model: ModelId::new("default"),
            live,
            reasoning_effort: None,
            max_iters: 100,
            budget: 128_000,
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

    /// The provider in effect (used by the App layer for model listing).
    pub fn provider(&self) -> &dyn Provider {
        self.provider.as_ref()
    }

    pub fn set_model(&mut self, model: impl Into<ModelId>) {
        self.model = model.into();
    }

    pub fn set_provider(&mut self, provider: Box<dyn Provider>) {
        self.provider = provider;
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.reasoning_effort = effort;
    }

    pub fn live_handle(&self) -> LiveHandle {
        self.live.clone()
    }

    pub fn with_system(self, content: impl Into<String>) -> Self {
        self.set_system(content);
        self
    }

    pub fn set_system(&self, content: impl Into<String>) {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .base_system = Some(content.into());
    }

    pub fn set_mode(&self, mode: AgentMode) {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).mode = mode;
    }

    pub fn mode(&self) -> AgentMode {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).mode
    }

    pub fn set_todos(&self, todos: TodoList) {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).todos = todos;
    }

    pub fn todos(&self) -> TodoList {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .todos
            .clone()
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

    /// Set the soft token budget for conversation history.
    pub fn with_budget(mut self, budget: usize) -> Self {
        self.budget = budget;
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
        let mode = self.mode();
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
        let (mut system, todos, mode) = {
            let g = self.live.lock().unwrap_or_else(|e| e.into_inner());
            (g.base_system.clone(), g.todos.clone(), g.mode)
        };
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
                max_tokens: Some(4096),
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

    fn emit(tx: &Option<UnboundedSender<AgentEvent>>, event: AgentEvent) {
        if let Some(tx) = tx {
            let _ = tx.send(event);
        }
    }

    async fn dispatch(
        &self,
        name: &str,
        args: &serde_json::Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| AgentError::from(format!("unknown tool: {name}")))?;
        tool.run(args, cancel).await
    }

    async fn complete_response(
        &mut self,
        tx: &Option<UnboundedSender<AgentEvent>>,
    ) -> Result<Response, AgentError> {
        let req = self.build_request();

        match self.provider.stream(&req).await {
            Ok(mut stream) => {
                let mut collector = StreamCollector::new();
                while let Some(event) = stream.next().await {
                    match event {
                        Err(e) => return Err(e.into()),
                        Ok(event) => {
                            if let StreamEvent::ContentBlockDelta { delta, .. } = &event {
                                match delta {
                                    Delta::ThinkingDelta { thinking } if !thinking.is_empty() => {
                                        Self::emit(
                                            tx,
                                            AgentEvent::ThinkingDelta {
                                                agent_id: self.id,
                                                text: thinking.clone(),
                                            },
                                        );
                                    }
                                    Delta::TextDelta { text } if !text.is_empty() => {
                                        Self::emit(
                                            tx,
                                            AgentEvent::TextDelta {
                                                agent_id: self.id,
                                                text: text.clone(),
                                            },
                                        );
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
                let response = self.provider.complete(&req).await?;
                if !response.has_tool_use() {
                    let thinking = response.thinking();
                    if !thinking.is_empty() {
                        Self::emit(
                            tx,
                            AgentEvent::ThinkingDelta {
                                agent_id: self.id,
                                text: thinking,
                            },
                        );
                    }
                    let text = response.text();
                    if !text.is_empty() {
                        Self::emit(
                            tx,
                            AgentEvent::TextDelta {
                                agent_id: self.id,
                                text,
                            },
                        );
                    }
                }
                Ok(response)
            }
        }
    }

    async fn step(
        &mut self,
        tx: &Option<UnboundedSender<AgentEvent>>,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<String>, AgentError> {
        let response = self.complete_response(tx).await?;

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
            Self::emit(
                tx,
                AgentEvent::ToolStart {
                    agent_id: self.id,
                    call_id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    view,
                },
            );
            let (ok, result) = match self.dispatch(name, input, cancel).await {
                Ok(r) => (true, r),
                Err(e) => (false, format!("error: {e}")),
            };
            if ok && writes_todos {
                self.todo_written_this_turn = true;
                wrote_todo = true;
                Self::emit(
                    tx,
                    AgentEvent::TodoUpdated {
                        agent_id: self.id,
                        items: self.todos().items,
                    },
                );
            }
            let summary = truncate(&result, 1_500_000);
            Self::emit(
                tx,
                AgentEvent::ToolEnd {
                    agent_id: self.id,
                    call_id: id.clone(),
                    ok,
                    output: summary.clone(),
                },
            );
            self.history
                .push(Message::tool_result(id.clone(), summary, !ok));
        }
        self.todo_dirty = !wrote_todo;
        Ok(None)
    }

    /// Run one user turn, optionally streaming [`AgentEvent`]s and honoring
    /// cooperative cancellation via [`CancellationToken`].
    pub async fn run_with_emitter(
        &mut self,
        input: impl Into<String>,
        tx: Option<UnboundedSender<AgentEvent>>,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let input: String = input.into();
        let finish = |this: &Agent, tx: &Option<UnboundedSender<AgentEvent>>, text: String| {
            Self::emit(
                tx,
                AgentEvent::Done {
                    agent_id: this.id,
                    text: text.clone(),
                    usage: *this.total_usage(),
                },
            );
            text
        };

        // Race the whole turn against the cancellation token: a cancel landing
        // at any await point (streaming, tool work) drops the turn future and
        // aborts the in-flight provider stream or tool. The token is also
        // passed to tools so long-running ones can stop promptly on their own.
        self.todo_written_this_turn = false;
        let turn = async {
            self.history.push(Message::user_text(input));
            self.budget = self
                .provider
                .resolve_model(&self.model)
                .map(|info| info.context_window as usize)
                .unwrap_or(128_000);

            for _ in 0..self.max_iters {
                self.history.trim_to_budget(self.budget);
                match self.step(&tx, cancel).await {
                    Ok(Some(final_text)) => return Ok(finish(self, &tx, final_text)),
                    Ok(None) => continue,
                    Err(e) => return Err(e),
                }
            }
            Err(AgentError::from("agent loop exceeded max iterations"))
        };

        let result = match cancel {
            Some(token) => {
                tokio::pin!(turn);
                tokio::select! {
                    biased;
                    _ = token.cancelled() => Err(AgentError::cancelled()),
                    res = &mut turn => res,
                }
            }
            None => turn.await,
        };

        if result.as_ref().is_err_and(AgentError::is_cancelled) {
            Self::emit(&tx, AgentEvent::Cancelled { agent_id: self.id });
        }
        result
    }

    /// Run one user turn. Collects no events; equivalent to
    /// [`run_with_emitter`](Self::run_with_emitter) with no sink or cancel.
    pub async fn run(&mut self, input: impl Into<String>) -> Result<String, AgentError> {
        self.run_with_emitter(input, None, None).await
    }

    /// Last API-reported prompt-token count for the current conversation
    /// (0 before the first call or after a trim).
    pub fn prompt_tokens(&self) -> usize {
        self.history.prompt_tokens()
    }

    /// Cumulative token usage across all recorded responses.
    pub fn total_usage(&self) -> &Usage {
        self.history.total_usage()
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
    use crate::tools::{FileReadTool, FileWriteTool};
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{ModelInfo, ProviderError, ProviderName, Result as LlmResult, StopReason};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc::unbounded_channel;

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
        ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
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
        let mut agent = Agent::new(Box::new(mock), tools).with_max_iters(4);
        let result = agent.run("read note.txt").await.unwrap();
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
        let mut agent = Agent::new(Box::new(mock), tools)
            .with_id(AgentId(7))
            .with_max_iters(4);

        let (tx, mut rx) = unbounded_channel();
        let result = agent
            .run_with_emitter("read it", Some(tx), None)
            .await
            .unwrap();
        assert_eq!(result, "all good");

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolStart {
                agent_id: AgentId(7),
                call_id,
                name,
                input,
                view
            } if call_id == "call_1"
                && name == "file_read"
                && *input == json!({"path": "note.txt"})
                && view.summary == "Read note.txt"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolEnd {
                agent_id: AgentId(7),
                call_id,
                ok: true,
                output
            } if call_id == "call_1"
                && output == "hello world"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::TextDelta {
                agent_id: AgentId(7),
                text
            } if text == "all good"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Done {
                agent_id: AgentId(7),
                text,
                ..
            } if text == "all good"
        )));
    }

    #[tokio::test]
    async fn cancel_before_run_emits_cancelled() {
        let mock = MockProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(mock), Vec::new()).with_id(AgentId(1));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, mut rx) = unbounded_channel();
        let err = agent
            .run_with_emitter("hi", Some(tx), Some(&cancel))
            .await
            .unwrap_err();
        assert!(err.is_cancelled());
        let ev = rx.try_recv().unwrap();
        assert_eq!(
            ev,
            AgentEvent::Cancelled {
                agent_id: AgentId(1)
            }
        );
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
        ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
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
        let mut agent = Agent::new(Box::new(mock), Vec::new());
        agent.set_system("hello system");
        let result = agent.run("hi").await.unwrap();
        assert_eq!(result, "ok");
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some(Some("hello system".into()))
        );
    }

    #[tokio::test]
    async fn history_system_used_when_no_base_system() {
        let (mock, seen) = CaptureSystem::new();
        let mut agent = Agent::new(Box::new(mock), Vec::new());
        agent.push_history(Message::system("from history"));
        let result = agent.run("hi").await.unwrap();
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
        ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
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
        let live = Arc::new(Mutex::new(AgentLive::new(None)));
        let tools: Vec<Box<dyn Tool>> =
            vec![Box::new(crate::tools::TodoWriteTool::new(live.clone()))];
        Agent::new_with_live(provider, tools, live)
    }

    #[tokio::test]
    async fn default_request_omits_todo_write_tool() {
        let (mock, names) = CaptureTools::new();
        let mut agent = agent_with_todo_write(Box::new(mock));
        agent.run("hi").await.unwrap();
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
        agent.run("hi").await.unwrap();
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
        let (tx, mut rx) = unbounded_channel();
        let result = agent
            .run_with_emitter("plan it", Some(tx), None)
            .await
            .unwrap();
        assert_eq!(result, "done");
        assert_eq!(agent.todos().items.len(), 1);
        assert_eq!(agent.todos().items[0].id, "a");
        assert!(agent.todo_written_this_turn());

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::TodoUpdated { items, .. } if items.len() == 1 && items[0].id == "a"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolEnd { ok: true, output, .. } if output.contains("1 todos")
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
        let (tx, mut rx) = unbounded_channel();
        agent
            .run_with_emitter("bad write", Some(tx), None)
            .await
            .unwrap();
        assert_eq!(agent.todos().items[0].id, "keep");
        assert!(!agent.todo_written_this_turn());

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolEnd { ok: false, output, .. }
                if output.starts_with("error: agent error: todo_write:")
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TodoUpdated { .. }))
        );
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
        ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
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
        agent.run("hi").await.unwrap();
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
        agent.run("hi").await.unwrap();
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
        agent.run("hi").await.unwrap();
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

        let live = Arc::new(Mutex::new(AgentLive::new(None)));
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool::new(tmp.path())),
            Box::new(crate::tools::TodoWriteTool::new(live.clone())),
        ];
        let mut agent = Agent::new_with_live(Box::new(mock), tools, live).with_max_iters(4);
        assert_eq!(agent.mode(), AgentMode::Default);
        let live = agent.live_handle();

        let run = agent.run("read it");
        tokio::pin!(run);
        tokio::select! {
            biased;
            _ = entered_rx => {}
            _ = &mut run => panic!("turn finished before first complete awaited"),
        }
        live.lock().unwrap_or_else(|e| e.into_inner()).mode = AgentMode::Plan;
        drop(release_tx);
        assert_eq!(run.await.unwrap(), "done");

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
        let live = Arc::new(Mutex::new(AgentLive::new(None)));
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool::new(tmp.path())),
            Box::new(crate::tools::TodoWriteTool::new(live.clone())),
        ];
        let mut agent = Agent::new_with_live(Box::new(mock), tools, live).with_max_iters(4);
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        agent.run("read it").await.unwrap();

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
        let live = Arc::new(Mutex::new(AgentLive::new(None)));
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool::new(tmp.path())),
            Box::new(crate::tools::TodoWriteTool::new(live.clone())),
        ];
        let mut agent = Agent::new_with_live(Box::new(mock), tools, live).with_max_iters(6);
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        agent.run("do it").await.unwrap();

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
        agent.run("hi").await.unwrap();
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
        let live = Arc::new(Mutex::new(AgentLive::new(None)));
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool::new(tmp.path())),
            Box::new(crate::tools::TodoWriteTool::new(live.clone())),
        ];
        let mut agent = Agent::new_with_live(Box::new(mock), tools, live).with_max_iters(4);
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        agent.run("read it").await.unwrap();
        agent.set_mode(AgentMode::Default);
        agent.run("next").await.unwrap();

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
        let live = Arc::new(Mutex::new(AgentLive::new(None)));
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool::new(tmp.path())),
            Box::new(crate::tools::TodoWriteTool::new(live.clone())),
        ];
        let mut agent = Agent::new_with_live(Box::new(mock), tools, live).with_max_iters(4);
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        agent.run("read it").await.unwrap();
        assert!(agent.rewind_last_turn().is_some());
        agent.run("again").await.unwrap();

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
        let live = Arc::new(Mutex::new(AgentLive::new(None)));
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(FileReadTool::new(tmp.path())),
            Box::new(crate::tools::TodoWriteTool::new(live.clone())),
        ];
        let mut agent = Agent::new_with_live(Box::new(mock), tools, live).with_max_iters(4);
        agent.set_mode(AgentMode::Plan);
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        agent.run("read it").await.unwrap();
        agent.clear_history();
        agent.set_todos(crate::todo::TodoList {
            items: vec![pending_item()],
        });
        agent.run("fresh").await.unwrap();

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 3);
        assert!(system_of(&reqs[1]).contains("## Plan reminder"));
        assert!(!system_of(&reqs[2]).contains("## Plan reminder"));
    }
}
