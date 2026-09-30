use super::*;
use crate::RunPolicy;
use crate::StepStop;
use crate::TurnId;
use crate::capabilities::tools::{
    AnswerTool, BashTool, FileEditTool, FileReadTool, FileWriteTool, TodoWriteTool,
};
use crate::core::identity::ToolCallId;
use crate::core::sink::{NullSink, VecEventSink};
use crate::core::turn::TurnContext;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt, iter};
use oven_llm::{
    Delta, ModelInfo, ProviderError, ProviderName, Result as LlmResult, StopReason,
    StreamEvent as LlmStreamEvent,
};
use std::time::Duration;

use oven_llm::{ContentBlock, Message, Provider, Request, Response, Role, Usage};

use crate::core::error::AgentError;
use crate::core::event::{StreamEvent, ToolEvent, ToolResult, TurnEvent};
use crate::core::interaction::{ApprovalDecision, LoopLimitDecision};
use crate::core::turn::TurnOutput;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use tokio::sync::oneshot;

use crate::core::interaction::{AnswerResponse, PendingRequest, Question, UserRequest};

const APPROVED_FILE: &str = "approved.txt";
const APPROVED_CONTENT: &str = "approved";

fn write_approved_command() -> &'static str {
    #[cfg(windows)]
    {
        "Set-Content -Path approved.txt -Value approved -Encoding ascii -NoNewline"
    }
    #[cfg(not(windows))]
    {
        "printf approved > approved.txt"
    }
}

/// Bounds a test turn so a provider script that runs short fails on the
/// script rather than on an unbounded loop.
const TEST_MAX_ITERS: usize = 8;

fn turn_ctx(agent: &Agent) -> TurnContext {
    turn_ctx_with(agent, TEST_MAX_ITERS)
}

fn turn_ctx_with(agent: &Agent, max_iters: usize) -> TurnContext {
    TurnContext::new(TurnId::next(), CancellationToken::new(), agent.selection())
        .with_policy(RunPolicy::default().with_max_iters(max_iters))
}

async fn run_text(agent: &mut Agent, input: &str) -> String {
    let ctx = turn_ctx(agent);
    agent.run(input, &ctx, &mut NullSink).await.unwrap().text()
}

async fn run_plain(
    agent: &mut Agent,
    input: &str,
    sink: &mut impl EventSink,
) -> Result<TurnOutput, AgentError> {
    let ctx = turn_ctx(agent);
    agent.run(input, &ctx, sink).await
}

async fn run_with(
    agent: &mut Agent,
    input: &str,
    ctx: &TurnContext,
    sink: &mut impl EventSink,
) -> Result<TurnOutput, AgentError> {
    agent.run(input, ctx, sink).await
}

fn tool_approval(request: PendingRequest) -> (String, oneshot::Sender<ApprovalDecision>) {
    match request.request {
        UserRequest::ApproveTool {
            name, responder, ..
        } => (name, responder),
        _ => panic!("the turn must ask to approve a tool call"),
    }
}

fn loop_limit_prompt(request: PendingRequest) -> (usize, oneshot::Sender<LoopLimitDecision>) {
    match request.request {
        UserRequest::LoopLimit {
            max_iters,
            responder,
        } => (max_iters, responder),
        _ => panic!("the turn must ask whether it may keep going"),
    }
}

fn asked_question(request: PendingRequest) -> (Question, oneshot::Sender<AnswerResponse>) {
    match request.request {
        UserRequest::Question {
            question,
            responder,
        } => (question, responder),
        _ => panic!("the turn must ask a question"),
    }
}

fn is_terminal(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::Turn(
            TurnEvent::Completed { .. } | TurnEvent::Cancelled { .. } | TurnEvent::Failed { .. }
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

    let mut open_step = 0;
    for event in events {
        match event {
            AgentEvent::Turn(TurnEvent::StepStarted { index }) => {
                assert_eq!(*index, open_step + 1, "steps count from 1: {events:?}");
                open_step = *index;
            }
            AgentEvent::Turn(TurnEvent::StepFinished { index, .. }) => {
                assert_eq!(*index, open_step, "StepFinished must match its step");
            }
            _ => {}
        }
    }
    assert!(
        is_terminal(events.last().unwrap()),
        "last event must be terminal: {events:?}"
    );

    if let Some(done) = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Stream(StreamEvent::ThinkingDone { .. })))
    {
        assert!(
            !events[..done].iter().any(|e| matches!(
                e,
                AgentEvent::Stream(StreamEvent::TextDelta { .. })
                    | AgentEvent::Tool(ToolEvent::Started { .. })
            )),
            "the thinking window must close before the answer or tool that ended it: {events:?}"
        );
    }

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

struct SlowComplete {
    response: Response,
    delay: Duration,
}

impl SlowComplete {
    fn new(response: Response, delay: Duration) -> Self {
        Self { response, delay }
    }
}

#[async_trait]
impl Provider for SlowComplete {
    async fn complete(&self, _req: &Request) -> LlmResult<Response> {
        tokio::time::sleep(self.delay).await;
        Ok(self.response.clone())
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
        ProviderName::Custom("slow".into())
    }
}

/// Replays deltas as a provider stream, waiting `delay` between deltas so
/// tests can drive a thinking window of a known length.
struct ScriptedStream {
    deltas: Vec<Delta>,
    delay: Duration,
}

#[async_trait]
impl Provider for ScriptedStream {
    async fn complete(&self, _req: &Request) -> LlmResult<Response> {
        Err(ProviderError::Api {
            status: 500,
            body: "complete disabled in mock".into(),
        })
    }

    async fn stream(
        &self,
        _req: &Request,
    ) -> LlmResult<BoxStream<'static, LlmResult<LlmStreamEvent>>> {
        let mut events = vec![Ok(message_start())];
        for (index, delta) in self.deltas.iter().enumerate() {
            events.push(Ok(LlmStreamEvent::ContentBlockStart {
                index,
                block: delta_block(delta),
            }));
            events.push(Ok(LlmStreamEvent::ContentBlockDelta {
                index,
                delta: delta.clone(),
            }));
        }
        events.push(Ok(LlmStreamEvent::MessageStop));
        let delay = self.delay;
        Ok(Box::pin(iter(events).then(move |event| async move {
            tokio::time::sleep(delay).await;
            event
        })))
    }

    fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
        None
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom("scripted".into())
    }
}

fn delta_block(delta: &Delta) -> ContentBlock {
    const ACCUMULATED_LATER: &str = "";
    match delta {
        Delta::ThinkingDelta { .. } => ContentBlock::thinking(ACCUMULATED_LATER),
        Delta::TextDelta { .. } => ContentBlock::text(ACCUMULATED_LATER),
        Delta::InputJsonDelta { .. } => ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "bash".into(),
            input: json!({}),
            raw_arguments: None,
        },
    }
}

fn message_start() -> LlmStreamEvent {
    const MESSAGE_ID: &str = "scripted";
    LlmStreamEvent::MessageStart {
        id: MESSAGE_ID.into(),
        model: MESSAGE_ID.into(),
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
            raw_arguments: None,
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

fn thinking_response(thinking: &str, text: &str) -> Response {
    let mut response = text_response(text);
    response.content.insert(0, ContentBlock::thinking(thinking));
    response
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

    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FileReadTool::new(root)),
        Arc::new(FileWriteTool::new(root)),
    ];
    let mut agent = Agent::new(router_with(Box::new(mock)), tools);
    let result = run_text(&mut agent, "read note.txt").await;
    assert_eq!(result, "done");
    assert!(agent.history.iter().any(|m| content_has(m, "hello world")));
}

#[tokio::test]
async fn ask_rejects_hidden_write_tool_without_writing() {
    let tmp = tmp_dir();
    let mock = MockProvider::new(vec![
        tool_response(
            "call_1",
            "file_write",
            json!({"path": "created.txt", "content": "unexpected"}),
        ),
        text_response("explained"),
    ]);
    let mut agent = Agent::new(
        router_with(Box::new(mock)),
        vec![Arc::new(FileWriteTool::new(tmp.path()))],
    );
    agent.set_mode(AgentMode::Ask);

    assert_eq!(run_text(&mut agent, "write it").await, "explained");
    assert!(!tmp.path().join("created.txt").exists());
    assert!(
        agent
            .history
            .iter()
            .any(|message| content_has(message, "unavailable in Ask mode"))
    );
}

#[tokio::test]
async fn ask_requires_approval_before_running_bash() {
    let tmp = tmp_dir();
    let marker = tmp.path().join(APPROVED_FILE);
    let command = write_approved_command();
    let mock = MockProvider::new(vec![
        tool_response("call_1", "bash", json!({"command": command})),
        text_response("done"),
    ]);
    let mut agent = Agent::new(
        router_with(Box::new(mock)),
        vec![Arc::new(BashTool::new(tmp.path()))],
    );
    agent.set_mode(AgentMode::Ask);
    let (requests, mut asked) = tokio::sync::mpsc::unbounded_channel();
    let ctx = turn_ctx(&agent).with_requests(Arc::new(requests));
    let mut sink = NullSink;
    let turn = agent.run("run it", &ctx, &mut sink);
    tokio::pin!(turn);

    let request = tokio::select! {
        request = asked.recv() => request.unwrap(),
        _ = &mut turn => panic!("turn completed before requesting approval"),
    };
    let (name, responder) = tool_approval(request);
    assert_eq!(name, "bash");
    assert!(!marker.exists());
    responder.send(ApprovalDecision::Approved).unwrap();

    assert_eq!(turn.await.unwrap().text(), "done");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), APPROVED_CONTENT);
}

#[tokio::test]
async fn the_answer_tool_blocks_the_turn_until_the_user_replies() {
    const QUESTION: &str = "which database?";
    const ANSWER: &str = "postgres";
    let mock = MockProvider::new(vec![
        tool_response(
            "call_1",
            "answer",
            json!({
                "question": QUESTION,
                "options": [{ "label": ANSWER }, { "label": "sqlite" }]
            }),
        ),
        text_response("done"),
    ]);
    let mut agent = Agent::new(router_with(Box::new(mock)), vec![Arc::new(AnswerTool)]);
    let (requests, mut asked) = tokio::sync::mpsc::unbounded_channel();
    let ctx = turn_ctx(&agent).with_requests(Arc::new(requests));
    let mut sink = VecEventSink::default();
    let reply = {
        let turn = agent.run("pick one", &ctx, &mut sink);
        tokio::pin!(turn);

        let request = tokio::select! {
            request = asked.recv() => request.unwrap(),
            _ = &mut turn => panic!("turn completed before asking the user"),
        };
        let (question, responder) = asked_question(request);
        assert_eq!(question.question, QUESTION);
        assert_eq!(question.options.len(), 2);
        responder
            .send(AnswerResponse::Answered {
                answer: ANSWER.into(),
            })
            .unwrap();

        turn.await.unwrap().text()
    };

    assert_eq!(reply, "done");
    assert!(
        agent.history().any(|message| content_has(message, ANSWER)),
        "the answer must reach the model"
    );
    let detail = sink
        .events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Tool(ToolEvent::Finished { detail, .. }) => detail.clone(),
            _ => None,
        })
        .expect("the landing event reports what the call shows");
    assert_eq!(detail, ANSWER, "the row shows the answer the user picked");
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

    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(FileReadTool::new(root))];
    let mut agent = Agent::new(router_with(Box::new(mock)), tools).with_id(AgentId(7));

    let mut sink = VecEventSink::default();
    let result = run_plain(&mut agent, "read it", &mut sink).await.unwrap();
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
    let steps: Vec<(usize, StepStop)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Turn(TurnEvent::StepFinished { index, stop }) => Some((*index, *stop)),
            _ => None,
        })
        .collect();
    assert_eq!(
        steps,
        [(1, StepStop::ToolUse), (2, StepStop::FinalAnswer)],
        "one step per provider response"
    );
}

/// A response with several calls, which is how the model asks for work
/// to be done at the same time.
fn calls_response(calls: &[(&str, &str)]) -> Response {
    Response {
        content: calls
            .iter()
            .map(|(id, name)| ContentBlock::ToolUse {
                id: (*id).into(),
                name: (*name).into(),
                input: json!({}),
                raw_arguments: None,
            })
            .collect(),
        ..tool_response(calls[0].0, calls[0].1, json!({}))
    }
}

/// Finishes only once the other one has: whichever runs second lets the
/// first one go.
struct PairedTool {
    name: &'static str,
    signal: Arc<tokio::sync::Notify>,
    waits: bool,
}

#[async_trait]
impl Tool for PairedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "test tool"
    }

    fn schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    async fn run(
        &self,
        _args: &serde_json::Value,
        _cx: &TurnContext,
    ) -> Result<String, AgentError> {
        match self.waits {
            true => {
                self.signal.notified().await;
                Ok(format!("{} last", self.name))
            }
            false => {
                self.signal.notify_one();
                Ok(format!("{} first", self.name))
            }
        }
    }
}

/// Records what it is doing so a test can tell whether two calls of one
/// step overlapped.
struct ExclusiveTool {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
    exclusive: bool,
}

#[async_trait]
impl Tool for ExclusiveTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "test tool"
    }

    fn schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn caps(&self) -> crate::capabilities::tools::ToolCaps {
        crate::capabilities::tools::ToolCaps {
            exclusive: self.exclusive,
            ..Default::default()
        }
    }

    async fn run(
        &self,
        _args: &serde_json::Value,
        _cx: &TurnContext,
    ) -> Result<String, AgentError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("enter {}", self.name));
        tokio::task::yield_now().await;
        self.log
            .lock()
            .unwrap()
            .push(format!("leave {}", self.name));
        Ok(self.name.to_string())
    }
}

/// A tool that rewrites a file from what it read cannot run beside
/// another: two edits to one file would lose one of them. Everything else
/// runs at the same time, which is what makes several subagents at once
/// work.
#[tokio::test]
async fn exclusive_tools_take_turns_and_the_rest_overlap() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ExclusiveTool {
            name: "write_a",
            log: Arc::clone(&log),
            exclusive: true,
        }),
        Arc::new(ExclusiveTool {
            name: "write_b",
            log: Arc::clone(&log),
            exclusive: true,
        }),
    ];
    let mock = MockProvider::new(vec![
        calls_response(&[("c1", "write_a"), ("c2", "write_b")]),
        text_response("done"),
    ]);
    let mut agent = Agent::new(router_with(Box::new(mock)), tools);
    run_plain(&mut agent, "go", &mut VecEventSink::default())
        .await
        .unwrap();
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [
            "enter write_a",
            "leave write_a",
            "enter write_b",
            "leave write_b"
        ],
        "exclusive calls must not overlap"
    );

    let log = Arc::new(Mutex::new(Vec::new()));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ExclusiveTool {
            name: "read_a",
            log: Arc::clone(&log),
            exclusive: false,
        }),
        Arc::new(ExclusiveTool {
            name: "read_b",
            log: Arc::clone(&log),
            exclusive: false,
        }),
    ];
    let mock = MockProvider::new(vec![
        calls_response(&[("c1", "read_a"), ("c2", "read_b")]),
        text_response("done"),
    ]);
    let mut agent = Agent::new(router_with(Box::new(mock)), tools);
    run_plain(&mut agent, "go", &mut VecEventSink::default())
        .await
        .unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 4, "{log:?}");
    assert!(
        log[0].starts_with("enter") && log[1].starts_with("enter"),
        "both calls must be in flight before either leaves: {log:?}"
    );
}

/// Calls finish in whatever order they finish, but a provider expects one
/// tool result per call in the order it asked, so the history has to put
/// them back in that order.
#[tokio::test]
async fn tool_results_keep_the_order_the_model_asked_for() {
    let signal = Arc::new(tokio::sync::Notify::new());
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(PairedTool {
            name: "slow",
            signal: Arc::clone(&signal),
            waits: true,
        }),
        Arc::new(PairedTool {
            name: "fast",
            signal,
            waits: false,
        }),
    ];
    let mock = MockProvider::new(vec![
        calls_response(&[("c1", "slow"), ("c2", "fast")]),
        text_response("done"),
    ]);
    let mut agent = Agent::new(router_with(Box::new(mock)), tools);
    let mut sink = VecEventSink::default();
    run_plain(&mut agent, "go", &mut sink).await.unwrap();

    let started: Vec<ToolCallId> = sink
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Tool(ToolEvent::Started { call_id, .. }) => Some(*call_id),
            _ => None,
        })
        .collect();
    let finished: Vec<ToolCallId> = sink
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Tool(ToolEvent::Finished { call_id, .. }) => Some(*call_id),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 2, "both calls started");
    assert_eq!(finished.len(), 2, "both calls finished");
    assert_eq!(finished[0], started[1], "the second call landed first");

    let results: Vec<String> = agent
        .history()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        ["slow last", "fast first"],
        "history keeps the order the model asked for"
    );
}

/// A call to a tool that is not mounted still owes the provider a tool
/// result, and it owes it in the order the calls were asked for. Losing it
/// would desync the provider's tool ids for the whole conversation.
#[tokio::test]
async fn an_unmounted_tool_still_gets_a_result() {
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(PairedTool {
        name: "known",
        signal: Arc::new(tokio::sync::Notify::new()),
        waits: false,
    })];
    let mock = MockProvider::new(vec![
        calls_response(&[("c1", "known"), ("c2", "invented")]),
        text_response("done"),
    ]);
    let mut agent = Agent::new(router_with(Box::new(mock)), tools);
    run_plain(&mut agent, "go", &mut VecEventSink::default())
        .await
        .unwrap();

    let results: Vec<String> = agent
        .history()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        ["known first", "unknown tool: invented"],
        "one result per call, in the order the model asked"
    );
}

#[tokio::test]
async fn cancel_before_run_emits_cancelled() {
    let mock = MockProvider::new(vec![]);
    let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new()).with_id(AgentId(1));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut sink = VecEventSink::default();
    let ctx = TurnContext::new(TurnId::next(), cancel, agent.selection());
    let err = agent.run("hi", &ctx, &mut sink).await.unwrap_err();
    assert!(err.is_cancelled());
    assert_valid_event_sequence(&sink.events);
    assert_eq!(
        sink.events,
        vec![
            AgentEvent::Turn(TurnEvent::Started),
            AgentEvent::Turn(TurnEvent::Cancelled { duration_ms: 0 }),
        ]
    );
}

#[tokio::test]
async fn successful_turn_has_valid_lifecycle() {
    let mock = MockProvider::new(vec![text_response("ok")]);
    let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
    let mut sink = VecEventSink::default();
    let out = run_plain(&mut agent, "hi", &mut sink).await.unwrap();
    assert_eq!(out.text(), "ok");
    assert_valid_event_sequence(&sink.events);
    assert!(matches!(
        sink.events.last(),
        Some(AgentEvent::Turn(TurnEvent::Completed { .. }))
    ));
}

#[tokio::test]
async fn completed_duration_ms_matches_history_timestamps() {
    let mock = MockProvider::new(vec![text_response("ok")]);
    let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
    let mut sink = VecEventSink::default();
    run_plain(&mut agent, "hi", &mut sink).await.unwrap();
    let Some(AgentEvent::Turn(TurnEvent::Completed { duration_ms, .. })) = sink.events.last()
    else {
        panic!("expected Completed");
    };
    let timed: Vec<(Role, u64)> = agent
        .history_timed()
        .filter(|(m, _, _)| m.role != Role::System)
        .map(|(m, ts, _)| (m.role, ts))
        .collect();
    assert_eq!(timed.len(), 2);
    let persisted = timed[1].1.saturating_sub(timed[0].1);
    const DURATION_SLACK_MS: u64 = 5_000;
    assert!(
        *duration_ms >= persisted,
        "duration_ms={duration_ms} persisted={persisted}"
    );
    assert!(
        duration_ms.saturating_sub(persisted) < DURATION_SLACK_MS,
        "duration_ms={duration_ms} persisted={persisted}"
    );
}

#[tokio::test]
async fn fallback_complete_times_thinking_and_streams_it() {
    const THINKING: &str = "weighing options";
    const ANSWER: &str = "done";
    const COMPLETE_DELAY_MS: u64 = 20;
    const MIN_THINKING_MS: u64 = 1;
    let provider = SlowComplete::new(
        thinking_response(THINKING, ANSWER),
        Duration::from_millis(COMPLETE_DELAY_MS),
    );
    let mut agent = Agent::new(router_with(Box::new(provider)), Vec::new());
    let mut sink = VecEventSink::default();
    let out = run_plain(&mut agent, "hi", &mut sink).await.unwrap();
    assert_eq!(out.text(), ANSWER);
    assert_valid_event_sequence(&sink.events);
    let reported = sink.events.iter().find_map(|event| match event {
        AgentEvent::Stream(StreamEvent::ThinkingDone { duration_ms }) => Some(*duration_ms),
        _ => None,
    });
    let recorded = agent
        .history_timed()
        .filter(|(m, _, _)| m.role == Role::Assistant)
        .find_map(|(_, _, thinking)| thinking);
    assert_eq!(
        reported, recorded,
        "the transcript and the persisted thinking span must agree"
    );
    assert!(
        reported.is_some_and(|ms| ms >= MIN_THINKING_MS),
        "expected timed thinking, got {reported:?}"
    );
    let thinking_deltas: Vec<&str> = sink
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Stream(StreamEvent::ThinkingDelta { text }) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking_deltas, [THINKING]);
}

#[tokio::test]
async fn streamed_thinking_reports_the_same_span_as_it_persists() {
    const THINKING: &str = "weighing options";
    const ANSWER: &str = "done";
    const GAP_MS: u64 = 10;
    let provider = ScriptedStream {
        deltas: vec![
            Delta::ThinkingDelta {
                thinking: THINKING.into(),
            },
            Delta::TextDelta {
                text: ANSWER.into(),
            },
        ],
        delay: Duration::from_millis(GAP_MS),
    };
    let mut agent = Agent::new(router_with(Box::new(provider)), Vec::new());
    let mut sink = VecEventSink::default();
    let out = run_plain(&mut agent, "hi", &mut sink).await.unwrap();
    assert_eq!(out.text(), ANSWER);
    assert_valid_event_sequence(&sink.events);
    let reported = sink.events.iter().find_map(|event| match event {
        AgentEvent::Stream(StreamEvent::ThinkingDone { duration_ms }) => Some(*duration_ms),
        _ => None,
    });
    let recorded = agent
        .history_timed()
        .filter(|(m, _, _)| m.role == Role::Assistant)
        .find_map(|(_, _, thinking)| thinking);
    assert_eq!(reported, recorded);
    assert!(reported.is_some_and(|ms| ms >= GAP_MS));
}

#[tokio::test]
async fn thinking_span_closes_before_the_tool_it_led_to() {
    const THINKING: &str = "weighing options";
    let todos = json!({"todos":[{"id":"a","content":"one","status":"in_progress"}]});
    let mut call = tool_response("c1", "todo_write", todos);
    call.content.insert(0, ContentBlock::thinking(THINKING));
    let mock = MockProvider::new(vec![call, text_response("done")]);
    let mut agent = agent_with_todo_write(Box::new(mock));
    let mut sink = VecEventSink::default();
    run_plain(&mut agent, "plan it", &mut sink).await.unwrap();

    assert_valid_event_sequence(&sink.events);
    assert!(
        sink.events
            .iter()
            .any(|e| matches!(e, AgentEvent::Tool(ToolEvent::Started { .. }))),
        "the tool must have run: {:?}",
        sink.events
    );
    assert!(
        sink.events
            .iter()
            .any(|e| matches!(e, AgentEvent::Stream(StreamEvent::ThinkingDone { .. }))),
        "the reasoning phase must have been reported: {:?}",
        sink.events
    );
}

#[tokio::test]
async fn untimed_stream_emits_no_thinking_done() {
    let provider = ScriptedStream {
        deltas: vec![Delta::TextDelta {
            text: "done".into(),
        }],
        delay: Duration::ZERO,
    };
    let mut agent = Agent::new(router_with(Box::new(provider)), Vec::new());
    let mut sink = VecEventSink::default();
    run_plain(&mut agent, "hi", &mut sink).await.unwrap();
    assert_valid_event_sequence(&sink.events);
    assert!(
        !sink
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::Stream(StreamEvent::ThinkingDone { .. }))),
        "a response without thinking must not report a thinking window"
    );
}

#[tokio::test]
async fn completed_usage_is_the_last_turn_not_session_total() {
    let mock = MockProvider::new(vec![text_response("one"), text_response("two")]);
    let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
    let mut sink = VecEventSink::default();
    let out1 = run_plain(&mut agent, "first", &mut sink).await.unwrap();
    assert_eq!(out1.usage.input_tokens, 10);

    sink.events.clear();
    let out2 = run_plain(&mut agent, "second", &mut sink).await.unwrap();
    assert_eq!(out2.usage.input_tokens, 10);
    assert_eq!(out2.usage.output_tokens, 5);
    assert_eq!(agent.last_turn_usage().input_tokens, 10);
    match sink.events.last() {
        Some(AgentEvent::Turn(TurnEvent::Completed { usage, .. })) => {
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 5);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn usage_is_reported_as_each_response_arrives() {
    let tmp = tmp_dir();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let mock = MockProvider::new(vec![
        tool_response("call_1", "file_read", json!({"path": "note.txt"})),
        text_response("done"),
    ]);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(FileReadTool::new(tmp.path()))];
    let mut agent = Agent::new(router_with(Box::new(mock)), tools);
    let mut sink = VecEventSink::default();
    run_plain(&mut agent, "read note.txt", &mut sink)
        .await
        .unwrap();
    assert_valid_event_sequence(&sink.events);

    let reported: Vec<Usage> = sink
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Usage { usage } => Some(*usage),
            _ => None,
        })
        .collect();
    assert_eq!(reported.len(), 2, "one report per provider response");
    assert!(reported.iter().all(|usage| usage.input_tokens == 10));

    let last_usage = sink
        .events
        .iter()
        .rposition(|event| matches!(event, AgentEvent::Usage { .. }));
    let completed = sink.events.iter().position(is_terminal);
    assert!(
        last_usage < completed,
        "usage must be reported before the turn completes: {last_usage:?} vs {completed:?}"
    );
}

#[tokio::test]
async fn cancelled_turn_has_one_terminal_event() {
    let mock = MockProvider::new(vec![]);
    let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut sink = VecEventSink::default();
    let ctx = TurnContext::new(TurnId::next(), cancel, agent.selection());
    let err = agent.run("hi", &ctx, &mut sink).await.unwrap_err();
    assert!(err.is_cancelled());
    assert_valid_event_sequence(&sink.events);
    assert!(matches!(
        sink.events.last(),
        Some(AgentEvent::Turn(TurnEvent::Cancelled { .. }))
    ));
}

#[tokio::test]
async fn failed_turn_has_one_terminal_event() {
    let mock = MockProvider::new(vec![]);
    let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
    let mut sink = VecEventSink::default();
    let err = run_plain(&mut agent, "hi", &mut sink).await.unwrap_err();
    assert!(!err.is_cancelled());
    assert_valid_event_sequence(&sink.events);
    assert!(matches!(
        sink.events.last(),
        Some(AgentEvent::Turn(TurnEvent::Failed { .. }))
    ));
}

fn looping_read_agent(responses: Vec<Response>) -> (Agent, tempdir::TempDir) {
    let tmp = tmp_dir();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(FileReadTool::new(tmp.path()))];
    let agent = Agent::new(router_with(Box::new(MockProvider::new(responses))), tools);
    (agent, tmp)
}

fn read_note(id: &str) -> Response {
    tool_response(id, "file_read", json!({"path": "note.txt"}))
}

#[tokio::test]
async fn loop_limit_without_sender_fails() {
    let (mut agent, _tmp) = looping_read_agent(vec![
        read_note("c1"),
        read_note("c2"),
        read_note("c3"),
        text_response("done"),
    ]);
    let mut sink = VecEventSink::default();
    let ctx = turn_ctx_with(&agent, 2);
    let err = run_with(&mut agent, "read it", &ctx, &mut sink)
        .await
        .unwrap_err();
    assert_eq!(err.message, crate::MAX_ITERS_EXCEEDED);
    assert_valid_event_sequence(&sink.events);
    assert!(matches!(
        sink.events.last(),
        Some(AgentEvent::Turn(TurnEvent::Failed { error, .. }))
            if error.message == crate::MAX_ITERS_EXCEEDED
    ));
}

#[tokio::test]
async fn loop_limit_continue_runs_another_round() {
    let (mut agent, _tmp) = looping_read_agent(vec![
        read_note("c1"),
        read_note("c2"),
        read_note("c3"),
        text_response("done"),
    ]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = turn_ctx_with(&agent, 2).with_requests(Arc::new(tx));
    let mut sink = VecEventSink::default();
    let text = {
        let turn = agent.run("read it", &ctx, &mut sink);
        tokio::pin!(turn);

        let request = tokio::select! {
            request = rx.recv() => request.unwrap(),
            _ = &mut turn => panic!("turn completed before loop limit prompt"),
        };
        let (max_iters, responder) = loop_limit_prompt(request);
        assert_eq!(max_iters, 2);
        responder.send(LoopLimitDecision::Continue).unwrap();
        turn.await.unwrap().text()
    };
    assert_eq!(text, "done");
    assert_valid_event_sequence(&sink.events);
    assert!(matches!(
        sink.events.last(),
        Some(AgentEvent::Turn(TurnEvent::Completed { .. }))
    ));
}

#[tokio::test]
async fn loop_limit_exit_fails() {
    let (mut agent, _tmp) = looping_read_agent(vec![
        read_note("c1"),
        read_note("c2"),
        read_note("c3"),
        text_response("done"),
    ]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = turn_ctx_with(&agent, 2).with_requests(Arc::new(tx));
    let mut sink = VecEventSink::default();
    let err = {
        let turn = agent.run("read it", &ctx, &mut sink);
        tokio::pin!(turn);

        let request = tokio::select! {
            request = rx.recv() => request.unwrap(),
            _ = &mut turn => panic!("turn completed before loop limit prompt"),
        };
        let (_, responder) = loop_limit_prompt(request);
        responder.send(LoopLimitDecision::Exit).unwrap();
        turn.await.unwrap_err()
    };
    assert_eq!(err.message, crate::MAX_ITERS_EXCEEDED);
    assert_valid_event_sequence(&sink.events);
    assert!(matches!(
        sink.events.last(),
        Some(AgentEvent::Turn(TurnEvent::Failed { error, .. }))
            if error.message == crate::MAX_ITERS_EXCEEDED
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
    let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new()).with_system("hello system");
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
        vec![Arc::new(crate::capabilities::tools::TodoWriteTool)],
    )
}

fn agent_with_file_and_todo(provider: Box<dyn Provider>, root: &std::path::Path) -> Agent {
    Agent::new(
        router_with(provider),
        vec![
            Arc::new(FileReadTool::new(root)),
            Arc::new(crate::capabilities::tools::TodoWriteTool),
        ],
    )
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
async fn ask_request_hides_write_tools_but_keeps_bash() {
    let tmp = tmp_dir();
    let (mock, names) = CaptureTools::new();
    let mut agent = Agent::new(
        router_with(Box::new(mock)),
        vec![
            Arc::new(FileReadTool::new(tmp.path())),
            Arc::new(FileEditTool::new(tmp.path())),
            Arc::new(FileWriteTool::new(tmp.path())),
            Arc::new(BashTool::new(tmp.path())),
            Arc::new(TodoWriteTool),
        ],
    );
    agent.set_mode(AgentMode::Ask);
    run_text(&mut agent, "hi").await;
    let names = names.lock().unwrap().clone();
    assert_eq!(names.len(), 1);
    assert!(names[0].iter().any(|name| name == "file_read"));
    assert!(names[0].iter().any(|name| name == "bash"));
    assert!(!names[0].iter().any(|name| name == "file_edit"));
    assert!(!names[0].iter().any(|name| name == "file_write"));
    assert!(!names[0].iter().any(|name| name == "todo_write"));
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
    let mut agent = agent_with_todo_write(Box::new(mock));
    let mut sink = VecEventSink::default();
    let result = run_plain(&mut agent, "plan it", &mut sink).await.unwrap();
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
    let mut agent = agent_with_todo_write(Box::new(mock));
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![crate::core::todo::TodoItem {
            id: "keep".into(),
            content: "old".into(),
            status: crate::core::todo::TodoStatus::Pending,
        }],
    });
    let mut sink = VecEventSink::default();
    run_plain(&mut agent, "bad write", &mut sink).await.unwrap();
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

fn pending_item() -> crate::core::todo::TodoItem {
    crate::core::todo::TodoItem {
        id: "a".into(),
        content: "one".into(),
        status: crate::core::todo::TodoStatus::Pending,
    }
}

fn completed_item() -> crate::core::todo::TodoItem {
    crate::core::todo::TodoItem {
        id: "a".into(),
        content: "one".into(),
        status: crate::core::todo::TodoStatus::Completed,
    }
}

#[tokio::test]
async fn plan_first_request_has_plan_prompt_and_todo_write() {
    let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
    let mut agent = agent_with_todo_write(Box::new(mock)).with_system("base");
    agent.set_mode(AgentMode::Plan);
    run_text(&mut agent, "hi").await;
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert!(system_of(&reqs[0]).contains("# Plan Mode"));
    assert!(system_of(&reqs[0]).contains("base"));
    assert!(!system_of(&reqs[0]).contains("## Current TODO list"));
    assert!(tool_names(&reqs[0]).contains(&"todo_write"));
}

#[tokio::test]
async fn default_request_omits_plan_section_and_todo_write() {
    let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
    let mut agent = agent_with_todo_write(Box::new(mock)).with_system("base");
    run_text(&mut agent, "hi").await;
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert!(!system_of(&reqs[0]).contains("# Plan Mode"));
    assert!(!system_of(&reqs[0]).contains("## Plan reminder"));
    assert!(!tool_names(&reqs[0]).contains(&"todo_write"));
}

#[tokio::test]
async fn default_still_injects_nonempty_list() {
    let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
    let mut agent = agent_with_todo_write(Box::new(mock));
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![pending_item()],
    });
    run_text(&mut agent, "hi").await;
    let reqs = seen.lock().unwrap().clone();
    assert!(system_of(&reqs[0]).contains("## Current TODO list"));
    assert!(!system_of(&reqs[0]).contains("# Plan Mode"));
    assert!(!system_of(&reqs[0]).contains("## Plan reminder"));
    assert!(!tool_names(&reqs[0]).contains(&"todo_write"));
}

#[tokio::test]
async fn next_turn_clears_finished_todos() {
    let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
    let mut agent = agent_with_todo_write(Box::new(mock));
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![completed_item()],
    });
    let mut sink = VecEventSink::default();
    run_plain(&mut agent, "next", &mut sink).await.unwrap();
    assert!(agent.todos().is_empty());
    assert!(agent.todo_written_this_turn());
    assert!(sink.events.iter().any(|e| matches!(
        e,
        AgentEvent::TodosChanged { todos } if todos.is_empty()
    )));
    let reqs = seen.lock().unwrap().clone();
    assert!(!system_of(&reqs[0]).contains("## Current TODO list"));
}

#[tokio::test]
async fn next_turn_keeps_open_todos() {
    let (mock, seen) = CaptureRequests::new(vec![text_response("ok")]);
    let mut agent = agent_with_todo_write(Box::new(mock));
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![pending_item()],
    });
    run_text(&mut agent, "next").await;
    assert_eq!(agent.todos().items[0].id, "a");
    assert!(!agent.todo_written_this_turn());
    let reqs = seen.lock().unwrap().clone();
    assert!(system_of(&reqs[0]).contains("## Current TODO list"));
}

#[tokio::test]
async fn completing_turn_keeps_finished_list() {
    let todos = json!({"todos":[{"id":"a","content":"one","status":"completed"}]});
    let mock = MockProvider::new(vec![
        tool_response("c1", "todo_write", todos),
        text_response("done"),
    ]);
    let mut agent = agent_with_todo_write(Box::new(mock));
    agent.set_mode(AgentMode::Plan);
    run_text(&mut agent, "plan it").await;
    assert_eq!(agent.todos().items.len(), 1);
    assert_eq!(
        agent.todos().items[0].status,
        crate::core::todo::TodoStatus::Completed
    );
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
    assert_eq!(agent.mode(), AgentMode::Agent);

    let mut sink = NullSink;
    let ctx = turn_ctx(&agent);
    let selection = agent.selection();
    let run = agent.run("read it", &ctx, &mut sink);
    tokio::pin!(run);
    tokio::select! {
        biased;
        _ = entered_rx => {}
        _ = &mut run => panic!("turn finished before first complete awaited"),
    }
    selection.set_mode(AgentMode::Plan);
    drop(release_tx);
    assert_eq!(run.await.unwrap().text(), "done");

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert!(!system_of(&reqs[0]).contains("# Plan Mode"));
    assert!(!tool_names(&reqs[0]).contains(&"todo_write"));
    assert!(system_of(&reqs[1]).contains("# Plan Mode"));
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
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![pending_item()],
    });
    run_text(&mut agent, "read it").await;

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert!(system_of(&reqs[0]).contains("# Plan Mode"));
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
    let mut agent = agent_with_file_and_todo(Box::new(mock), tmp.path());
    agent.set_mode(AgentMode::Plan);
    agent.set_todos(crate::core::todo::TodoList {
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
    assert!(system_of(&reqs[0]).contains("# Plan Mode"));
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
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![pending_item()],
    });
    run_text(&mut agent, "read it").await;
    agent.set_mode(AgentMode::Agent);
    run_text(&mut agent, "next").await;

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3);
    assert!(system_of(&reqs[1]).contains("## Plan reminder"));
    assert!(system_of(&reqs[2]).contains("## Current TODO list"));
    assert!(!system_of(&reqs[2]).contains("# Plan Mode"));
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
    agent.set_todos(crate::core::todo::TodoList {
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
    assert!(system_of(&reqs[2]).contains("# Plan Mode"));
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
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![pending_item()],
    });
    run_text(&mut agent, "read it").await;
    agent.clear_history();
    agent.set_todos(crate::core::todo::TodoList {
        items: vec![pending_item()],
    });
    run_text(&mut agent, "fresh").await;

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3);
    assert!(system_of(&reqs[1]).contains("## Plan reminder"));
    assert!(!system_of(&reqs[2]).contains("## Plan reminder"));
}
