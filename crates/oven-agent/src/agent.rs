use futures::StreamExt;
use oven_llm::{
    ContentBlock, Delta, Message, ModelId, Provider, Request, Response, Role, SamplingParams,
    StreamCollector, StreamEvent, ToolChoice, Usage,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::event::{AgentEvent, AgentId};
use crate::history::History;
use crate::slash::CommandOutcome;
use crate::slash::SlashRegistry;
use crate::tools::Tool;

/// The conversation driver. Holds tools and dispatches tool calls returned by
/// the provider until the provider replies without tool calls.
pub struct Agent {
    id: AgentId,
    pub(crate) provider: Box<dyn Provider>,
    pub(crate) tools: Vec<Box<dyn Tool>>,
    pub(crate) slash: SlashRegistry,
    pub(crate) history: History,
    model: ModelId,
    system: Option<String>,
    max_iters: usize,
    /// Soft budget on conversation tokens; oldest turns are dropped to stay
    /// under it before each provider call.
    budget: usize,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Self {
        Self {
            id: AgentId(0),
            provider,
            tools,
            slash: SlashRegistry::with_builtin(),
            history: History::new(),
            model: ModelId::new("default"),
            system: None,
            max_iters: 100,
            budget: 128_000,
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

    pub fn with_system(mut self, content: impl Into<String>) -> Self {
        self.system = Some(content.into());
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

    /// Replace the slash registry with a custom one.
    pub fn with_slash(mut self, slash: SlashRegistry) -> Self {
        self.slash = slash;
        self
    }

    /// Names and descriptions of the registered slash commands.
    pub fn slash_commands(&self) -> Vec<(String, String)> {
        self.slash.commands()
    }

    pub fn history(&self) -> &[Message] {
        self.history.messages()
    }

    pub fn history_revision(&self) -> u64 {
        self.history.revision()
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Replace the entire conversation history. Useful for replaying a
    /// persisted session before resuming it.
    pub fn set_history(&mut self, history: Vec<Message>) {
        self.history.set_messages(history);
    }

    /// Append a message to the history. Used by the App layer to preload a
    /// persisted session.
    pub fn push_history(&mut self, message: Message) {
        self.history.push(message);
    }

    fn llm_tools(&self) -> Vec<oven_llm::Tool> {
        self.tools
            .iter()
            .map(|t| oven_llm::Tool {
                name: t.name().to_string(),
                description: Some(t.description().to_string()),
                input_schema: t.schema(),
            })
            .collect()
    }

    fn build_request(&self, tools: Vec<oven_llm::Tool>) -> Request {
        let mut system = self.system.clone();
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
        Request {
            model: self.model.clone(),
            system,
            messages,
            tools,
            tool_choice: ToolChoice::Auto,
            sampling: SamplingParams::default(),
            thinking: None,
            reasoning_effort: None,
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
        tools: Vec<oven_llm::Tool>,
        tx: &Option<UnboundedSender<AgentEvent>>,
    ) -> Result<Response, AgentError> {
        let req = self.build_request(tools);

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
        tools: Vec<oven_llm::Tool>,
        tx: &Option<UnboundedSender<AgentEvent>>,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<String>, AgentError> {
        let response = self.complete_response(tools, tx).await?;
        if let Some(usage) = &response.usage {
            self.history.record_usage(usage);
        }

        if !response.has_tool_use() {
            let text = response.text();
            self.history.push(Message::assistant(response.content));
            return Ok(Some(text));
        }

        self.history
            .push(Message::assistant(response.content.clone()));

        for block in response.tool_uses() {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            Self::emit(
                tx,
                AgentEvent::ToolStart {
                    agent_id: self.id,
                    call_id: id.clone(),
                    name: name.clone(),
                },
            );
            let (ok, result) = match self.dispatch(name, input, cancel).await {
                Ok(r) => (true, r),
                Err(e) => (false, format!("error: {e}")),
            };
            Self::emit(
                tx,
                AgentEvent::ToolEnd {
                    agent_id: self.id,
                    call_id: id.clone(),
                    ok,
                },
            );
            let summary = truncate(&result, 2000);
            self.history
                .push(Message::tool_result(id.clone(), summary, !ok));
        }
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
        let turn = async {
            // Move the slash registry out of `self` so commands can borrow
            // `self` mutably while executing (commands often touch
            // `agent.history` etc.).
            let registry = std::mem::take(&mut self.slash);
            let outcome = registry.parse_and_run(self, &input);
            self.slash = registry;
            match outcome? {
                CommandOutcome::Passthrough => {}
                CommandOutcome::Reply(r) => return Ok(finish(self, &tx, r)),
                CommandOutcome::Cleared => {
                    let text = finish(self, &tx, "history cleared".to_string());
                    Self::emit(&tx, AgentEvent::HistoryCleared { agent_id: self.id });
                    return Ok(text);
                }
                CommandOutcome::Exit => {
                    let text = finish(self, &tx, "goodbye".to_string());
                    Self::emit(&tx, AgentEvent::Exit { agent_id: self.id });
                    return Ok(text);
                }
            }

            self.history.push(Message::user_text(input));
            let tools = self.llm_tools();
            self.budget = self
                .provider
                .resolve_model(&self.model)
                .map(|info| info.context_window as usize)
                .unwrap_or(128_000);

            for _ in 0..self.max_iters {
                self.history.trim_to_budget(self.budget);
                match self.step(tools.clone(), &tx, cancel).await {
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
    use std::sync::Mutex;
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
                name
            } if call_id == "call_1" && name == "file_read"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolEnd {
                agent_id: AgentId(7),
                call_id,
                ok: true
            } if call_id == "call_1"
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

    #[tokio::test]
    async fn slash_clear_runs_without_model() {
        let mock = MockProvider::new(vec![]);
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let mut agent = Agent::new(Box::new(mock), tools);
        agent.history.push(Message::user_text("prior"));
        let out = agent.run("/clear").await.unwrap();
        assert_eq!(out, "history cleared");
        assert!(agent.history.is_empty());
    }

    #[tokio::test]
    async fn slash_exit_returns_goodbye() {
        let mock = MockProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(mock), Vec::new());
        let out = agent.run("/exit").await.unwrap();
        assert_eq!(out, "goodbye");
    }

    #[tokio::test]
    async fn slash_exit_emits_exit_event() {
        let mock = MockProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(mock), Vec::new()).with_id(AgentId(3));
        let (tx, mut rx) = unbounded_channel();
        let out = agent
            .run_with_emitter("/exit", Some(tx), None)
            .await
            .unwrap();
        assert_eq!(out, "goodbye");

        let mut saw_done = false;
        let mut saw_exit = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AgentEvent::Done {
                    agent_id: AgentId(3),
                    text,
                    ..
                } if text == "goodbye" => saw_done = true,
                AgentEvent::Exit {
                    agent_id: AgentId(3),
                } => saw_exit = true,
                _ => {}
            }
        }
        assert!(saw_done);
        assert!(saw_exit);
    }

    #[tokio::test]
    async fn slash_clear_emits_history_cleared_event() {
        let mock = MockProvider::new(vec![]);
        let mut agent = Agent::new(Box::new(mock), Vec::new()).with_id(AgentId(4));
        agent.history.push(Message::user_text("prior"));
        let (tx, mut rx) = unbounded_channel();
        let out = agent
            .run_with_emitter("/clear", Some(tx), None)
            .await
            .unwrap();
        assert_eq!(out, "history cleared");

        let mut saw_done = false;
        let mut saw_cleared = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AgentEvent::Done { text, .. } if text == "history cleared" => saw_done = true,
                AgentEvent::HistoryCleared { .. } => saw_cleared = true,
                _ => {}
            }
        }
        assert!(saw_done);
        assert!(saw_cleared);
        assert!(agent.history.is_empty());
    }

    #[tokio::test]
    async fn slash_clear_resets_total_usage() {
        let mock = MockProvider::new(vec![text_response("one")]);
        let mut agent = Agent::new(Box::new(mock), Vec::new());
        let out = agent.run("hello").await.unwrap();
        assert_eq!(out, "one");
        assert!(agent.total_usage().input_tokens > 0);

        let out = agent.run("/clear").await.unwrap();
        assert_eq!(out, "history cleared");
        assert_eq!(agent.total_usage().input_tokens, 0);
        assert_eq!(agent.total_usage().output_tokens, 0);
    }
}
