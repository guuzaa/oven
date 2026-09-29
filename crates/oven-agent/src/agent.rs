use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use oven_llm::{
    ContentBlock, Delta, Message, ModelId, Provider, ReasoningEffort, Request, Response, Role,
    Router, SamplingParams, StreamCollector, StreamEvent as LlmStreamEvent, ThinkingMode,
    ToolChoice, Usage,
};

use oven_host::{as_ms, now_ms};

use crate::approval::{ApprovalDecision, ApprovalRequestId, LoopLimitDecision, LoopLimitRequestId};
use crate::error::{AgentError, MAX_ITERS_EXCEEDED};
use crate::event::{AgentEvent, CallOutcome, StreamEvent, ToolEvent, ToolResult, TurnEvent};
use crate::history::{History, Record};
use crate::identity::{AgentId, ToolCallId};
use crate::mode::{AgentMode, ToolAccess};
use crate::prompt_template;
use crate::sink::EventSink;
use crate::todo::TodoList;
use crate::tools::{TodoWriteTool, Tool};
use crate::turn::{Step, StepCall, TurnContext, TurnOutput};

/// Cap on a tool's output as it enters the conversation, keeping a single
/// huge `file_read`/`bash` result from being carried (and re-encoded on
/// every request) forever.
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

/// A router shared between an `Agent` and callers that need to read it
/// (e.g. to validate a model switch) without the exclusive `&mut Agent`
/// access a running turn holds. Reading clones the inner `Arc<Router>`
/// snapshot (cheap, safe to hold across `.await`); mutating goes through
/// [`Agent::replace_router`].
pub type RouterHandle = Arc<RwLock<Arc<Router>>>;

/// A handle onto `router`, for callers that need the conversation driver and
/// the agents it spawns to share one router from the start.
pub fn router_handle(router: Router) -> RouterHandle {
    Arc::new(RwLock::new(Arc::new(router)))
}

/// The conversation driver. Holds tools and dispatches tool calls returned by
/// the provider until the provider replies without tool calls.
pub struct Agent {
    id: AgentId,
    router: RouterHandle,
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) history: History,
    model: ModelId,
    system: Option<String>,
    mode: AgentMode,
    todos: TodoList,
    reasoning_effort: Option<ReasoningEffort>,
    todo_written_this_turn: bool,
    todo_dirty: bool,
}

impl Agent {
    pub fn new(router: Router, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self::with_router(router_handle(router), tools)
    }

    /// Build an agent on a router it shares with whoever handed the handle
    /// over. A subagent joins the conversation driver's router rather than
    /// snapshotting it, so a `/setup` or `/model` switch reaches both.
    pub fn with_router(router: RouterHandle, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            id: AgentId::next(),
            router,
            tools,
            history: History::new(),
            model: ModelId::new("default"),
            system: None,
            mode: AgentMode::Agent,
            todos: TodoList::default(),
            reasoning_effort: None,
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

    pub fn with_system(mut self, content: impl Into<String>) -> Self {
        self.system = Some(content.into());
        self
    }

    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    /// A snapshot of the current router. Cheap to clone and safe to hold
    /// across `.await` points, unlike a lock guard.
    pub fn router(&self) -> Arc<Router> {
        self.router
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A handle to the shared router, independent of `&Agent`/`&mut Agent`.
    /// Lets a caller validate or read the router while a turn holds the
    /// agent's exclusive `&mut` borrow.
    pub fn router_handle(&self) -> RouterHandle {
        Arc::clone(&self.router)
    }

    /// Swaps in a freshly built router (e.g. `/setup` registering a new
    /// provider). Replacing the snapshot rather than mutating it in place is
    /// what makes this safe while another agent shares the same handle: a
    /// reader that captured the old router finishes its request on it, and
    /// the next reader gets the new one.
    pub fn replace_router(&mut self, router: Router) {
        let mut guard = self.router.write().unwrap_or_else(PoisonError::into_inner);
        *guard = Arc::new(router);
    }

    pub fn set_model(&mut self, model: impl Into<ModelId>) {
        self.model = model.into();
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.reasoning_effort = effort;
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

    fn dismiss_finished_todos(&mut self, sink: &mut impl EventSink) {
        if !self.todos.is_finished() {
            return;
        }
        self.todos = TodoList::default();
        self.todo_written_this_turn = true;
        sink.emit(AgentEvent::TodosChanged {
            todos: TodoList::default(),
        });
    }

    /// Set the reasoning effort for provider calls.
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    pub fn history(&self) -> impl ExactSizeIterator<Item = &Message> + '_ {
        self.history.messages()
    }

    /// The conversation as shared handles: a snapshot costs one refcount bump
    /// per message instead of copying every message the app layer renders.
    pub fn shared_history(&self) -> Vec<Arc<Message>> {
        self.history.shared_messages().cloned().collect()
    }

    pub fn history_timed(
        &self,
    ) -> impl ExactSizeIterator<Item = (&Message, u64, Option<u64>)> + '_ {
        self.history.iter_timed()
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

    /// [`history_records`](Self::history_records) for the messages from index
    /// `start` onward. The App layer appends only these after each turn, so
    /// persisting never rescales with the whole conversation.
    pub fn history_records_from(&self, start: usize) -> Vec<Record> {
        self.history.records_from(start)
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
            .filter(|t| !matches!(mode.tool_access(t.caps().permission), ToolAccess::Hidden))
            .map(|t| oven_llm::Tool {
                name: t.name().to_string(),
                description: Some(t.description().to_string()),
                input_schema: t.schema(),
            })
            .collect()
    }

    pub(crate) fn build_request(&self) -> Request {
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
        system = prompt_template::compose_todo_system(
            system.as_deref(),
            mode,
            todos,
            mode == AgentMode::Plan && self.todo_dirty && !todos.is_empty(),
        );
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
            provider_options: serde_json::Map::default(),
        }
    }

    async fn complete_response(
        &mut self,
        sink: &mut impl EventSink,
    ) -> Result<(Response, Option<(u64, u64)>), AgentError> {
        let started = Instant::now();
        let result = self.complete_or_stream(sink).await;
        tracing::debug!(
            model = %self.model,
            duration_ms = as_ms(started.elapsed()),
            "llm request complete"
        );
        result
    }

    async fn complete_or_stream(
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
                tracing::warn!(error = %error, model = %self.model, "stream start failed, falling back to complete");
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

    /// One provider round trip: ask, commit the reply, run the tools it asked
    /// for. [`Agent::run`] is one loop policy over this; a strategy of its own
    /// drives it step by step instead, and reads the returned [`Step`] to
    /// decide what to do next.
    ///
    /// The calls of one step run at the same time. That is what the system
    /// prompt promises the model when it asks it to batch independent calls,
    /// and it is the difference between three subagents working in parallel
    /// and three subagents working one after another.
    pub async fn step(
        &mut self,
        sink: &mut impl EventSink,
        ctx: &TurnContext,
    ) -> Result<Step, AgentError> {
        self.mode = ctx.mode();
        (self.model, self.reasoning_effort) = ctx.model();
        let (response, thinking) = self.complete_response(sink).await?;

        self.history
            .push(Message::assistant(response.content.clone()));
        if let Some((started_at, duration_ms)) = thinking {
            self.history.record_thinking(started_at, duration_ms);
        }
        if let Some(usage) = &response.usage {
            self.history.record_usage(usage);
            sink.emit(AgentEvent::Usage { usage: *usage });
        }

        let text = response.text();
        let usage = response.usage;
        if !response.has_tool_use() {
            return Ok(Step {
                text,
                calls: Vec::new(),
                usage,
            });
        }

        let planned = self.plan_calls(&response);
        let gates = gate_calls(&planned, ctx, sink).await?;
        let records = run_calls(&planned, gates, ctx, sink).await;
        let (calls, wrote_todo) = self.commit_calls(&planned, records, sink);
        self.todo_dirty = !wrote_todo;
        Ok(Step { text, calls, usage })
    }

    /// Resolves the response's calls against the mounted tools before any of
    /// them runs.
    fn plan_calls(&self, response: &Response) -> Vec<PlannedCall> {
        response
            .tool_uses()
            .filter_map(|block| {
                let ContentBlock::ToolUse {
                    id, name, input, ..
                } = block
                else {
                    return None;
                };
                let tool = self.tools.iter().find(|tool| tool.name() == name).cloned();
                let caps = tool.as_ref().map(|tool| tool.caps());
                let todos = if name == TodoWriteTool::NAME {
                    TodoList::parse(input).ok()
                } else {
                    None
                };
                Some(PlannedCall {
                    id: id.clone(),
                    call_id: ToolCallId::next(),
                    name: name.clone(),
                    input: input.clone(),
                    view: crate::tools::present_tool(name, input),
                    todos,
                    exclusive: caps.is_some_and(|caps| caps.exclusive),
                    tool,
                })
            })
            .collect()
    }

    /// Writes what each call left behind, in the order the model asked for it:
    /// a provider expects one tool result per call, in that order.
    fn commit_calls(
        &mut self,
        planned: &[PlannedCall],
        records: Vec<CallRecord>,
        sink: &mut impl EventSink,
    ) -> (Vec<StepCall>, bool) {
        let mut calls = Vec::with_capacity(planned.len());
        let mut wrote_todo = false;
        for (call, record) in planned.iter().zip(records) {
            calls.push(StepCall {
                call_id: call.call_id,
                name: call.name.clone(),
                outcome: record.outcome,
            });
            wrote_todo |= self.commit_todo(call, sink);
            self.history.push(Message::tool_result(
                call.id.clone(),
                record.summary,
                record.is_error,
            ));
        }
        (calls, wrote_todo)
    }

    /// A `todo_write` call replaces the checklist with what its arguments say.
    fn commit_todo(&mut self, call: &PlannedCall, sink: &mut impl EventSink) -> bool {
        let Some(list) = call.todos.clone() else {
            return false;
        };
        self.todos = list.clone();
        self.todo_written_this_turn = true;
        sink.emit(AgentEvent::TodosChanged { todos: list });
        true
    }

    #[tracing::instrument(name = "agent.turn", skip_all, fields(turn_id = ctx.turn_id.0, model = %self.model))]
    pub async fn run(
        &mut self,
        input: impl Into<String>,
        ctx: &TurnContext,
        sink: &mut impl EventSink,
    ) -> Result<TurnOutput, AgentError> {
        let input: String = input.into();
        sink.emit(AgentEvent::Turn(TurnEvent::Started));

        self.todo_written_this_turn = false;
        self.dismiss_finished_todos(sink);
        let policy = ctx.policy();
        let turn = async {
            self.history.push(Message::user_text(input));

            let mut index = 0;
            loop {
                for _ in 0..policy.max_iters {
                    index += 1;
                    sink.emit(AgentEvent::Turn(TurnEvent::StepStarted { index }));
                    let step = self.step(sink, ctx).await?;
                    sink.emit(AgentEvent::Turn(TurnEvent::StepFinished {
                        index,
                        stop: step.stop(),
                    }));
                    if step.is_final() {
                        let usage = self.last_turn_usage();
                        let duration_ms = self.history.elapsed_ms();
                        sink.emit(AgentEvent::Turn(TurnEvent::Completed {
                            usage,
                            duration_ms,
                        }));
                        return Ok(TurnOutput {
                            response: Message::assistant_text(step.text),
                            usage,
                        });
                    }
                }
                match self.ask_loop_continue(sink, ctx).await? {
                    LoopLimitDecision::Continue => {}
                    LoopLimitDecision::Exit => {
                        return Err(AgentError::max_iters_exceeded());
                    }
                }
            }
        };

        let result = {
            tokio::pin!(turn);
            tokio::select! {
                biased;
                () = ctx.cancellation.cancelled() => Err(AgentError::cancelled()),
                res = &mut turn => res,
            }
        };
        self.mode = ctx.mode();
        (self.model, self.reasoning_effort) = ctx.model();

        let duration_ms = self.history.elapsed_ms();
        match &result {
            Ok(output) => {
                tracing::info!(
                    duration_ms,
                    input_tokens = output.usage.input_tokens,
                    output_tokens = output.usage.output_tokens,
                    "turn completed"
                );
            }
            Err(e) if e.is_cancelled() => {
                tracing::info!(duration_ms, "turn cancelled");
                sink.emit(AgentEvent::Turn(TurnEvent::Cancelled { duration_ms }));
            }
            Err(e) => {
                tracing::warn!(duration_ms, error = %e.message, "turn failed");
                sink.emit(AgentEvent::Turn(TurnEvent::Failed {
                    error: e.clone(),
                    duration_ms,
                }));
            }
        }
        result
    }

    async fn ask_loop_continue(
        &self,
        sink: &mut impl EventSink,
        ctx: &TurnContext,
    ) -> Result<LoopLimitDecision, AgentError> {
        let max_iters = ctx.policy().max_iters;
        if !ctx.has_loop_limit_sender() {
            return Err(AgentError::max_iters_exceeded());
        }
        tracing::warn!(max_iters, "{MAX_ITERS_EXCEEDED}");
        let request_id = LoopLimitRequestId::next();
        sink.emit(AgentEvent::Turn(TurnEvent::LoopLimitReached {
            request_id,
            max_iters,
        }));
        match ctx.request_loop_continue(request_id, max_iters).await {
            Some(decision) => Ok(decision),
            None => Err(AgentError::cancelled()),
        }
    }

    /// Token usage of the last user turn.
    #[inline]
    pub fn last_turn_usage(&self) -> Usage {
        self.history.last_turn_usage()
    }
}

/// One tool call from a response, resolved against the mounted tools before
/// anything runs.
struct PlannedCall {
    /// The provider's id for the call, echoed back with its result.
    id: String,
    call_id: ToolCallId,
    name: String,
    input: serde_json::Value,
    view: crate::tools::ToolView,
    /// The checklist a `todo_write` call stands for. `None` when the call is
    /// not one, or its arguments do not describe a valid list.
    todos: Option<TodoList>,
    /// Whether this call has to run on its own. A tool that rewrites a file
    /// from what it read would otherwise lose one of two edits to the same
    /// file, and a tool that asks the user something would lose the question.
    exclusive: bool,
    tool: Option<Arc<dyn Tool>>,
}

/// What a call may do: run with the tool it resolved to, or report without
/// running anything.
enum Gate {
    Run(Arc<dyn Tool>),
    Refused(ToolResult),
}

/// What a finished call leaves for the commit phase. The output is kept as
/// the string the history needs rather than the whole result, which the
/// transcript has already been handed.
struct CallRecord {
    summary: String,
    is_error: bool,
    outcome: CallOutcome,
}

impl CallRecord {
    fn of(result: &ToolResult) -> Self {
        Self {
            summary: result.output().to_string(),
            is_error: !result.is_success(),
            outcome: result.outcome(),
        }
    }
}

/// Decides what each call may do. Approvals are asked one at a time and in
/// the order the model asked: a frontend answers a single prompt at a time,
/// so asking for them all at once would strand every answer but one.
async fn gate_calls(
    planned: &[PlannedCall],
    ctx: &TurnContext,
    sink: &mut impl EventSink,
) -> Result<Vec<Gate>, AgentError> {
    let mut gates = Vec::with_capacity(planned.len());
    for call in planned {
        // A gate that runs carries the tool it runs, so no later stage has to
        // go looking for one again — and an unmounted tool cannot run.
        let Some(tool) = call.tool.clone() else {
            let error = format!("unknown tool: {}", call.name);
            gates.push(Gate::Refused(ToolResult::Failed {
                error: error.clone(),
                output: Some(error),
            }));
            continue;
        };
        let gate = match ctx.mode().tool_access(tool.caps().permission) {
            ToolAccess::Hidden => Gate::Refused(ToolResult::Rejected {
                reason: format!("tool '{}' is unavailable in Ask mode", call.name),
            }),
            ToolAccess::RequiresApproval => {
                let request_id = ApprovalRequestId::next();
                sink.emit(AgentEvent::Tool(ToolEvent::ApprovalRequested {
                    request_id,
                    call_id: call.call_id,
                    name: call.name.clone(),
                    view: call.view.clone(),
                }));
                match ctx
                    .request_approval(
                        request_id,
                        call.call_id,
                        call.name.clone(),
                        call.view.clone(),
                    )
                    .await
                {
                    Some(ApprovalDecision::Approved) => Gate::Run(tool),
                    Some(ApprovalDecision::Rejected) => Gate::Refused(ToolResult::Rejected {
                        reason: "tool execution was not performed: the user declined permission"
                            .into(),
                    }),
                    None => return Err(AgentError::cancelled()),
                }
            }
            ToolAccess::Allowed => Gate::Run(tool),
        };
        gates.push(gate);
    }
    Ok(gates)
}

/// Runs everything the gates allowed. `Started` is reported for all of them
/// first so a frontend shows what is in flight; `Finished` follows as each
/// call actually lands, which is not the order they started in.
///
/// Calls run at the same time except for the tools that declare themselves
/// exclusive, which take a turn each: two edits to one file must see each
/// other's work, and two questions would leave the first one unanswered.
///
/// Every call leaves a record, in the order the model asked for them, so a
/// caller commits a history entry per call without re-checking which ran.
async fn run_calls(
    planned: &[PlannedCall],
    gates: Vec<Gate>,
    ctx: &TurnContext,
    sink: &mut impl EventSink,
) -> Vec<CallRecord> {
    let mut pending: Vec<Option<CallRecord>> = planned.iter().map(|_| None).collect();
    let mut running = FuturesUnordered::new();
    let exclusive = Arc::new(tokio::sync::Mutex::new(()));
    for (index, (call, gate)) in planned.iter().zip(gates).enumerate() {
        match gate {
            Gate::Refused(result) => {
                log_tool_finished(&call.name, call.call_id, &result, Instant::now());
                pending[index] = Some(CallRecord::of(&result));
                sink.emit(AgentEvent::Tool(ToolEvent::Finished {
                    call_id: call.call_id,
                    result,
                }));
            }
            Gate::Run(tool) => {
                sink.emit(AgentEvent::Tool(ToolEvent::Started {
                    call_id: call.call_id,
                    name: call.name.clone(),
                    view: call.view.clone(),
                }));
                log_tool_started(&call.name, call.call_id);
                let input = call.input.clone();
                let cx = ctx.clone();
                let exclusive = call.exclusive.then(|| Arc::clone(&exclusive));
                running.push(async move {
                    let _guard = match &exclusive {
                        Some(lock) => Some(lock.lock().await),
                        None => None,
                    };
                    let started = Instant::now();
                    (index, tool.run(&input, &cx).await, started)
                });
            }
        }
    }

    while let Some((index, outcome, started)) = running.next().await {
        let call = &planned[index];
        let result = match outcome {
            Ok(output) => ToolResult::Success {
                output: truncate(&output, MAX_TOOL_OUTPUT_BYTES),
            },
            Err(error) => ToolResult::Failed {
                error: error.to_string(),
                output: Some(truncate(&format!("error: {error}"), MAX_TOOL_OUTPUT_BYTES)),
            },
        };
        log_tool_finished(&call.name, call.call_id, &result, started);
        pending[index] = Some(CallRecord::of(&result));
        sink.emit(AgentEvent::Tool(ToolEvent::Finished {
            call_id: call.call_id,
            result,
        }));
    }
    pending
        .into_iter()
        .enumerate()
        .map(|(index, record)| match record {
            Some(record) => record,
            // A gate that allowed a call the loop never awaited cannot happen:
            // the loop only ends once every running call has landed, so this
            // keeps the promise of one record per call even if that breaks.
            None => {
                let call = &planned[index];
                tracing::error!(tool = %call.name, "a running tool call left no record");
                CallRecord::of(&ToolResult::Failed {
                    error: format!("tool '{}' was never run", call.name),
                    output: Some(format!("tool '{}' was never run", call.name)),
                })
            }
        })
        .collect()
}

#[derive(Default)]
struct ThinkingSpan {
    started_at: Option<u64>,
    ended_at: Option<u64>,
}

impl ThinkingSpan {
    fn note(&mut self) {
        if self.started_at.is_none() {
            self.started_at = Some(now_ms());
        }
    }

    /// Ends the reasoning phase and reports its duration to the transcript, so
    /// the window closes before the answer or tool call that ended it streams.
    /// No-op once closed, or when the phase was too short to time.
    fn close(&mut self, sink: &mut impl EventSink) {
        if self.started_at.is_none() || self.ended_at.is_some() {
            return;
        }
        self.ended_at = Some(now_ms());
        if let Some((_, duration_ms)) = self.span() {
            sink.emit(AgentEvent::Stream(StreamEvent::ThinkingDone {
                duration_ms,
            }));
        }
    }

    /// Closes the span, reports it to the transcript and returns it for the
    /// persisted history. Call once per span, after the stream is drained.
    fn finish(&mut self, sink: &mut impl EventSink) -> Option<(u64, u64)> {
        self.close(sink);
        self.span()
    }

    fn span(&self) -> Option<(u64, u64)> {
        let start = self.started_at?;
        let duration_ms = self.ended_at.unwrap_or(start).saturating_sub(start);
        (duration_ms > 0).then_some((start, duration_ms))
    }
}

/// A whole non-streaming response arrives as one shot: its wall-clock duration
/// is all the thinking window that can be observed. Sub-millisecond durations
/// round up so timed thinking is never mistaken for untimed.
fn thinking_span(elapsed: Duration) -> (u64, u64) {
    let duration_ms = as_ms(elapsed).max(1);
    (now_ms().saturating_sub(duration_ms), duration_ms)
}

fn log_tool_started(name: &str, call_id: ToolCallId) {
    tracing::info!(name, call_id = call_id.0, "tool started");
}

fn log_tool_finished(name: &str, call_id: ToolCallId, result: &ToolResult, started: Instant) {
    let duration_ms = as_ms(started.elapsed());
    let (ok, error) = match result {
        ToolResult::Success { .. } => (true, None),
        ToolResult::Failed { error, .. } => (false, Some(error.as_str())),
        ToolResult::Rejected { reason } => (false, Some(reason.as_str())),
        ToolResult::Cancelled => (false, Some("cancelled")),
    };
    tracing::info!(
        name,
        call_id = call_id.0,
        ok,
        error,
        duration_ms,
        "tool finished"
    );
}

/// Keeps the head and the tail of an oversized output, dropping only the
/// middle: the opening lines carry context, the closing lines carry the
/// result (exit status, summary, last diff hunk).
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = s.floor_char_boundary(max / 2);
    let tail_start = s.floor_char_boundary(s.len() - (max - max / 2));
    format!("{}\n...[truncated]\n{}", &s[..head], &s[tail_start..])
}

#[cfg(test)]
mod truncate_tests {
    use super::{MAX_TOOL_OUTPUT_BYTES, truncate};

    const HEAD: &str = "HEAD";
    const TAIL: &str = "TAIL";
    const MARKER: &str = "\n...[truncated]\n";

    #[test]
    fn short_output_is_returned_unchanged() {
        assert_eq!(truncate("hello", MAX_TOOL_OUTPUT_BYTES), "hello");
    }

    #[test]
    fn oversized_output_keeps_head_and_tail() {
        let body = format!(
            "{}{}",
            HEAD.repeat(MAX_TOOL_OUTPUT_BYTES),
            TAIL.repeat(MAX_TOOL_OUTPUT_BYTES)
        );
        let out = truncate(&body, MAX_TOOL_OUTPUT_BYTES);
        assert!(
            out.len() <= MAX_TOOL_OUTPUT_BYTES + MARKER.len(),
            "cap must bound the stored output: {}",
            out.len()
        );
        assert!(out.starts_with(HEAD), "the head must survive: {out:?}");
        assert!(out.ends_with(TAIL), "the tail must survive: {out:?}");
        assert!(out.contains(MARKER));
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let body = "é".repeat(4 * MAX_TOOL_OUTPUT_BYTES);
        let out = truncate(&body, MAX_TOOL_OUTPUT_BYTES - 1);
        assert!(out.contains(MARKER));
        assert!(out.len() <= MAX_TOOL_OUTPUT_BYTES - 1 + MARKER.len());
        assert!(out.starts_with('é'), "must start on a char boundary");
        assert!(out.ends_with('é'), "must end on a char boundary");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RunPolicy;
    use crate::StepStop;
    use crate::TurnId;
    use crate::identity::ToolCallId;
    use crate::question::AnswerResponse;
    use crate::sink::{NullSink, VecEventSink};
    use crate::tools::{
        AnswerTool, BashTool, FileEditTool, FileReadTool, FileWriteTool, TodoWriteTool,
    };
    use crate::turn::TurnContext;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt, iter};
    use oven_llm::{
        Delta, ModelInfo, ProviderError, ProviderName, Result as LlmResult, StopReason,
        StreamEvent as LlmStreamEvent,
    };
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

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

    fn turn_ctx() -> TurnContext {
        turn_ctx_with(TEST_MAX_ITERS)
    }

    fn turn_ctx_with(max_iters: usize) -> TurnContext {
        TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            AgentMode::Agent,
            ModelId::new("default"),
            None,
        )
        .with_policy(RunPolicy::default().with_max_iters(max_iters))
    }

    async fn run_text(agent: &mut Agent, input: &str) -> String {
        let mut sink = NullSink;
        let ctx = TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            agent.mode(),
            agent.model().clone(),
            agent.reasoning_effort(),
        )
        .with_policy(RunPolicy::default().with_max_iters(TEST_MAX_ITERS));
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
                TurnEvent::Completed { .. }
                    | TurnEvent::Cancelled { .. }
                    | TurnEvent::Failed { .. }
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
        let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            AgentMode::Ask,
            ModelId::new("default"),
            None,
        )
        .with_approval_sender(approval_tx);
        let mut sink = NullSink;
        let turn = agent.run("run it", &ctx, &mut sink);
        tokio::pin!(turn);

        let approval = tokio::select! {
            approval = approval_rx.recv() => approval.unwrap(),
            _ = &mut turn => panic!("turn completed before requesting approval"),
        };
        assert_eq!(approval.name, "bash");
        assert!(!marker.exists());
        approval.responder.send(ApprovalDecision::Approved).unwrap();

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
        let (question_tx, mut question_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = turn_ctx().with_question_sender(question_tx);
        let mut sink = NullSink;
        let reply = {
            let turn = agent.run("pick one", &ctx, &mut sink);
            tokio::pin!(turn);

            let request = tokio::select! {
                request = question_rx.recv() => request.unwrap(),
                _ = &mut turn => panic!("turn completed before asking the user"),
            };
            assert_eq!(request.question.question, QUESTION);
            assert_eq!(request.question.options.len(), 2);
            request
                .responder
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

        fn caps(&self) -> crate::tools::ToolCaps {
            crate::tools::ToolCaps {
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
        run_with(&mut agent, "go", &turn_ctx(), &mut VecEventSink::default())
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
        run_with(&mut agent, "go", &turn_ctx(), &mut VecEventSink::default())
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
        run_with(&mut agent, "go", &turn_ctx(), &mut sink)
            .await
            .unwrap();

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
        run_with(&mut agent, "go", &turn_ctx(), &mut VecEventSink::default())
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
        let err = agent
            .run(
                "hi",
                &TurnContext::new(
                    TurnId::next(),
                    cancel,
                    AgentMode::Agent,
                    ModelId::new("default"),
                    None,
                ),
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
                AgentEvent::Turn(TurnEvent::Cancelled { duration_ms: 0 }),
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
    async fn completed_duration_ms_matches_history_timestamps() {
        let mock = MockProvider::new(vec![text_response("ok")]);
        let mut agent = Agent::new(router_with(Box::new(mock)), Vec::new());
        let mut sink = VecEventSink::default();
        run_with(&mut agent, "hi", &turn_ctx(), &mut sink)
            .await
            .unwrap();
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
        let out = run_with(&mut agent, "hi", &turn_ctx(), &mut sink)
            .await
            .unwrap();
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
        let out = run_with(&mut agent, "hi", &turn_ctx(), &mut sink)
            .await
            .unwrap();
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
        run_with(&mut agent, "plan it", &turn_ctx(), &mut sink)
            .await
            .unwrap();

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
        run_with(&mut agent, "hi", &turn_ctx(), &mut sink)
            .await
            .unwrap();
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
        run_with(&mut agent, "read note.txt", &turn_ctx(), &mut sink)
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
        let err = agent
            .run(
                "hi",
                &TurnContext::new(
                    TurnId::next(),
                    cancel,
                    AgentMode::Agent,
                    ModelId::new("default"),
                    None,
                ),
                &mut sink,
            )
            .await
            .unwrap_err();
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
        let err = run_with(&mut agent, "read it", &turn_ctx_with(2), &mut sink)
            .await
            .unwrap_err();
        assert_eq!(err.message, crate::MAX_ITERS_EXCEEDED);
        assert_valid_event_sequence(&sink.events);
        assert!(
            !sink
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::Turn(TurnEvent::LoopLimitReached { .. })))
        );
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
        let ctx = turn_ctx_with(2).with_loop_limit_sender(tx);
        let mut sink = VecEventSink::default();
        let text = {
            let turn = agent.run("read it", &ctx, &mut sink);
            tokio::pin!(turn);

            let prompt = tokio::select! {
                prompt = rx.recv() => prompt.unwrap(),
                _ = &mut turn => panic!("turn completed before loop limit prompt"),
            };
            assert_eq!(prompt.max_iters, 2);
            prompt.responder.send(LoopLimitDecision::Continue).unwrap();
            turn.await.unwrap().text()
        };
        assert_eq!(text, "done");
        assert_valid_event_sequence(&sink.events);
        assert!(sink.events.iter().any(|e| matches!(
            e,
            AgentEvent::Turn(TurnEvent::LoopLimitReached { max_iters: 2, .. })
        )));
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
        let ctx = turn_ctx_with(2).with_loop_limit_sender(tx);
        let mut sink = VecEventSink::default();
        let err = {
            let turn = agent.run("read it", &ctx, &mut sink);
            tokio::pin!(turn);

            let prompt = tokio::select! {
                prompt = rx.recv() => prompt.unwrap(),
                _ = &mut turn => panic!("turn completed before loop limit prompt"),
            };
            prompt.responder.send(LoopLimitDecision::Exit).unwrap();
            turn.await.unwrap_err()
        };
        assert_eq!(err.message, crate::MAX_ITERS_EXCEEDED);
        assert_valid_event_sequence(&sink.events);
        assert!(
            sink.events
                .iter()
                .any(|e| matches!(e, AgentEvent::Turn(TurnEvent::LoopLimitReached { .. })))
        );
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
        let mut agent =
            Agent::new(router_with(Box::new(mock)), Vec::new()).with_system("hello system");
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
            vec![Arc::new(crate::tools::TodoWriteTool)],
        )
    }

    fn agent_with_file_and_todo(provider: Box<dyn Provider>, root: &std::path::Path) -> Agent {
        Agent::new(
            router_with(provider),
            vec![
                Arc::new(FileReadTool::new(root)),
                Arc::new(crate::tools::TodoWriteTool),
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
        let mut agent = agent_with_todo_write(Box::new(mock));
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

    fn completed_item() -> crate::todo::TodoItem {
        crate::todo::TodoItem {
            id: "a".into(),
            content: "one".into(),
            status: crate::todo::TodoStatus::Completed,
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
        agent.set_todos(crate::todo::TodoList {
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
        agent.set_todos(crate::todo::TodoList {
            items: vec![completed_item()],
        });
        let mut sink = VecEventSink::default();
        run_with(&mut agent, "next", &turn_ctx(), &mut sink)
            .await
            .unwrap();
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
        agent.set_todos(crate::todo::TodoList {
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
            crate::todo::TodoStatus::Completed
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
        agent.set_todos(crate::todo::TodoList {
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
        agent.set_todos(crate::todo::TodoList {
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
