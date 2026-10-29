# Changelog

## [Unreleased]

### Added
- `oven-proto`: a session file names the format it was written in — `SessionMeta::format_version` records it, and `SessionMeta::is_readable_by_this_build` is the rule: a file a newer oven wrote is refused rather than half-understood
- `oven-proto`: `Record::into_version` writes a record in a given format, so a session keeps the format it was written in: a file resumed from an older one is written in that format, and what that format does not know, it does not get
- `oven-app`, `oven-tui`: a session records the directory it was started in, and resuming it in another one says so — the paths its conversation names mean something else there. A file written before that was recorded names no directory, and there is nothing to compare it against

### Changed
- **Breaking:** `oven-proto`: `SessionMeta` names the format its file was written in and the directory the session was started in — a struct literal has to name both
- **Breaking:** `oven-app`: `SessionError` grew `UnsupportedFormat` — a file written in a format this build does not read is refused instead of being read halfway

## [0.1.3] - 2026-10-28

### Added
- `oven-proto`: `AgentError` joins the vocabulary `oven-app` hands a frontend — it rides in `TurnEvent::Failed`, so a frontend that renders the event can name the type without depending on `oven-proto` itself
- `oven-app`: `CommandResult` says what the app did with a submission — `Submitted`, `Parked` and `Unparked` each carry the input, so a frontend draws it the way the runtime will treat it
- `oven-app`: `App::prompt` is a `Headless` client now: one implementation reads the answer off the event stream, and any frontend without a screen reuses it instead of writing the loop again
- `oven-llm`: `complete_streaming()` drives a streamed call and returns the assembled response — every event goes to the callback first and is collected afterwards, so what the caller saw and what the response holds cannot drift apart
- `oven-llm`: `Response::tool_calls()`, `has_tool_calls()` and `stopped_for_tools()` answer the questions a loop asks about a reply, and `ToolUse::arguments()` reports a truncated argument object instead of handing the caller an empty one
- `oven-agent`: a reply that cannot end the turn is logged with the provider's own words — the stop reason it reported, how many content blocks arrived and the output tokens it counted. Every request's debug line carries the stop reason too, so a reply that came back without one is visible in the log and not only in the failure that follows it

### Changed
- **Breaking:** `oven-app`, `oven-tui`: every frontend request goes through `App::apply(Command)` — a submission, a steer, a rewind, a cancel, a mode switch, an answer, a subagent to stop — so the terminal and a second frontend share one contract. The command carries a classified `Input` and `App::classify` is the only classifier, so no text is read twice
- `oven-tui`: the terminal's key handler is synchronous, so the requests wait in an outbox until the event loop sends them; the ones the last key made go out before the app is shut down, so a cancel is never lost
- `oven-app`: the event bus hands every subscriber the same `Arc<AppEvent>`, so a stream of text deltas and a 64 KB tool result cost one reference count rather than a copy each
- **Breaking:** `oven-llm`: `ToolUse` is a type now — it carries the id, the name, the arguments and the raw wire text, and keeps the cache-hit contract to itself, so the wire text the model generated is no longer a public field every caller had to know about. The JSON of a content block is unchanged, so stored conversations still load
- **Breaking:** `oven-llm`: the request methods say what they do — `push_*` adds one message or tool, `set_*` replaces the collection
- **Breaking:** `oven-llm`: `Message::system_prompt()` is gone; `Message::text()` reads the text of any message and the role says whether it is the system one, and `Request.system` is the one home of a system prompt
- `oven-llm`: the request asks for the model's declared output cap — the provider's own default is lower than the model allows and cut a long answer off mid-sentence
- `oven-llm` is published as 0.7.0: the typed `ToolUse`, the `push_*`/`set_*` request methods, the provider's own words in a refused request and the `IncompleteStream` decode error above ship in it
- `oven-tui`: a turn that stops before the model answered has one shape — the reason first, then the line that says how it ended and how long it ran: a failure is a red row with the reason followed by `Failed after 1.5s`, a cancellation a dim `cancelled` row followed by `Cancelled after 1.5s`, and a turn that finished keeps `Worked for 1.5s`
- The minimum supported Rust version is 1.94, and CI caches with `actions/cache` v5

### Fixed
- A refused request says why, in the provider's own words: a provider that answers with `{"error":{"message":"..."}}` gets its message read out of it, a body that is not JSON is collapsed onto one line and cut to 200 characters, and a body that says nothing falls back to the status
- A rate limit says what was exceeded and how long to wait: the `Retry-After` header is read, and `ProviderError::RateLimit` carries the reason beside the wait instead of dropping it
- A turn that ends with no answer says so: a reply that carries neither text nor a tool call fails the turn instead of reading as the model's final answer, and the reply stays out of the history, so the next turn starts from the last thing that happened
- `oven-llm`: a Chat Completions stream that ends before the provider said why is `CompletionsDecodeError::IncompleteStream` now, the way the Responses protocol already reports a stream that ends without `response.completed` — a truncated reply no longer reads as an answer
- The reason a request failed reaches the screen: `ProviderError::Stream`, `InvalidRequest`, `CompletionsEncode` and `ResponsesEncode` keep it in the message now and not only in the source chain, so the row says `provider: api error (500): boom` and not `failed: stream protocol error`
- An answer that was cut off is marked with `…`, so a failed half-answer is no longer flushed as if it were whole
- An exclusive tool call runs on its own: the lock is a read-write lock now, so an exclusive call waits for the calls that share its step and they wait for it — a `file_write` no longer runs beside a `file_read` of the same file
- An edit that lands shows its diff in red and green: the diff a file edit leaves behind wears its sign first, the way the frontend reads a diff, and names the line the way `file_read` reports one

## [0.1.2] - 2026-10-24

### Added
- `oven-app`: quality profiles — a name we choose (`balanced`, `deep`…) and what it means for each vendor, so the same quality is a different model on a different vendor. `/profile <name>` switches to one, `/profile` lists what the active vendor has, and a vendor without that binding is refused rather than quietly kept
- `oven-llm`: replay tests that pin both halves of a call — a recorded exchange per vendor (`kimi-k3`, `kimi-k2.6`, `MiniMax-M3`, `grok-4.6`) replays the request body and the response through the encoders and the decoders, so a change in either shows up as a failing test instead of a broken call
- `oven-llm`: MiniMax is a vendor now — `minimax` in the setup wizard, the OpenAI-compatible endpoint (`https://api.minimax.io/v1`), and the reasoning parameters MiniMax actually takes: `thinking.type` on the models that can turn thinking off, `reasoning_effort` on the ones that take a depth, and `reasoning_split` on every request so the thinking comes back as thinking rather than as text
- `oven-proto`: the protocol vocabulary moves into a crate of its own — the events, the state and the commands a frontend shares with the runtime. It is a leaf, on `oven-llm` and serde alone, so a frontend can be written without the agent runtime in its dependency tree. Documented in [`docs/oven-proto.md`](docs/oven-proto.md)
- `Command` is one enum for every request a frontend can make, and `App::apply` applies one: `Submit`, `Steer`, `Rewind`, `Cancel`, `CancelCompaction`, `SetMode`, `Respond`, `StopSubagent`, `StopSubagents`. A transport turns what it received into a `Command` and nothing else, and `CommandResult::Refused` says the request did not apply — the driver was idle, or the request it answered was already closed
- `AppState` splits in two: `levels` holds the phase, the mode, the model, the provider, the checklist and the usage — small, and cheap enough to forward on every change — and the conversation stays beside it, read once when a frontend connects
- `App` is cloneable and shuts down by reference, so a server can hand one session to several connections
- Every protocol type has a serde form, and the wire format is pinned by tests
- The releases and changelog pages on the site draw a skeleton while they load, so the page does not jump when the data arrives

### Changed
- `oven-app`: a manual `/model` unbinds the profile — the user asked for a model, not for a quality level — and switching vendors re-expands the profile for the new vendor
- **Breaking:** `oven-llm`: the sampling parameters are no longer sent by default — the vendor's own default is the right one, and the Kimi family fixes its temperature per model (thinking 1.0, non-thinking 0.6) and rejects any other value
- `oven-llm`: the Moonshot and xAI reasoning parameters are calibrated from the vendor docs — Kimi K3 takes `reasoning_effort` (`low`/`high`/`max`, no `medium`), `kimi-k2.6` takes the `thinking.type` switch, `kimi-k2.7-code` always thinks and takes neither, and the Grok models with no effort option say so
- `oven-llm`: a model whose reasoning depth is fixed gets no reasoning field at all — a new `EffortSupport::Fixed`, next to `Unknown`, `Unsupported`, `Toggle` and `Levels`
- `oven-tui`: the status bar shows the effort as it was asked for; a switch-only model reads as `on`, and a model that does not reason (or whose depth is fixed) shows no effort at all
- **Breaking:** `oven-llm`: the request and the options are separate arguments — `Provider::complete` and `Provider::stream` take `(&Request, &CallOptions)`. The request is the conversation; the options name the model it goes to and how to ask (sampling, reasoning effort, private fields). The same conversation can go out to another model or with other options, and a turn rebuilds only the conversation
- **Breaking:** `oven-llm`: the model moves from the request to the options — `CallOptions::new(model)` is the only way to make one, so a call always names its model, and `Request::builder().build()` cannot fail any more: `Request` is the conversation alone, and `BuilderError` is gone
- **Breaking:** `oven-llm`: `ModelId` moves out of `domain::request` into `domain::model`; the crate-root paths are unchanged
- **Breaking:** `oven-llm`: `SamplingParams` and `CallOptions` move out of `domain::request` into `domain::options`; the crate-root paths are unchanged
- `oven-llm`: the model's own capability decides which reasoning channel a request uses — a model with levels takes the level channel (`reasoning_effort`), a model with only a switch takes the switch channel (`thinking.type`), and a model nobody knows falls back to the vendor's dialect: Zhipu gets `thinking.type` and no `reasoning_effort`, OpenAI and DeepSeek get `reasoning_effort` and no `thinking`, and a vendor nobody verified keeps sending both. MiniMax follows the model instead: `MiniMax-M3` has a switch, `MiniMax-M3.1-Flash` has levels. A level the model does not have is clamped down to the closest one it has, and the request path never fails on it
- **Breaking:** `oven-proto`: `AppLevels.models` is a list of `ModelChoice` — id, provider, and what the model can express — rather than a list of pairs, so a frontend can offer the levels a model actually takes
- `oven-app`, `oven-tui`: the model picker lists the reasoning levels the model takes and nothing else, and the status bar reads the effort the way the model reads it: a switch-only model shows `on`, and a level the model does not take shows the one it will use
- **Breaking:** `oven-llm`: the reasoning effort is one axis — `ReasoningEffort::None` is now `Off` (the wire word stays `"none"`), and `Minimal`, `XHigh`, `Max` and `Budget(u32)` join it. An effort that is not set means "no preference" and reaches the wire as no field at all; before, the Responses protocol turned it into `effort: "none"` and forced the upstream default off
- **Breaking:** `oven-llm`: `Request::thinking` and `ThinkingMode` are gone — the thinking switch is derived from the effort, so the two protocols can no longer disagree about whether reasoning is on
- `oven-llm`: a compaction asks for `Off` instead of switching the thinking switch off by hand
- `oven-app`, `oven-tui`: `/model` and the model picker take the whole vocabulary (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, and a token budget written as a number), and an effort that is not set reads as "unspecified" rather than "none"
- **Breaking:** `AppState` is `{ levels, history }` rather than one flat struct, and `AppState::provider` is a `ProviderView` — the API key stays in the app layer, and a frontend is told only whether a credential is configured
- **Breaking:** `App::provider_config` is gone; `App::provider_view` hands out the credential-free view instead, so no public method on the facade returns the key
- **Breaking:** `App::respond` returns whether the answer was delivered — `false` when the request was already closed, or the answer did not fit it
- `AppState` and `AppLevels` have a serde form, so a transport forwards them as bytes
- **Breaking:** `NodeInfo::elapsed_ms`, `NodeInfo::progress` and `NodeInfo::notice` take the current time (`elapsed_ms(now_ms)`): a protocol crate does not read the clock
- **Breaking:** `oven-agent` no longer re-exports the protocol vocabulary — `oven_agent::AgentEvent` and the rest are gone, and a crate that names an event depends on `oven-proto` for it. `oven-app` keeps re-exporting what it hands a frontend, so `oven_app::AppEvent` still resolves
- `oven-llm` talks HTTP over `reqwest` instead of `isahc` (0.6.0): the crate now needs a Tokio runtime, `ProviderError::Transport` carries a `reqwest::Error`, and `ProviderError::Io` is gone. The `curl-sys`, `libz-sys` and `libnghttp2-sys` builds leave the tree, so a build needs no C toolchain and no CMake
- The Windows release artifact is built with the MSVC toolchain and is named `oven-<tag>-x86_64-pc-windows-msvc.zip`; an ARM64 build (`aarch64-pc-windows-msvc`) ships beside it. The `x86_64-pc-windows-gnu` target is dropped, and the PowerShell installer picks the MSVC build the machine needs
- `oven-llm` is published as 0.6.0: the vendor profiles, the MiniMax vendor, the one-axis effort and the `reqwest` transport above ship in it
- **Breaking:** the built-in tools move from `oven-agent` to `oven-app`: `file_read`, `file_write`, `file_edit`, `bash`, `glob`, `grep`, `web_fetch`, `todo_write`, `answer`, `task`, `task_output`, `list_models` and `skill_read` are reached through `oven_app::…` now, and `BUILTIN_TOOLS` and `present_tool` live there too. `oven-agent` keeps the `Tool` protocol and the nouns around it, so the driver never names a tool it was not handed
- **Breaking:** `Skill` and `SkillRegistry` move to `oven-app` as well. The agent layer never consumed a skill — it took a rendered list for the system prompt and nothing else — so the registry and the `skill_read` tool belong with the rest of the app's capabilities
- `Tool::effect` says what a call leaves behind: `todo_write` hands back the checklist it wrote, and the driver records it and emits `TodosChanged` without naming the tool
- `Tool::view` falls back to the tool's own name, so a tool describes itself instead of being looked up in a global table
- `oven-tui`: the transcript renders incrementally — a streaming reply's Markdown is parsed once per line instead of once per frame, so the cost of a reply stops growing with its length (17.0 ms → 1.6 ms over 200 frames on a 4 kB body)
- `oven-tui`: the transcript goes through the terminal's diff instead of being repainted whole — only the cell after a wide glyph is forced, the one a Windows terminal may leave stale, and an unchanged frame draws nothing
- `oven-tui`: a drawn glyph no longer allocates a string per character — the glyph is kept as a `char` and turns into owned text only when it does not fit the shape
- `oven-tui`: the composer popups are refreshed once per keystroke instead of twice, and the agent strip counts its rows instead of building them to measure them
- `oven-app`: the published state is synced once per turn instead of twice — a turn no longer walks the whole conversation three times and asks the frontend to redraw twice
- `oven-llm`: a tool's JSON schema is shared instead of copied per request — an MCP tool no longer copies the tree the handshake already fetched, and the encoder no longer copies it again on the way to the wire
- `oven-app`, `oven-mem`: a memory lookup answers from the index the store already holds instead of copying every entry of it to find one, and the budget check on a write stops copying them all again
- `oven-app`: a provider is normalized once on its way into the config instead of twice, and asking whether a key is configured no longer copies the key to find out
- `oven-app`: the memory reference lives in one place — how a reference is parsed, which labels a memory is shown with, the header both renderings open with, and the schema every memory tool carries
- `oven-app`: the requested tools are looked up in a set instead of scanning the name list per builtin tool, and an MCP server hands out one caller its tools share
- The release profile builds with one codegen unit (`codegen-units = 1`), trading build time for a smaller and faster binary

### Fixed
- Switching to a model that does not take the chosen reasoning effort now says so — the request path degrades silently, so the switch is where the difference is said out loud: `glm-5.3 only has a reasoning switch; high will be treated as on`
- GLM no longer receives a `reasoning_effort` it does not read: it takes `thinking.type` only, so the level used to be dropped in silence while the status bar showed it as if it had taken effect
- A `todo_write` call that was refused no longer rewrites the checklist: the driver takes the checklist from the tool it ran, so a call to a tool that is not mounted leaves the plan alone
- A config file's `max_iters` and `[subagents]` block are no longer dropped when the user and project configs merge — neither file could change them. An MCP server entry merges field by field too, so a project file that only tweaked a server's arguments no longer loses the user's command, environment and headers
- A shell command that times out stops the work and not just its shell: the process group (Unix) or the job object (Windows) is killed, and the output is returned instead of the read hanging on a detached descendant that never closes the pipe
- `oven -Q` reports a failed query on stderr — tracing only writes to the log file, so a failed headless run used to print nothing at all
- `oven model add` keeps a project API key out of the user config: the key is read from the user file, so the file that is written holds the user's own key, while the key that will run is still the project's when it has one
- The vendor menu in `oven model add` is built from the provider registry, so every vendor oven knows is offered — Anthropic was missing from the hand-written list
- A failed prompt is told apart from an empty answer: a cancelled prompt, a terminal that is not there, an input that ended and a read error are named, and an empty answer asks again since the field is required
- A `--protocol` on a known vendor says that it is ignored instead of passing silently, the provider argument is named the way it is passed (a positional), and the protocol error lists the `messages` protocol the parser accepts
- A table separator needs one dash, not three: the table rule and the code fence rule no longer share one constant

## [0.1.1] - 2026-10-22

### Added
- `file_edit` takes an ordered array of edits: `edit` is an array of `{old_string, new_string}` applied in order to the file as it stands, so one call makes several edits and each one lands on a match that is unique in the content that edit sees. A `old_string` that matches twice is refused, with the count and the file named
- The top-level agent rules leave the shared system prompt: `prompt_template/top_level.md` names the driver-only tools (`answer`, `task`) and the shared prompt no longer mentions them, so a subagent does not read rules meant for the driver. The env block drops the working directory, carries the UTC offset with the date, and detects a git repository in a parent directory
- A background subagent that finishes pushes a notice into the turn waiting on it: the driver is told which subagent is done, what it spent, and where to read the rest, instead of having to poll `task_output`. `NodeInfo::progress` and `NodeInfo::notice` render that line, and `task_output` quotes the same progress, so the two never describe one subagent differently
- The activity row stays open while its turn runs: the row shows the newest of its body as the turn streams, and the row budget lifts once the row is sealed
- The landing site gets a motion pass, the docs page a reveal and scroll pass, and the site its SEO metadata: an Open Graph image, `robots.txt` and `sitemap.xml`

### Changed
- **Breaking:** `Tool::run` returns `ToolOutcome { output, detail }` — the text history keeps, and the body a frontend shows under the call once it lands — and `Tool::result_detail` is gone. `file_edit` reports the diff it applied with a gutter naming the line each side sits on, so an edit row ends on where the edit landed instead of on the diff the model asked for; `ToolOutcome: From<String>` keeps a tool that has nothing to add a one-line change
- **Breaking:** `file_edit` takes `edit` — an ordered array of `{old_string, new_string}` — instead of one `old_string`/`new_string` pair and a `replace_all` flag. Every `old_string` must match exactly once, so replacing every occurrence is no longer a call; a prompt or a script written against the old shape has to be updated
- The shared system prompt is rewritten around working, tools and safety: it asks for the same language as the request, a `path/to/file.rs:120` reference, and an explicit statement of what could not be verified, and it reads a web page, an MCP response and a tool result as data rather than as instructions
- `oven-mem`: `format::render` returns `Result<String, MemoryError>`, and a frontmatter that cannot be rendered is the new `MemoryError::Render` instead of a panic
- A model that declares no context window is assumed to have a 1,000,000-token window (`DEFAULT_CONTEXT_WINDOW`), so the context readout and the auto-compaction threshold work for a vendor oven does not ship

### Fixed
- The stream caret stays on the last glyph of a reply. A trailing newline no longer opens a blank row under the paragraph, and a line that ends on the wrap width puts the caret on the next row
- The Markdown renderer handles `**bold**`, `_italic_` and `~~strike~~` again: the inline scanner is byte-based and knows the marker characters and the bold length, so a marker run that is not a pair stays literal text and a stray `*` no longer swallows the rest of the line. Every drawn glyph maps back to the source bytes it came from, so a selection copies the source range, and an invisible glyph such as an emoji joiner rides in a neighbour's text
- The activity row no longer folds its body away when a thought retires beside a tool call: a thought on its own keeps its body until the row is sealed
- The user config template is written and flushed before the test that reads it, so that check no longer fails on a slow filesystem

## [0.1.0] - 2026-10-14

### Added
- The transcript renders assistant Markdown: inline marks come from `pulldown-cmark`, and lists, quotes, fences, headings and tables from a line scan that keeps the source indentation as hanging wrap, with `code`, `link` and per-level `heading` styles. Assistant text rows and the live stream both reparse the whole message on each rewrap, and a mouse selection maps display columns back to the original Markdown — hidden markers left out, soft-break newlines not inserted. Thinking and tool output stay literal

### Changed
- **Breaking:** `Agent::compact` takes a `CancellationToken`. A cancelled or failed summary leaves history untouched
- **Breaking:** `AppConfig::load`, `load_file`, `save_at`, `save_provider_at`, and `ensure_user_config` are async, and so are `App::submit` and `App::rewind`. A `/model` switch writes config without blocking a runtime thread. The file format is unchanged
- The todo checklist and the plan reminder leave the system prompt: a checklist rides along as a user message on the single request after it changes, and a reminder is a user message on the request after `PLAN_REMINDER_AFTER_MISSES` tool rounds skip `todo_write`, after which the count restarts. Neither is stored, so the cached prefix stays byte-identical for the whole session, and `prompt_template::mode` composes the mode prompt over the base in place of the removed `plan` module
- Tool rows are named by verb — `Searched`, `Found`, `Edited`, `Wrote`, `Asked`, `Fetched` — so a call with malformed arguments groups with the siblings it belongs to instead of falling back to its raw tool name; `file_edit` keeps the path (without the diff) when its strings do not parse
- Release builds run on `ubuntu-24.04` and cache the cargo registry with `actions/cache`, keyed on the target and `Cargo.lock`

### Fixed
- Cancelling during `/compact` or auto-compaction aborts the summary request, including mid-stream, and leaves the session id, session file, and `cwd_latest.json` unchanged. Cancelling auto-compaction keeps the turn that already finished
- Config saves write a temporary file and rename it into place, so a crash or a concurrent save no longer leaves a truncated `config.toml`. Creating the template user config no longer races with another process creating it
- A cancelled turn keeps the prompt and every step that finished: the in-flight step is dropped so no tool call is left without a result, and usage, todos and session meta are saved for that prefix, so the first turn of a new session lands under `~/.oven/sessions` and can be resumed
- A text delta that is only a newline, CR or spaces no longer seals the open activity row. That text is never drawn, and with thinking off it arrives between tool steps, so each edit used to start a row of its own
- Table rows map every glyph back to its source, so a selection copies one cell instead of the whole line and a wide cell wraps inside its column rather than as prose
- A long prompt frame stays capped while scrolling it back into the transcript: the row cap only applied to the pinned header, so the box grew with every line
- The stream caret no longer jumps on wrapped Markdown: a glyph in the last column arms pending wrap, which opened a blank row under the text while a reply streamed
- The prompt box drawn after you press Enter now highlights selected text, and a long prompt scrolls inside that box.

## [0.0.11] - 2026-10-06

### Added
- `web_fetch` fetches an `http` or `https` URL and returns the page to the model as markdown. A failed fetch returns an error and writes nothing
- `crates/oven-llm` is vendored into the workspace and released on its own `oven-llm-v*` tag: the domain types (`domain/message.rs`, `request.rs`, `response.rs`, `stream.rs`, `tool.rs`), the completions and Responses clients, the retry loop, the vendor catalog and request validation, with oven depending on it by path while it keeps the metadata for an independent crates.io release
- `oven-llm` returns typed `ProviderError`s instead of panicking, stays runtime-agnostic (backoff and timeouts wait on `futures-timer`, tokio is only a dev-dependency), and ships `examples/agent_loop.rs`, `completions_usage.rs`, `responses_usage.rs` and `router_usage.rs`

### Changed
- **Breaking:** `Router::complete` and `Router::stream` return `ProviderError` and take optional retry and timeout; `RouterHandle` is shareable and registering a provider no longer needs a dummy API key. `Router` only records which provider owns a model — calls go through `Client`, which implements `Provider`, and retry lives on `RetryingProvider` applied when a provider is built
- **Breaking:** the duplicated router, catalog and provider code is gone from `oven-agent`: `core/retry.rs` and the local copies of the retry loop, model defaults, capability overrides and completions/Responses client shell are dropped, and `oven_llm::{ModelCatalog, Provider, ProviderBuilder, ProviderName, Request, Response, Router, RouterHandle, RetryingProvider, …}` is the single copy
- Thinking and tool calls fold into one activity row in the transcript (`widgets/transcript/activity.rs`): one run of thought and calls is a single collapsible row whose body is the timeline, and a later step's `file_edit` adds to the same burst's total instead of starting a new one
- The subagent strip takes the keyboard while the composer is empty: `↑`/`↓` move a highlight through the driver (`◇ main`) and the subagents, `Enter` opens the highlighted one, `Esc` returns to the driver, and the strip stays after one has been opened so arrows can move on — a rewindable message still keeps its two-press confirm. Inside the viewer `↑`/`↓` switch to the previous or next agent while `Shift+↑` scrolls the transcript line by line, and the hint row reads `↑↓ select · enter view · esc undo` / `↑↓ switch · esc back · pgup/pgdn scroll · x stop`
- The landing site adds a docs page, a releases page, release assets grouped by platform, and a Cloudflare worker deployment under `scripts/worker/`, and CI skips workflow runs for docs, scripts and markdown changes
- `--version` also names the build's target triple, from the `BUILD_TARGET` release CI sets or Cargo's own `TARGET`

### Fixed
- Streamed token usage survives a Responses turn and a failed one fails closed instead of reporting zero usage
- Invalid tool arguments no longer abort the turn: a truncated `{` keeps that call as a tool error, lets the others run, and replays `{}` so the next request stays valid

## [0.0.10] - 2026-10-03

### Added
- Durable memory in `oven-mem`: one markdown file per fact or preference under `<root>/.oven/memory` and `~/.oven/memory`. The catalog is appended to the system prompt, `memory_read`, `memory_write` and `memory_forget` are the model's tools, and `/memory` plus `oven mem` (`ls`, `show`, `rm`, `edit`) list, show and remove them. `oven --amnesia` mounts no tools, builds no store and adds nothing to the prompt for that run
- `oven model` configures providers without starting the TUI: `ls` lists every configured model next to the catalog `oven-llm` ships (and, with `--refresh`, what each endpoint reports), `add` walks a vendor into `~/.oven/config.toml` and `rm` drops a provider or one declaration. Every question of `add` is also a flag, so a script answers nothing and a missing required one fails naming it; the key is read without echo, a one-token request verifies the endpoint before anything is saved, and `--activate` is what switches the model — `add` otherwise leaves the selection alone. The subcommands live in `crates/oven-tui/src/commands/`, where config is still reached only through `oven-app`
- `commands/prompt.rs`: the line prompts a subcommand asks, behind a `Prompter` trait, so the walk is testable with scripted answers and a session with no terminal answers nothing instead of blocking
- `AppConfig::load_file` / `save_at` / `empty` / `remove_provider` / `remove_model`, and `provider_toml` for a preview with the API key masked: `save_provider_at` could only merge, so deleting a provider or a model had no path
- `oven_app::provider_catalog` reads the shipped catalog through a throwaway client with a placeholder key, `provider_models` is the real `GET /models`, and `verify` sends one token
- `ModelMetadata::from_info` and a public `ModelMetadata::merge_fields`
- A provider table can carry `context_window`, `max_output_tokens` and the `supports_*` flags directly, as defaults every `[providers.<slug>.models."<wire-id>"]` entry inherits wherever it leaves a field unset. A model that sets a field keeps its own value, an entry that only inherits is dropped when the file is rewritten, and the stored entries stay as written, so saving never bakes an inherited value into a model that only meant to follow the provider
- Create a landing page at `oven.paulden.site`

### Changed
- **Breaking:** `Session::open`, `resolve`, `load_records`, `append_records`, `overwrite`, `record_recent`, and `recent_session_id` are async. The JSONL layout is unchanged
- **Breaking:** `SkillRegistry::load_from_dirs`, `load_instructions`, `AppBuilder::load_config`, and `AppBuilder::with_config` are async. Discovery order is unchanged
- A provider with no preset model falls back under its own slug (`myproxy/deepseek-v4-flash`) instead of hardcoding the `deepseek` vendor, and an empty selection is no longer written as `active = ""`
- **Breaking:** `[provider]` is gone: the active vendor is the root `active = "<slug>"`, and a provider is named by its `[providers.<slug>]` table key instead of a repeated `name`. Model metadata moves back to keyed tables — `[providers.<slug>.models."<wire-id>"]` — keeping the id out of the body, so ids with dots (`gpt-4.1`) just need quoting. Files still in the old shape keep loading and are rewritten on the next save, and a declared model now merges field by field across the user and project files instead of the newer entry replacing the older one outright
- `ProviderConfig::suggested_base_url` reads `oven_llm::ProviderName::base_url` instead of keeping its own copy of the same five endpoints

### Fixed
- `oven model add` leaves a shipped model undeclared when there is nothing to override, and seeds an override from the whole catalog entry: a declared entry *replaces* the catalog's, so declaring one with empty metadata used to zero its context window and output limit

## [0.0.9] - 2026-09-30

### Added
- Subagents: the `task` tool delegates a self-contained job to another agent (`explore` — read-only tools — or `general`) with its own context, and `task_output` reports on one, or on all of them
- Subagent lifecycles: a registry with a concurrency cap (further spawns wait as `queued`), `role#n` addressing, and cancellation by name, by the turn that spawned them, or by app shutdown
- `/agents` lists subagents and stops, drops or focuses one (`x` stops the focused one); the TUI shows them in a strip above the composer (active first, `+N` when capped) and gives each its own transcript, opened with `/agents <n>` or by clicking its row
- `answer` tool: the model asks a question mid-turn and blocks until the answer arrives, which then becomes the tool result, with the question, option labels and descriptions clamped to their display limits. The question renders as an overlay prompt listing the proposed answers plus an `Other…` row that hands the composer the keystrokes, so any answer can be typed; `Esc` skips it and `Ctrl-C` cancels the turn
- A pending request is mirrored as `ToolEvent::QuestionAsked` with `AppPhase::Awaiting` and resumed by `App::respond`, and `AppEventKind::RequestResolved` closes its overlay even when the turn ends still holding it
- `max_iters` is configurable and bounds one run rather than one agent; `[subagents]` configures `enabled`, `max_concurrent` and the subagents' own `max_iters`
- `TurnEvent::StepStarted` / `StepFinished` bound each provider round trip, and `Agent::step` is public and returns a `Step` (text, calls, usage), so loop progress is observable and the loop is one policy over a step rather than the only way to drive an agent
- Tools receive the run's `TurnContext`, which carries cancellation, mode, model and the channel an interactive tool asks its question on
- Per-model metadata declared as `[[providers.<slug>.models]]` accepts `max_output_tokens` and the `supports_system_prompt` / `supports_tools` / `supports_streaming` / `supports_vision` flags next to `context_window`: omitted limits stay unknown (validation skips them) and omitted capabilities count as supported, so declaring a model never silently disables it
- A tool call in flight shimmers its transcript row the way thinking does, until its result lands: the burst row while a grouped call has yet to answer, and each call rendered as its own row

### Changed
- **Breaking:** `AppCommand` and `ControlCommand` are gone. `App::submit(text)` classifies the text once into an `Input` (`Chat`, `Shell`, `Slash`, `Rewind`) and returns it, so the runtime and the TUI no longer sniff the same string for the same syntax
- **Breaking:** what needs no conversation driver is a method on `App`, applied by the caller instead of queued behind the turn it affects: `cancel(turn_id)`, `set_mode`, `respond`, `stop_subagent`, `stop_subagents`. `/model`, `/agents` and `/exit` still apply mid-turn, and `shutdown` cancels the running turn directly
- **Breaking:** `TurnContext::new` takes the agent's `Selection` (its mode and model) instead of copies, `set_mode` / `set_model` are gone, and a turn asks the user through a `RequestSink` (`TurnContext::with_requests`) that `Shared` stores, announces and answers by id, so nothing is copied back or relayed when a run ends
- **Breaking:** `StateChange` and `StateEvent` are gone: mode, model, context window, providers, models, subagents, todos and usage are read from `AppState`, which the TUI follows through the `watch` channel. The one event left is `AppEventKind::HistoryChanged { reason }`
- **Breaking:** model metadata moves from the quoted `[providers.<slug>.models."<wire-id>"]` table keys to `[[providers.<slug>.models]]` entries with an `id` field, so ids containing dots (`gpt-4.1`, `glm-5.3`) need no quoting; entries merge by `id` and are rewritten sorted when the config is saved
- `AppState` lives only in its `watch::Sender` and is changed in place, queued prompts are counted in the inbox so `App::shutdown` reports every prompt the driver never took, and tools mount as `Arc<dyn Tool>` shared by subagents
- `AppPhase::Compacting` marks the driver busy summarizing and the TUI is busy exactly while `phase.is_active()`, and `AppPhase::Awaiting` carries a pending request
- `/setup` rebuilds the router from config and swaps the snapshot in (`Agent::replace_router`) instead of mutating a live router in place, which used to be able to panic while another agent was mid-request
- Only the driver's own events move app state, and every agent reports on one event channel the runtime drains while idle, so a subagent's progress and usage no longer land in the main turn's readout and reach the frontend between turns
- The turn driver moves to `runtime/turn.rs`: the runtime actor keeps the command loop and persistence, and what happens while a turn runs sits beside it
- `oven-app`, `oven-agent` and `oven-tui` are layered (`core` / `runtime` / `capabilities` / `api` / `platform` / `widgets`) with each `lib.rs` a map of those layers, and MCP servers are declared in config instead of the MCP layer
- `/model` and `/setup` completion lists filter by the characters typed: the query is rendered in the composer (`/model deep`), `Tab` completes the highlighted entry into it, and the provider and protocol stages of the wizard narrow the same way; `oven_app::complete` owns the prefix rule shared by the slash popup, the model picker and the wizard
- Transcript chrome: user prompts and `!` shell commands are framed like the composer, with the submitted prompt as the first row of its turn, and the hint row moved to the composer's bottom-right
- `docs/subagents.md`, `docs/architectures.md`, `docs/oven-app.md` and `docs/oven-tui.md` cover the subagent design, the question channel, the new phase and the crate layers
- `Tool::result_detail` lets a tool name the body its row ends on when it lands, so `ToolEvent::Finished` carries it beside the result; a question row shows the answer the user picked instead of repeating the choices the prompt already offered
- A turn's questions are counted in the burst title the way its searches, reads and commands are: `Asked 1 question`, `Asked 2 questions`
- An answer opens with its question: what a call landed on is pinned, so a burst reopened after the turn moved on lists each question with the answer it got instead of hiding the answers behind a second click

### Fixed
- The composer no longer garbles itself: a deleted wide glyph leaves no stale half-cell, the prompt stays `>`, the hardware cursor is parked on the caret for IME, and the prompt stays pinned until the viewport leaves its turn
- Tool calls from one response run at the same time instead of one after another, so asking for several `task`s actually runs several subagents in parallel; approvals are still asked one at a time, tools that declare `ToolCaps::exclusive` (file edits and writes, the todo list, the question tool) take a turn each, and tool results still enter the history in the order the model asked for them
- `/agents` applies while a turn is running and opening a subagent builds its view on demand, so it can be watched from the moment it is spawned instead of only after the reply lands; its clock stops when it does, so a cancelled or failed subagent shows the time it took instead of counting up forever
- `Ctrl-C` stops what is still running before quitting — the turn, then the subagents it spawned — and reports the messages that were queued but never sent on the way out
- Leaving a subagent's transcript takes a single `Esc` instead of the usual confirmation pair, so `Esc` first closes an open viewer before it cancels a turn or rewinds; the hint row spells the way back out, the arrow keys scroll it, and `Ctrl-C` still quits from inside the viewer
- Restore the per-turn `Worked for Xs` transcript separator after answers, tool follow-ups, cancellations, failures, compaction, and app errors, including the turn duration when resuming a session from its persisted timestamps
- A verbose question no longer fails the `answer` call, which used to leave the user with a failed tool row and no prompt at all, and the question prompt grows with the question's wrapped text instead of clipping it to a fixed block
- Composer slash and mention completions stay shut while a question waits for a typed answer, so `Tab` can no longer complete command text into the answer
- Declaring a model no longer registers it with every capability off, which made oven reject each turn locally with `provider: invalid request` before the request ever reached the upstream API
- Provider request-validation failures name the rule that rejected them (`max_tokens 4096 exceeds model's max_output_tokens 8192`, `model does not support tools`, …) instead of only the bare `invalid request`
- Expanding or collapsing a transcript block no longer yanks the view to that block's line: the anchor is the first body line the layout would draw, which accounts for the pinned prompt, so the clicked header keeps its screen row and the body grows below it
- Tool rows share the thinking color, matching the in-flight shimmer
- The install scripts skip the download when no tag is pinned and the requested version is already installed

## [0.0.8] - 2026-09-22

### Added
- Live token usage: the status bar and context readout update as each provider response arrives, instead of once per turn
- File edit/write results are collapsible in the transcript, toggled like thinking blocks
- Tool bursts are titled by count (e.g. `Ran 2 commands, Read 1 file, 1 failed`) with the individual call lines in a collapsible body
- Newest-only live bodies: thinking deltas and open tool bursts render only their most recent screen rows, marked with an "N earlier lines" gap, so streaming cannot scroll the view upward
- `@` file mention now lists directories too, and `Tab` completes a directory without submitting

### Changed
- Assistant prose renders a single bullet gutter on its first line, with continuation lines indented under the body; other line kinds share the message indent
- Live bodies are capped by wrapped screen rows rather than logical lines
- Session files are appended from the already-persisted message count instead of rewriting every record after a turn
- History messages are shared as `Arc`, so publishing state and re-seeding the transcript cost refcounts instead of copying the conversation
- The agent owns the thinking clock and emits `StreamEvent::ThinkingDone { duration_ms }`, so the reported span, the transcript row, and the persisted session always agree; the non-streaming fallback now times its reasoning too
- Drop the per-turn `Worked for Xs` transcript separator and other dead code
- Bump `oven-llm` to 0.4.2 (new `raw_arguments` field on tool-use blocks)
- Install script prefers the `linux-gnu` artifacts (glibc ≥ 2.28) and falls back to musl, and fails fast when neither curl nor wget is available
- Add `docs/oven-tui.md` and `docs/oven-app.md`; refresh `docs/architectures.md`

### Fixed
- Close the thinking window before the answer or tool it led to
- Pin the clicked header row while a collapsed detail expands
- Keep transcript text selection alive across streaming events
- Align non-user rows with the user gutter
- Mid-turn `/model` switches publish the switching model's context window

## [0.0.7] - 2026-09-16

### Added
- Ask mode (`Shift+Tab` cycles Agent → Plan → Ask): read-only tools run freely, shell commands need interactive approval, write and MCP tools are hidden
- Rotating file logs at `~/.oven/logs/oven.log` (10 MiB × 2 files) and tracing across the agent and runtime
- Auto-collapse thinking and tool details when the next message starts (new details start expanded; session-seeded ones stay collapsed; manual expands stay pinned)
- Collapsible diff rows for `file_edit` / `file_write` with per-line added/removed styling
- Choice popup when the agent loop reaches its iteration limit, letting the user continue or end the turn
- `--version` prints the commit hash and release date

### Fixed
- Unknown slash commands pass through to the model instead of erroring
- Normalize `\r\n` and lone `\r` to `\n` when pasting into the composer
- Treat wide characters as atomic in transcript selection
- Pin the transcript to the bottom on user input and `!` shell commands
- Show a reason when a tool is rejected or unavailable
- Dismiss a finished todo list when the next message is sent
- Include the last 8 characters of the session id in log entries

## [0.0.6] - 2026-09-06

### Added
- `/compact` slash command and auto-compaction when history nears the model's context window
- New `oven-host` crate for host system interaction: process execution, command-output decoding, path confinement, and directory walking
- Collapsible tool results, toggled like thinking blocks
- Thinking block hover with double-click collapse
- Tool bursts grouped by action with detail summaries
- Turn elapsed time shown after each turn (`Worked for Xs`); thought elapsed time
- Mouse scrolling across multiple lines in the textarea

### Changed
- Split `AppCommand` into `Prompt` and `Control`; slash state changes and `Rewind` queue behind a running turn
- Move the system prompt module into `oven-agent`
- Parse `TodoWrite` args with serde
- Unify transcript viewport anchoring and trim redundant TUI state

### Fixed
- Garbled shell output on Windows
- Refresh the file list when using `@` to mention files

## [0.0.5] - 2026-09-01

### Added
- `!` local shell in the composer (host bash/PowerShell in the workspace, no LLM turn)
- Diff rendering for `file_edit` / `file_write` in the transcript
- Persist vendors under `[providers.<slug>]` so `/setup` and `/model` reuse saved API keys

### Changed
- Merge `AppHandle` into `App`; construct via `AppBuilder`
- Status bar reports last-turn token usage; drop history budget trimming
- Extract `walk_dir` into `oven-agent`; `oven-app` no longer depends on `ignore`
- Reorganize prompt templates under `prompt_template/`
- Fold `turn.rs` into `runtime/mod.rs`
- Slash replies shown as a toast

### Fixed
- Keep the selected model visible when scrolling the picker
- Cursor blinking in the input
- `file_read` results include line numbers
- Flaky tests on Windows

## [0.0.4] - 2026-08-24

### Added
- @ syntax to mention files in chat

### Changed
- Move AppEvent to a single mod
- Simplify env detection in system prompt
- Enhance event protocol
- Move resolve_session into Session::resolve

### Fixed
- Fix UI custom models displaying text
- Put effort value right after model name with a space
- Add rounded input border and quieter motion to TUI

## [0.0.3] - 2026-08-20

### Changed
- Adopt oven-llm Router (0.4.1) for model/provider routing
- Replace provider `kind` with `protocol` (custom vendors only); rewrite aliases `grok`/`kimi`/`glm` to `xai`/`moonshot`/`zhipu`
- Stop sending a default `max_tokens` of 4096
- File tools use async I/O
- Restructure TUI widgets under `components/`

### Fixed
- Accumulate token usage of all provider responses in a turn

## [0.0.2] - 2026-08-18

### Added
- Tool system architecture and UI overhaul

### Changed
- Replaced XDG directories with a single ~/.oven home directory
- Split transcript.rs into modular components

### Fixed
- UI full paint when scrolling
- Enlarged truncate length for tool results


## [0.0.1] - 2026-08-16
- Initial Release