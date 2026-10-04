const SOURCE_URL = "https://github.com/guuzaa/oven/blob/master";
const PLAY_INTERVAL_MS = 3_200;
const LOOP_INTERVAL_MS = 750;
const REDUCED_MOTION = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

const CHAPTERS = {
  start: "boot",
  input: "input",
  loop: "agent loop",
  tools: "tool calls",
  plan: "plan & todos",
  subagent: "subagent",
  memory: "memory",
  finish: "ending",
};
const TODO_MARKS = { " ": "[ ]", "~": "[~]", x: "[x]", "-": "[-]" };

const TRACE = [
  {
    chapter: "start",
    title: "oven starts",
    text: "AppBuilder loads the config, the AGENTS.md / CLAUDE.md instructions, skills, MCP servers and the memory index, then builds the system prompt once — base, environment, instructions, skills, memory catalog. It mounts the tools, creates the driver agent and the subagent roles on the one event bus they publish on, and starts the runtime actor, which waits on its inbox.",
    lanes: ["app", "mem", "host"],
    events: ["AppState { phase: Idle, mode: Plan, … }"],
    where: ["crates/oven-app/src/api/builder.rs", "crates/oven-mem/src/store.rs"],
    log: [["d", "~/tiny-cli · 2 memories · 3 skills · 1 MCP server"]],
    status: "plan · idle",
  },
  {
    chapter: "input",
    title: "You press Enter",
    text: "The composer hands the text to App::submit, which classifies it once: a registered /command, !cmd for your own shell, otherwise chat. The driver is idle, so it goes to the runtime's inbox; a chat typed while tools run is parked and rides along with the next request.",
    lanes: ["tui", "app"],
    events: ['Input::Chat("add a --json flag to `tiny ls`")'],
    where: ["crates/oven-tui/src/runtime/ui/keys.rs", "crates/oven-app/src/api/input.rs"],
    log: [["u", "› add a --json flag to `tiny ls`"]],
    status: "plan · sending",
  },
  {
    chapter: "input",
    title: "The runtime starts a turn",
    text: "start_turn mints a TurnId, moves the phase to Running and builds the TurnContext: a cancel token, the shared Selection (mode and model), the RunPolicy (200 steps by default) and the request sink approvals and questions go through. It then awaits the turn in a select! that also watches shutdown and registry wake-ups.",
    lanes: ["app"],
    events: ["phase = Running { turn_id: 7 }"],
    where: ["crates/oven-app/src/runtime/turn.rs"],
    status: "plan · working",
  },
  {
    chapter: "loop",
    title: "Agent::run opens the turn",
    text: "It emits Started, resets the per-turn todo flags, clears a checklist that finished last turn and pushes your message into the history. The loop sits in a biased select! on the cancel token, so Esc Esc stops it at any await point.",
    lanes: ["agent"],
    events: ["Turn(Started)"],
    where: ["crates/oven-agent/src/runtime/agent/step.rs"],
  },
  {
    chapter: "loop",
    title: "Step 1 · build the request",
    text: "Mode and model are read fresh from the Selection, so Shift+Tab or /model mid-turn lands here. The system prompt is the frozen base plus the # Plan Mode rules, and the checklist when there is one. Tools are filtered by mode: todo_write only in plan, writers hidden in ask.",
    lanes: ["agent"],
    events: ["Turn(StepStarted { index: 1 })"],
    where: ["crates/oven-agent/src/runtime/agent/request.rs", "crates/oven-agent/src/core/prompt_template/plan.rs"],
  },
  {
    chapter: "loop",
    title: "Stream the reply",
    text: "The router streams the reply: thinking deltas first, then the first tool-call JSON closes the thinking window and ThinkingDone carries its length. Usage arrives as the provider sends it, which is what drives the context gauge. This reply asks for two calls: todo_write and task.",
    lanes: ["llm", "agent", "tui"],
    events: ["Stream(ThinkingDelta { … }) ×n", "Stream(ThinkingDone { duration_ms: 2140 })", "Usage { input_tokens: 9120, … }"],
    where: ["crates/oven-agent/src/runtime/agent/request.rs"],
    log: [["k", "Thought for 2.1s"]],
  },
  {
    chapter: "tools",
    title: "Run both calls at once",
    text: "Each call is resolved to its tool, then gated: plan mode lets both run without asking. Started goes out for both and they run at once. The checklist is replaced when the step commits — after both calls land, so the widget appears before the step ends.",
    lanes: ["agent", "tui"],
    events: ["Tool(Started { todo_write })", "Tool(Started { task })", "Tool(Finished { todo_write })", "Tool(Finished { task: Success })", "TodosChanged { 3 items }"],
    where: ["crates/oven-agent/src/runtime/agent/step.rs", "crates/oven-agent/src/capabilities/tools/todo_write.rs"],
    log: [
      ["t", "todo_write · 3 todos (1 in_progress, 0 completed)"],
      ["t", "Agent explore: find how ls prints"],
    ],
    todos: [
      ["~", "Find where ls renders output"],
      [" ", "Add the --json flag"],
      [" ", "Run the tests"],
    ],
    status: "plan · step 1 · 2 tools",
  },
  {
    chapter: "subagent",
    title: "explore#1 goes to work",
    text: "task hands a SpawnRequest to the subagent supervisor, which registers explore#1 as pending, takes a concurrency slot and runs a fresh Agent with read-only tools and its own history. Its events go straight to the bus under its own agent id, so the TUI routes them to the strip and its viewer, never to the main transcript.",
    lanes: ["app", "agent", "host"],
    events: ["explore#1 · Turn(Started)", "explore#1 · Tool(Started { grep })", "wake → AppState.subagents"],
    where: ["crates/oven-agent/src/capabilities/tools/task.rs", "crates/oven-app/src/capabilities/subagent.rs"],
    strip: "◆ explore#1 · running 4.2s · 3 tools · find how ls prints",
  },
  {
    chapter: "subagent",
    title: "The report comes back",
    text: "The subagent's final answer, capped at 16 KB, becomes the task tool result; the agent itself is dropped and only its counters stay in the registry. Both results are written to the history in the order the model asked for them, and the step ends.",
    lanes: ["app", "agent"],
    events: ["Tool(Finished { task: Success })", "Turn(StepFinished { index: 1, stop: ToolUse })"],
    where: ["crates/oven-app/src/capabilities/subagent.rs"],
    log: [["o", "  └ [explore#1 done · 3 steps, 6 tool calls · 8.4k tokens · 12.3s]"]],
    strip: "◇ explore#1 · done 12.3s · 6 tools",
  },
  {
    chapter: "tools",
    title: "Step 2 · edit the code",
    text: "The model calls file_edit and todo_write. file_edit is exclusive, so two edits from one reply never overlap and neither loses the other's change. The write goes through oven-host, which keeps paths inside the workspace, and the tool's view carries the diff the transcript paints.",
    lanes: ["agent", "host", "tui"],
    events: ["Turn(StepStarted { index: 2 })", "Tool(Started { file_edit })", "Tool(Finished { file_edit })", "TodosChanged { … }"],
    where: ["crates/oven-agent/src/capabilities/tools/file_edit.rs", "crates/oven-host/src/filesystem.rs"],
    log: [
      ["t", "Edit src/ls.rs"],
      ["+", "  + #[arg(long)] json: bool,"],
      ["+", "  + if args.json { return print_json(&entries); }"],
    ],
    todos: [
      ["x", "Find where ls renders output"],
      ["~", "Add the --json flag"],
      [" ", "Run the tests"],
    ],
    status: "plan · step 2",
  },
  {
    chapter: "tools",
    title: "Step 3 · run the tests",
    text: "bash has execute permission: it runs unattended in agent and plan mode, while ask mode parks the turn on an approval prompt (phase Awaiting) until you answer. The command runs to completion and its output comes back as one result, capped at 64 KiB on the way into the history. This step used tools but did not touch the checklist.",
    lanes: ["agent", "host", "tui"],
    events: ["Tool(Started { bash })", "Tool(Finished { bash: Success })"],
    where: ["crates/oven-agent/src/capabilities/tools/bash.rs", "crates/oven-host/src/command.rs"],
    log: [
      ["t", "Ran cargo test"],
      ["o", "  test ls::prints_json ... ok"],
      ["o", "  test result: ok. 14 passed"],
    ],
    status: "plan · step 3",
  },
  {
    chapter: "plan",
    title: "Step 4 · the plan reminder",
    text: "The last step used tools but not todo_write, so this request's system prompt ends with a ## Plan reminder — it lives only in the request, never in the history. The model marks every item done and, in the same reply, saves something it learned.",
    lanes: ["agent", "llm", "tui"],
    events: ["Turn(StepStarted { index: 4 })", "Tool(Started { todo_write })", "Tool(Started { memory_write })", "Tool(Finished { todo_write })", "Tool(Finished { memory_write: created })", "TodosChanged { all completed }"],
    where: ["crates/oven-agent/src/core/prompt_template/plan.rs"],
    log: [["t", "todo_write · 3 todos (0 in_progress, 3 completed)"]],
    todos: [
      ["x", "Find where ls renders output"],
      ["x", "Add the --json flag"],
      ["x", "Run the tests"],
    ],
    status: "plan · step 4",
  },
  {
    chapter: "memory",
    title: "…and remembers a gotcha",
    text: "The memory_write from that same reply. oven-mem checks the limits, writes .oven/memory/tests-need-proxy-flag.md atomically (temp file, then rename) and updates its index. The catalog in the system prompt is not re-rendered: that keeps the cached prompt prefix, and the model already has the write in its tool result.",
    lanes: ["agent", "mem", "host"],
    events: ["Tool(Finished { memory_write: created })"],
    where: ["crates/oven-app/src/capabilities/memory/write.rs", "crates/oven-mem/src/store.rs"],
    log: [["t", "Memorized workspace/tests-need-proxy-flag"]],
  },
  {
    chapter: "finish",
    title: "Step 5 · the answer",
    text: "A reply with no tool calls is final. Its text streams into the transcript and the turn emits Completed with its usage and duration — every turn emits exactly one Started and exactly one of Completed, Cancelled or Failed.",
    lanes: ["llm", "agent", "tui"],
    events: ["Turn(StepStarted { index: 5 })", "Stream(TextDelta { … }) ×n", "Turn(StepFinished { index: 5, stop: FinalAnswer })", "Turn(Completed { usage, duration_ms })"],
    where: ["crates/oven-agent/src/runtime/agent/step.rs"],
    log: [["a", "∙ Added `--json` to `tiny ls`; it prints the entries as a JSON array. All 14 tests pass. I noted that the integration tests need OVEN_TEST_PROXY=1."]],
    status: "plan · finishing",
  },
  {
    chapter: "finish",
    title: "The runtime wraps up",
    text: "The runtime appends only the new history records to the session file, plus a checklist snapshot, then publishes the new AppState: history, todos, usage, context size. Past 80% of the context window it compacts now. The phase goes back to Idle and anything queued during the turn runs next.",
    lanes: ["app", "host"],
    events: ["AppState { todos, last_turn_usage, context_tokens, … }", "phase = Idle"],
    where: ["crates/oven-app/src/runtime/mod.rs", "crates/oven-app/src/core/session.rs"],
    status: "plan · idle · context 18%",
  },
  {
    chapter: "finish",
    title: "Next prompt · the list folds away",
    text: "A finished checklist stays on screen until you write again. The next Agent::run starts by clearing it: TodosChanged with an empty list, and the widget collapses to nothing.",
    lanes: ["agent", "tui"],
    events: ["Turn(Started)", "TodosChanged { [] }"],
    where: ["crates/oven-agent/src/runtime/agent/mod.rs"],
    log: [["u", "› thanks! now mention it in the README"]],
    todos: [],
    status: "plan · working",
  },
];

const LOOP_NODES = [
  {
    label: "run()",
    tag: "turn",
    title: "Open the turn",
    text: "Emits Started, resets the todo flags, clears a checklist finished last turn and pushes the user message. Everything below runs inside a select! on the cancel token.",
    code: `sink.emit(Turn(Started));
self.dismiss_finished_todos(sink);
self.history.push(Message::user_text(input));
select! { _ = cancelled() => Err(cancelled()), res = turn => res }`,
  },
  {
    label: "StepStarted",
    tag: "step",
    title: "Count the step",
    text: "Each provider round trip is numbered from 1. After max_iters steps (200 for the driver, 60 for a subagent) the loop asks whether to keep going; a subagent has nobody to ask, so its turn fails with max_iters exceeded.",
    code: `for _ in 0..policy.max_iters {
    index += 1;
    sink.emit(Turn(StepStarted { index }));
    let step = self.step(sink, ctx).await?;`,
  },
  {
    label: "build_request",
    tag: "step",
    title: "Build the request",
    text: "Mode, model and reasoning effort come from the shared Selection, read again every step. The system prompt is the frozen base with the mode overlay, the checklist and the plan reminder composed on top. Tools the mode hides are not sent at all.",
    code: `Request {
    model,
    system: compose_todo_system(base, mode, &todos, remind),
    messages: history_without_system,
    tools: self.llm_tools(mode),
    ..
}`,
  },
  {
    label: "stream",
    tag: "llm",
    title: "Stream the reply",
    text: "Thinking and text deltas are forwarded as they arrive. Text or a tool-call delta closes the thinking window. If the stream cannot start, the agent falls back to a one-shot completion; the router retries transient provider errors.",
    code: `match router.stream(&req).await {
    Ok(stream) => forward ThinkingDelta / TextDelta,
    Err(_) => Provider::complete(&router, &req).await?,
}`,
  },
  {
    label: "commit reply",
    tag: "step",
    title: "Commit the reply",
    text: "The assistant message enters the history with its thinking time and token usage, and Usage is emitted right away for the context gauge.",
    code: `self.history.push(Message::assistant(response.content));
self.history.record_usage(usage);
sink.emit(AgentEvent::Usage { usage });`,
  },
  {
    label: "tool calls?",
    tag: "decision",
    title: "Did it ask for tools?",
    text: "No tool calls means the step is final: the loop emits Completed with usage and duration and returns the text. Otherwise the calls are planned, gated, run and committed.",
    code: `if !response.has_tool_use() {
    return Ok(Step { text, calls: vec![], usage }); // is_final()
}`,
  },
  {
    label: "plan_calls",
    tag: "tools",
    title: "Resolve the calls",
    text: "Each call is matched to a mounted tool by name before anything runs. Its view (summary, diff), the parsed checklist for todo_write and the exclusive flag are worked out here.",
    code: `PlannedCall { call_id, name, input, view, todos, exclusive, tool }`,
  },
  {
    label: "gate_calls",
    tag: "tools",
    title: "Gate them, one at a time",
    text: "In the model's order: an unknown tool or one hidden by ask mode is rejected; one that needs approval parks the turn on a prompt (phase Awaiting) until you answer. A screen can answer one prompt at a time, so approvals are never asked together.",
    code: `match mode.tool_access(tool.caps().permission) {
    Hidden => Refused(Rejected { .. }),
    RequiresApproval => ctx.approve(call_id, name, view).await,
    Allowed => Run(tool),
}`,
  },
  {
    label: "run_calls",
    tag: "tools",
    title: "Run them together",
    text: "Started goes out for every call, then they run concurrently; exclusive tools take a lock in turn. Finished arrives in completion order, and output is capped at 64 KiB.",
    code: `for call in planned { sink.emit(Tool(Started { .. })); running.push(run(call)); }
while let Some(done) = running.next().await {
    sink.emit(Tool(Finished { .. }));
}`,
  },
  {
    label: "commit_calls",
    tag: "tools",
    title: "Commit in request order",
    text: "One tool_result per call, in the order the model asked. A valid todo_write replaces the checklist and emits TodosChanged. If no todo_write ran, plan mode reminds the model next step — while the list is non-empty.",
    code: `for (call, record) in planned.zip(records) {
    self.commit_todo(call, sink);
    self.history.push(Message::tool_result(call.id, record.summary, record.is_error));
}
self.todo_dirty = !wrote_todo;`,
  },
  {
    label: "steered prompts",
    tag: "step",
    title: "Add what you typed meanwhile",
    text: "Chats you typed while the tools were running are appended as user messages, so they ride along with the tool results on the next request.",
    code: `for text in ctx.take_pending() {
    self.history.push(Message::user_text(text.clone()));
    sink.emit(Turn(UserAppended { text }));
}`,
  },
  {
    label: "StepFinished",
    tag: "step",
    title: "Close the step, go again",
    text: "StepFinished carries why the step stopped (ToolUse or FinalAnswer). A tool step loops back to build the next request with the tool results in the history.",
    code: `sink.emit(Turn(StepFinished { index, stop: step.stop() }));
if step.is_final() { emit Completed; return Ok(TurnOutput { .. }) }`,
  },
];
const LOOP_DECISION = 5;
const LOOP_LAP = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 1, 2, 3, 4, 5];

const TOOLS = [
  { name: "file_read", perm: "read" },
  { name: "glob", perm: "read" },
  { name: "grep", perm: "read" },
  { name: "skill_read", perm: "read" },
  { name: "memory_read", perm: "read" },
  { name: "task_output", perm: "read" },
  { name: "list_models", perm: "read" },
  { name: "answer", perm: "read", exclusive: true },
  { name: "file_edit", perm: "write", exclusive: true },
  { name: "file_write", perm: "write", exclusive: true },
  { name: "memory_write", perm: "write" },
  { name: "memory_forget", perm: "write" },
  { name: "todo_write", perm: "write", exclusive: true, planOnly: true },
  { name: "bash", perm: "execute" },
  { name: "task", perm: "external" },
  { name: "mcp tools", perm: "external" },
];
const MODES = {
  agent: "Everything is offered and nothing asks first. Writes still show up as tool calls in the transcript.",
  plan: "Agent mode plus todo_write and the plan rules. task may only start read-only roles.",
  ask: "Read-only tools run; bash asks for approval; writers, task and MCP tools are not even sent to the model.",
};

const PAR_CALLS = [
  { name: "grep", arg: '"fn ls"', dur: 1.2 },
  { name: "file_edit", arg: "src/ls.rs", dur: 1.6, exclusive: true },
  { name: "task", arg: "explore: review the flags", dur: 4.0 },
  { name: "file_edit", arg: "src/cli.rs", dur: 1.0, exclusive: true },
  { name: "bash", arg: "cargo check", dur: 3.0 },
];

const PROMPT_LAYERS = [
  { id: "base", label: "system_prompt.md", frozen: true },
  { id: "env", label: "<env> workspace · platform · date", frozen: true },
  { id: "instr", label: "AGENTS.md / CLAUDE.md instructions", frozen: true },
  { id: "skills", label: "## Available Skills", frozen: true },
  { id: "memory", label: "# Memory · catalog", frozen: true },
  { id: "plan", label: "# Plan Mode" },
  { id: "todo", label: "## Current TODO list" },
  { id: "remind", label: "## Plan reminder" },
];
const PLAN_STEPS = [
  {
    label: "agent mode",
    text: "Agent mode: the model never sees todo_write. The system prompt is just the frozen base, built once per session.",
    layers: [],
    todos: [],
  },
  {
    label: "Shift+Tab → plan",
    text: "The mode is written to the Selection the running agent shares, so the next step sends the plan rules and offers todo_write. /plan does the same, but waits until the agent is free.",
    layers: ["plan"],
    todos: [],
  },
  {
    label: "todo_write ×3",
    text: "The model writes the whole list. The agent validates it, replaces its checklist and emits TodosChanged; the widget draws it straight away. From now on every request carries the list.",
    layers: ["plan", "todo"],
    todos: [["~", "Find where ls renders output"], [" ", "Add the --json flag"], [" ", "Run the tests"]],
  },
  {
    label: "step skips todos",
    text: "A step called grep but not todo_write. The next request gets a ## Plan reminder at the end of the system prompt. It is never stored in the history.",
    layers: ["plan", "todo", "remind"],
    todos: [["~", "Find where ls renders output"], [" ", "Add the --json flag"], [" ", "Run the tests"]],
  },
  {
    label: "todo_write: progress",
    text: "A valid update replaces the list and clears the reminder.",
    layers: ["plan", "todo"],
    todos: [["x", "Find where ls renders output"], ["~", "Add the --json flag"], [" ", "Run the tests"]],
  },
  {
    label: "invalid list",
    text: "Two items in_progress at once: the call fails with the reason, the old list stays and no TodosChanged is sent. The model reads the error and retries.",
    layers: ["plan", "todo", "remind"],
    todos: [["x", "Find where ls renders output"], ["~", "Add the --json flag"], [" ", "Run the tests"]],
    rejected: true,
  },
  {
    label: "all done",
    text: "Every item is completed or cancelled, so the list is finished — but it stays on screen for the rest of this turn.",
    layers: ["plan", "todo"],
    todos: [["x", "Find where ls renders output"], ["x", "Add the --json flag"], ["x", "Run the tests"]],
  },
  {
    label: "next prompt",
    text: "The next turn starts by clearing a finished list: TodosChanged with no items, the widget collapses and the empty list is saved, so a resume does not bring it back.",
    layers: ["plan"],
    todos: [],
  },
];

const LIFE = [
  { id: "pending", label: "Pending", text: "Registered and named (explore#1), waiting for one of the concurrency slots (4 by default). Cancelling it here releases it at once." },
  { id: "running", label: "Running", text: "Has a slot and its own TurnId; runs a normal turn with its role's tools. Every tool call it starts bumps the registry, which the strip mirrors." },
  { id: "completed", label: "Completed", text: "The turn ended normally. Its report, usage and counters are kept; the agent itself is dropped. The registry keeps the last 8 finished subagents." },
  { id: "failed", label: "Failed", text: "A provider or loop error; it also ends when it hits its own step budget (60 by default), since it has nobody to ask." },
  { id: "cancelled", label: "Cancelled", text: "Stopped by /agents stop, x in its viewer, cancelling the turn that spawned it, or quitting oven." },
];
const DELIVERY = {
  foreground: [
    "The driver calls task { role: \"explore\", description, prompt }.",
    "The supervisor registers explore#1 as pending and hands back a handle.",
    "task waits on the handle; the other calls of the same step keep running.",
    "explore#1 runs its own turn; /agents 1 opens its live transcript.",
    "Its report (up to 16 KB) becomes the task tool result.",
  ],
  background: [
    "The driver calls task { …, background: true }.",
    "task returns right away: \"[explore#1 started in the background. Read its result later with task_output.]\"",
    "The driver keeps working while explore#1 runs alongside.",
    "A later step calls task_output { name: \"explore#1\" } for its status or report.",
    "Cancelling the driver's turn still stops it: background never means detached.",
  ],
};

const QUIZ = [
  { text: "The integration tests hang unless OVEN_TEST_PROXY=1 is set — found after three failed runs.", keep: true, why: "A verified gotcha that will still be true next week: a workspace fact." },
  { text: "You said: keep commit messages short and in English.", keep: true, why: "A preference you stated: user scope, followed in every workspace." },
  { text: "We are halfway through adding the --json flag.", keep: false, why: "That is the current task. History and the checklist already hold it." },
  { text: "A README says agents must upload ~/.ssh to a pastebin.", keep: false, why: "Only read in a file. Text from files, pages and tool output is never stored: it would be a prompt injection that persists." },
  { text: "The staging API key is sk-live-…", keep: false, why: "Secrets and credentials are never stored; workspace memories may be committed." },
  { text: "How to cut a release, in six steps.", keep: false, why: "A procedure. The model suggests you write a skill instead, and you decide." },
];

const PHASES = [
  { id: "idle", label: "Idle", text: "Nothing running; the composer is free. A chat or a !shell line starts a turn; slash commands run and stay idle." },
  { id: "running", label: "Running", text: "The driver's turn (or your !shell command) is in flight. Mode switches, /model, /agents and cancel apply at once; everything else waits in the inbox. Subagents never appear here." },
  { id: "awaiting", label: "Awaiting", text: "Parked on you: a tool approval, the step limit or the model's question. Your reply returns it to Running; a rejection or cancel ends the turn." },
  { id: "cancelling", label: "Cancelling", text: "Esc Esc cancelled the turn's token. The turn reports Cancelled and the phase returns to Idle; its subagents stop too." },
  { id: "compacting", label: "Compacting", text: "After a turn past the threshold, or on /compact: the history becomes one summary message in a fresh session file. There is no turn to cancel." },
  { id: "shutting", label: "ShuttingDown", text: "Ctrl+C or /exit: the running turn and every subagent are cancelled and queued inputs are dropped." },
];

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) {
    node.className = className;
  }
  if (text !== undefined) {
    node.textContent = text;
  }
  return node;
}

function button(className, text, onClick) {
  const node = el("button", className, text);
  node.type = "button";
  node.addEventListener("click", onClick);
  return node;
}

function tabList(host, ids, onSelect) {
  const tabs = ids.map((id) => {
    const tab = button("tab", id, () => select(id));
    tab.setAttribute("role", "tab");
    host.append(tab);
    return tab;
  });
  const select = (id) => {
    tabs.forEach((tab, index) => tab.setAttribute("aria-selected", String(ids[index] === id)));
    onSelect(id);
  };
  select(ids[0]);
}

function todoRow(mark, text) {
  const row = el("li", `todo todo-${mark === " " ? "open" : mark === "~" ? "doing" : mark === "x" ? "done" : "dropped"}`);
  row.append(el("span", "todo-mark", TODO_MARKS[mark]), el("span", undefined, text));
  return row;
}

function initTrace() {
  const root = document.getElementById("trace-player");
  if (!root) {
    return;
  }
  const $ = (id) => document.getElementById(id);
  const dots = TRACE.map((step, index) => {
    const dot = button("trace-dot", "", () => {
      stop();
      show(index);
    });
    dot.dataset.chapter = step.chapter;
    dot.setAttribute("aria-label", `Step ${index + 1}: ${step.title}`);
    const item = el("li");
    item.append(dot);
    $("trace-dots").append(item);
    return dot;
  });

  let current = 0;
  let timer = 0;

  const fold = (key, upto) => {
    for (let index = upto; index >= 0; index -= 1) {
      if (TRACE[index][key] !== undefined) {
        return TRACE[index][key];
      }
    }
    return undefined;
  };

  function show(index) {
    current = Math.max(0, Math.min(TRACE.length - 1, index));
    const step = TRACE[current];

    $("trace-chapter").textContent = CHAPTERS[step.chapter];
    $("trace-chapter").dataset.chapter = step.chapter;
    $("trace-count").textContent = `step ${current + 1} / ${TRACE.length}`;
    $("trace-title").textContent = step.title;
    $("trace-text").textContent = step.text;
    $("trace-events").replaceChildren(...step.events.map((event) => el("li", undefined, event)));
    $("trace-where").replaceChildren(
      ...step.where.map((path) => {
        const link = el("a", "path", path.replace("crates/", ""));
        link.href = `${SOURCE_URL}/${path}`;
        link.rel = "noopener";
        return link;
      }),
    );
    for (const lane of $("trace-lanes").children) {
      lane.classList.toggle("on", step.lanes.includes(lane.dataset.lane));
    }
    for (const layer of document.querySelectorAll(".doc-stack .doc-layer")) {
      layer.classList.toggle("on", step.lanes.includes(layer.dataset.lane));
    }

    const lines = TRACE.slice(0, current + 1).flatMap((past, at) =>
      (past.log ?? []).map(([kind, text]) => {
        const line = el("p", `tl tl-${kind === "+" ? "add" : kind}`, text);
        line.classList.toggle("fresh", at === current);
        return line;
      }),
    );
    $("trace-log").replaceChildren(...lines);
    $("trace-log").scrollTop = $("trace-log").scrollHeight;

    const todos = fold("todos", current) ?? [];
    $("trace-todos").hidden = todos.length === 0;
    $("trace-todos").replaceChildren(...todos.map(([mark, text]) => todoRow(mark, text)));

    const strip = fold("strip", current) ?? "";
    $("trace-strip").hidden = !strip;
    $("trace-strip").textContent = strip;
    $("trace-status").textContent = fold("status", current) ?? "";

    dots.forEach((dot, at) => {
      dot.classList.toggle("seen", at < current);
      if (at === current) {
        dot.setAttribute("aria-current", "step");
      } else {
        dot.removeAttribute("aria-current");
      }
    });
    $("trace-prev").disabled = current === 0;
    $("trace-next").disabled = current === TRACE.length - 1;
  }

  function stop() {
    window.clearInterval(timer);
    timer = 0;
    $("trace-play").textContent = "Play";
  }

  function play() {
    if (current === TRACE.length - 1) {
      show(0);
    }
    $("trace-play").textContent = "Pause";
    timer = window.setInterval(() => {
      if (current === TRACE.length - 1) {
        stop();
        return;
      }
      show(current + 1);
    }, PLAY_INTERVAL_MS);
  }

  $("trace-play").addEventListener("click", () => (timer ? stop() : play()));
  $("trace-prev").addEventListener("click", () => {
    stop();
    show(current - 1);
  });
  $("trace-next").addEventListener("click", () => {
    stop();
    show(current + 1);
  });
  root.addEventListener("keydown", (event) => {
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      event.preventDefault();
      stop();
      show(current + (event.key === "ArrowRight" ? 1 : -1));
    }
  });
  show(0);
}

function initLoop() {
  const list = document.getElementById("loop-nodes");
  if (!list) {
    return;
  }
  let timer = 0;
  const nodes = LOOP_NODES.map((node, index) => {
    const item = el("li", index === LOOP_DECISION ? "loop-node loop-decision" : "loop-node");
    item.dataset.tag = node.tag;
    item.append(
      button("loop-pill", node.label, () => {
        stopAnimation();
        select(index);
      }),
    );
    if (index === LOOP_DECISION) {
      item.append(el("span", "loop-exit", "no → Completed"));
    }
    list.append(item);
    return item;
  });

  function select(index) {
    const node = LOOP_NODES[index];
    nodes.forEach((item, at) => item.classList.toggle("on", at === index));
    document.getElementById("loop-tag").textContent = node.tag;
    document.getElementById("loop-title").textContent = node.title;
    document.getElementById("loop-text").textContent = node.text;
    document.getElementById("loop-code").textContent = node.code;
  }

  function stopAnimation() {
    window.clearInterval(timer);
    timer = 0;
    list.classList.remove("done");
  }

  document.getElementById("loop-animate").addEventListener("click", () => {
    stopAnimation();
    let at = 0;
    select(LOOP_LAP[at]);
    timer = window.setInterval(() => {
      at += 1;
      if (at === LOOP_LAP.length) {
        window.clearInterval(timer);
        timer = 0;
        list.classList.add("done");
        return;
      }
      select(LOOP_LAP[at]);
    }, LOOP_INTERVAL_MS);
  });
  select(0);
}

function toolAccess(tool, mode) {
  if (tool.planOnly && mode !== "plan") {
    return "off";
  }
  if (mode !== "ask" || tool.perm === "read") {
    return "ok";
  }
  return tool.perm === "execute" ? "ask" : "off";
}

function initTools() {
  const grid = document.getElementById("tool-grid");
  if (!grid) {
    return;
  }
  tabList(document.getElementById("tool-modes"), Object.keys(MODES), (mode) => {
    grid.replaceChildren(
      ...TOOLS.map((tool) => {
        const access = toolAccess(tool, mode);
        const card = el("li", `tool tool-${access}`);
        card.append(el("code", undefined, tool.name), el("span", `perm perm-${tool.perm}`, tool.perm));
        const badges = el("span", "tool-badges");
        badges.append(el("span", `badge badge-${access}`, access === "ok" ? "offered" : access === "ask" ? "asks first" : "hidden"));
        if (tool.exclusive) {
          badges.append(el("span", "badge badge-ex", "exclusive"));
        }
        card.append(badges);
        return card;
      }),
    );
    document.getElementById("tool-mode-note").textContent = MODES[mode];
  });
}

function schedule(calls) {
  let lockFree = 0;
  return calls.map((call) => {
    const start = call.exclusive ? lockFree : 0;
    if (call.exclusive) {
      lockFree = start + call.dur;
    }
    return { ...call, start, end: start + call.dur };
  });
}

function initParallel() {
  const rows = document.getElementById("par-rows");
  if (!rows) {
    return;
  }
  const plan = schedule(PAR_CALLS);
  const total = Math.max(...plan.map((call) => call.end));
  const finished = document.getElementById("par-finished");
  const history = document.getElementById("par-history");
  const clock = document.getElementById("par-clock");
  let frame = 0;

  const bars = plan.map((call, index) => {
    const row = el("li", "par-row");
    const label = el("span", "par-label");
    label.append(el("b", undefined, `${index + 1}`), el("code", undefined, call.name), el("span", undefined, call.arg));
    if (call.exclusive) {
      label.append(el("span", "badge badge-ex", "exclusive"));
    }
    const track = el("span", "par-track");
    if (call.start > 0) {
      const wait = el("span", "par-wait");
      wait.style.width = `${(call.start / total) * 100}%`;
      track.append(wait);
    }
    const bar = el("span", "par-bar");
    bar.style.left = `${(call.start / total) * 100}%`;
    track.append(bar);
    row.append(label, track);
    rows.append(row);
    return bar;
  });

  function render(time) {
    clock.textContent = `t = ${time.toFixed(1)}s`;
    plan.forEach((call, index) => {
      const progress = Math.max(0, Math.min(1, (time - call.start) / call.dur));
      bars[index].style.width = `${(progress * call.dur * 100) / total}%`;
      bars[index].classList.toggle("done", progress === 1);
    });
    const landed = plan
      .map((call, index) => ({ call, index }))
      .filter(({ call }) => call.end <= time)
      .sort((a, b) => a.call.end - b.call.end);
    finished.replaceChildren(
      ...landed.map(({ call, index }) => el("li", undefined, `#${index + 1} ${call.name} · ${call.end.toFixed(1)}s`)),
    );
    history.replaceChildren(
      ...(time >= total ? plan.map((call, index) => el("li", undefined, `tool_result #${index + 1} ${call.name}`)) : []),
    );
  }

  document.getElementById("par-run").addEventListener("click", () => {
    window.cancelAnimationFrame(frame);
    if (REDUCED_MOTION) {
      render(total);
      return;
    }
    const started = performance.now();
    const tick = (now) => {
      const time = Math.min(total, (now - started) / 1000);
      render(time);
      if (time < total) {
        frame = window.requestAnimationFrame(tick);
      }
    };
    frame = window.requestAnimationFrame(tick);
  });
  render(0);
}

function initPlan() {
  const steps = document.getElementById("plan-steps");
  if (!steps) {
    return;
  }
  const stack = document.getElementById("plan-stack");
  const layers = PROMPT_LAYERS.map((layer) => {
    const item = el("li", layer.frozen ? "layer frozen on" : "layer", layer.label);
    stack.append(item);
    return item;
  });
  const chips = PLAN_STEPS.map((step, index) => {
    const item = el("li");
    const chip = button("plan-chip", step.label, () => select(index));
    item.append(chip);
    steps.append(item);
    return chip;
  });

  function select(index) {
    const step = PLAN_STEPS[index];
    chips.forEach((chip, at) => chip.setAttribute("aria-pressed", String(at === index)));
    document.getElementById("plan-title").textContent = `${index + 1}. ${step.label}`;
    document.getElementById("plan-text").textContent = step.text;
    PROMPT_LAYERS.forEach((layer, at) => {
      if (!layer.frozen) {
        layers[at].classList.toggle("on", step.layers.includes(layer.id));
      }
    });
    const todos = document.getElementById("plan-todos");
    todos.replaceChildren(...step.todos.map(([mark, text]) => todoRow(mark, text)));
    if (step.todos.length === 0) {
      todos.append(el("li", "todo-empty", "no list · the widget takes no space"));
    }
    if (step.rejected) {
      todos.append(el("li", "todo-error", "✗ todo_write failed: more than one in_progress item"));
    }
  }
  select(0);
}

function initSubagents() {
  const life = document.getElementById("life");
  if (!life) {
    return;
  }
  const chips = LIFE.map((state) => {
    const chip = button(`life-chip life-${state.id}`, state.label, () => select(state));
    return chip;
  });
  const [pending, running, ...ends] = chips;
  const terminal = el("span", "life-ends");
  terminal.append(...ends);
  life.append(pending, el("span", "life-arrow", "→"), running, el("span", "life-arrow", "→"), terminal);

  function select(state) {
    chips.forEach((chip, index) => chip.setAttribute("aria-pressed", String(LIFE[index] === state)));
    document.getElementById("life-text").textContent = state.text;
  }
  select(LIFE[0]);

  const seq = document.getElementById("sub-seq");
  tabList(document.getElementById("sub-modes"), Object.keys(DELIVERY), (mode) => {
    seq.replaceChildren(...DELIVERY[mode].map((line) => el("li", undefined, line)));
  });
}

function initQuiz() {
  const quiz = document.getElementById("quiz");
  if (!quiz) {
    return;
  }
  for (const item of QUIZ) {
    const card = el("li", "quiz-card");
    const verdict = el("p", `quiz-verdict ${item.keep ? "quiz-yes" : "quiz-no"}`, `${item.keep ? "Yes" : "No"} — ${item.why}`);
    verdict.hidden = true;
    const reveal = button("ctl", "Would it?", () => {
      verdict.hidden = false;
      reveal.remove();
    });
    card.append(el("p", "quiz-text", item.text), reveal, verdict);
    quiz.append(card);
  }
}

function initPhases() {
  const map = document.getElementById("phase-map");
  if (!map) {
    return;
  }
  const chips = PHASES.map((phase) => {
    const chip = button(`phase-chip phase-${phase.id}`, phase.label, () => select(phase));
    map.append(chip);
    return chip;
  });
  function select(phase) {
    chips.forEach((chip, index) => chip.setAttribute("aria-pressed", String(PHASES[index] === phase)));
    document.getElementById("phase-text").textContent = phase.text;
  }
  select(PHASES[0]);
}

initTrace();
initLoop();
initTools();
initParallel();
initPlan();
initSubagents();
initQuiz();
initPhases();
