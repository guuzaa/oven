//! One provider round trip and the tools it asked for.
//!
//! [`Agent::step`] is one round trip; [`Agent::run`] is the loop policy that
//! keeps stepping until the provider stops asking for tools.

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use oven_llm::{ContentBlock, Message, Response, Usage};

use super::Agent;
use super::notify::{MAX_TOOL_OUTPUT_BYTES, log_tool_finished, log_tool_started, truncate};
use crate::capabilities::tools::{TodoWriteTool, Tool};
use crate::core::error::{AgentError, MAX_ITERS_EXCEEDED};
use crate::core::event::{AgentEvent, CallOutcome, ToolEvent, ToolResult, TurnEvent};
use crate::core::identity::ToolCallId;
use crate::core::interaction::{ApprovalDecision, LoopLimitDecision};
use crate::core::mode::ToolAccess;
use crate::core::sink::EventSink;
use crate::core::todo::TodoList;
use crate::core::turn::{Step, StepCall, TurnContext, TurnOutput};

impl Agent {
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
        let gates = gate_calls(&planned, ctx).await?;
        let records = run_calls(&planned, gates, ctx, sink).await;
        let (calls, wrote_todo) = self.commit_calls(&planned, records, sink);
        self.append_queued_prompts(ctx, sink);
        self.todo_dirty = !wrote_todo;
        Ok(Step { text, calls, usage })
    }

    /// Chats typed while tools were running ride along with the results the
    /// next provider request uploads.
    fn append_queued_prompts(&mut self, ctx: &TurnContext, sink: &mut impl EventSink) {
        for text in ctx.take_pending() {
            self.history.push(Message::user_text(text.clone()));
            sink.emit(AgentEvent::Turn(TurnEvent::UserAppended { text }));
        }
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
                    view: crate::capabilities::tools::present_tool(name, input),
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

    #[tracing::instrument(name = "agent.turn", skip_all, fields(turn_id = ctx.turn_id.0, model = %self.model()))]
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
                match self.ask_loop_continue(ctx).await? {
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

    async fn ask_loop_continue(&self, ctx: &TurnContext) -> Result<LoopLimitDecision, AgentError> {
        let max_iters = ctx.policy().max_iters;
        if !ctx.has_user() {
            return Err(AgentError::max_iters_exceeded());
        }
        tracing::warn!(max_iters, "{MAX_ITERS_EXCEEDED}");
        match ctx.request_loop_continue(max_iters).await {
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
    view: crate::capabilities::tools::ToolView,
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
async fn gate_calls(planned: &[PlannedCall], ctx: &TurnContext) -> Result<Vec<Gate>, AgentError> {
    let mut gates = Vec::with_capacity(planned.len());
    for call in planned {
        // A gate that runs carries the tool it runs, so no later stage has to
        // go looking for one again — and an unmounted tool cannot run.
        let Some(tool) = call.tool.clone() else {
            gates.push(Gate::Refused(ToolResult::Rejected {
                reason: format!("unknown tool: {}", call.name),
            }));
            continue;
        };
        let gate = match ctx.mode().tool_access(tool.caps().permission) {
            ToolAccess::Hidden => Gate::Refused(ToolResult::Rejected {
                reason: format!("tool '{}' is unavailable in Ask mode", call.name),
            }),
            ToolAccess::RequiresApproval => {
                match ctx
                    .approve(call.call_id, call.name.clone(), call.view.clone())
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
        sink.emit(AgentEvent::Tool(ToolEvent::Started {
            call_id: call.call_id,
            name: call.name.clone(),
            view: call.view.clone(),
        }));
        log_tool_started(&call.name, call.call_id);
        match gate {
            Gate::Refused(result) => {
                log_tool_finished(&call.name, call.call_id, &result, Instant::now());
                pending[index] = Some(CallRecord::of(&result));
                sink.emit(AgentEvent::Tool(ToolEvent::Finished {
                    call_id: call.call_id,
                    result,
                    detail: None,
                }));
            }
            Gate::Run(tool) => {
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
        let detail = call
            .tool
            .as_ref()
            .and_then(|tool| tool.result_detail(&result));
        pending[index] = Some(CallRecord::of(&result));
        sink.emit(AgentEvent::Tool(ToolEvent::Finished {
            call_id: call.call_id,
            result,
            detail,
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
