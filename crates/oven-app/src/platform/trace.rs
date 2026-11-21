//! The trace of a run: what the agent did, one event per line.
//!
//! The event stream is where the agent says what it did. This module turns
//! that stream into log lines, so the log is a view of the same facts
//! rather than a second place to remember them: a fact that is not on the
//! stream never reaches the log.
//!
//! What happened is logged at `info`; what was said is logged at `trace`,
//! so the default filter keeps the shape of a run and
//! `RUST_LOG=oven.agent=trace` keeps every word of it. Everything is on the
//! `oven.agent` target.
//!
//! The same stream is also written down: the run as it happened, one event
//! per line, in a file of its own. The log is the readable view; the file
//! is the record. Every run gets its own file, so a session that is
//! resumed writes a second file rather than appending to the first, and
//! a file stands for exactly one run.
//!
//! The file keeps the shape of the run: what happened, when, and how it
//! ended. The text as it streamed is left out, because it is the bulk of a
//! trace and the session file holds it already. A trace that keeps every
//! word is asked for; it is worth the space only when a run has to be read
//! word by word. A run asked to be quiet keeps no trace at all, so it has
//! no file and no writer.
//!
//! A workspace keeps the newest traces and drops the older ones, so the
//! space the traces take is bounded by the number of runs and not by the
//! number of turns.
//!
//! The events are buffered and reach the disk a turn at a time: a run
//! writes thousands of lines, and one write per line costs more than the
//! trace is worth. The file is current at the end of every turn, and the
//! tail of a run can be missing if the app is killed before the last
//! flush.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use oven_proto::{
    AGENT_TARGET, AgentError, AgentEvent, AgentEventEnvelope, AppEvent, AppEventKind, StreamEvent,
    ToolEvent, ToolResult, ToolView, TurnEvent,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;

use crate::platform::id;

/// How many characters a run id has: short enough to type, long enough to
/// be a name of its own.
const RUN_ID_LEN: usize = 12;
/// Which shape of the trace file this build writes, and the newest one it
/// reads.
pub const TRACE_FORMAT_VERSION: u32 = 1;
/// How many traces a workspace keeps: the newest ones, the older ones go.
pub const DEFAULT_KEEP: usize = 2;
/// How much of a body the trace of a run keeps.
const MAX_BODY_BYTES: usize = 4 * 1024;
/// The mark that says a body was cut.
const CUT_MARK: &str = "\n...[truncated]";
/// How much of the trace is held in memory before it reaches the disk.
const WRITE_BUFFER_BYTES: usize = 64 * 1024;
/// What the trace directory tells git, so a trace never lands in a commit
/// with the workspace it belongs to.
///
/// The rule ignores the directory itself, so git does not report the
/// workspace as changed because a run wrote a trace into it.
const GITIGNORE: &str = "*\n";

#[derive(Debug, Error)]
pub enum TraceError {
    #[error("trace: {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("trace: {path}:{line}: {source}")]
    Parse {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "trace: {path}: the file was written by a newer oven: format {version}, this build reads {}",
        TRACE_FORMAT_VERSION
    )]
    NewerFormat { path: PathBuf, version: u32 },
    #[error("trace: '{id}' is not a plain name")]
    BadId { id: String },
    #[error("trace: no run named '{id}'")]
    NoSuchTrace { id: String },
    #[error("trace: '{id}' names more than one run: {candidates}")]
    AmbiguousTrace { id: String, candidates: String },
}

fn io(path: &Path, source: std::io::Error) -> TraceError {
    TraceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What the trace of a run keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    /// No trace at all: the run leaves no file behind.
    Quiet,
    /// The shape of the run: what happened, when, and how it ended.
    ///
    /// The text as it streamed is left out, because it is the bulk of a
    /// trace and the session file holds it already.
    Skeleton,
    /// Everything the run said, as it said it.
    Full,
}

/// How a workspace keeps its traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceSettings {
    /// What the trace of a run keeps.
    pub mode: TraceMode,
    /// How many traces a workspace keeps. Zero keeps them all.
    pub keep: usize,
}

impl Default for TraceSettings {
    fn default() -> Self {
        Self {
            mode: TraceMode::Skeleton,
            keep: DEFAULT_KEEP,
        }
    }
}

/// Logs one event under the agent target, with the ids every line carries.
macro_rules! event {
    ($level:ident, $envelope:expr, $($field:tt)*) => {
        tracing::$level!(
            target: AGENT_TARGET,
            agent_id = $envelope.agent_id.0,
            turn_id = $envelope.turn_id.0,
            $($field)*
        )
    };
}

/// Where the traces of a workspace live.
pub fn trace_dir(root: &Path) -> PathBuf {
    root.join(".oven").join("trace")
}

/// Where the trace of one run lives.
pub fn trace_path(root: &Path, id: &str) -> PathBuf {
    trace_dir(root).join(format!("{id}.jsonl"))
}

/// A run of the agent, and the trace it writes.
///
/// It is the first line of the trace file, so a file read on its own says
/// which run it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceRun {
    /// One run of the agent: the trace of one invocation.
    Run {
        /// Which shape of the trace file this is. A header written before
        /// the format named its version reads as 0 and is read as it
        /// stands; one from a newer oven is refused with its version
        /// rather than misread.
        #[serde(default)]
        version: u32,
        /// The id of the run: the name of its file.
        id: String,
        /// The session the run belonged to, when it belonged to one.
        session_id: Option<String>,
        /// When the run started, in Unix milliseconds.
        started_at: u64,
    },
}

impl TraceRun {
    /// Which shape of the trace file this is.
    pub fn version(&self) -> u32 {
        match self {
            TraceRun::Run { version, .. } => *version,
        }
    }

    /// The id of the run.
    pub fn id(&self) -> &str {
        match self {
            TraceRun::Run { id, .. } => id,
        }
    }

    /// The session the run belonged to, when it belonged to one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            TraceRun::Run { session_id, .. } => session_id.as_deref(),
        }
    }

    /// When the run started, in Unix milliseconds.
    pub fn started_at(&self) -> u64 {
        match self {
            TraceRun::Run { started_at, .. } => *started_at,
        }
    }
}

/// Starts the trace of a run: what the run is called.
pub fn start(session_id: Option<&str>) -> TraceRun {
    TraceRun::Run {
        version: TRACE_FORMAT_VERSION,
        id: new_run_id(),
        session_id: session_id.map(str::to_owned),
        started_at: oven_host::now_ms(),
    }
}

/// A name for a run: random, so two runs never share one.
fn new_run_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..RUN_ID_LEN].to_owned()
}

/// One trace of a workspace.
#[derive(Debug, Clone)]
pub struct TraceSummary {
    /// The run that wrote the trace.
    pub id: String,
    /// The session the run belonged to, when the file names it.
    pub session_id: Option<String>,
    /// When the run started, in Unix milliseconds, when the file names it.
    pub started_at: Option<u64>,
    /// How much the trace holds.
    pub bytes: u64,
    /// When it was last written.
    pub modified: Option<SystemTime>,
}

/// The traces of a workspace, newest first.
pub async fn list_traces(root: &Path) -> Result<Vec<TraceSummary>, TraceError> {
    let dir = trace_dir(root);
    let mut entries = match fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io(&dir, error)),
    };
    let mut traces = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|e| io(&dir, e))? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let metadata = entry.metadata().await.map_err(|e| io(&path, e))?;
        let run = read_run(&path).await;
        traces.push(TraceSummary {
            id: id.to_owned(),
            session_id: run
                .as_ref()
                .and_then(|run| run.session_id().map(str::to_owned)),
            started_at: run.as_ref().map(|run| run.started_at()),
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
        });
    }
    traces.sort_by_key(|trace| std::cmp::Reverse(trace.modified));
    Ok(traces)
}

/// The runs of one session, oldest first.
///
/// A session that is resumed writes a run of its own, so the runs of a
/// session are the traces that name it. The ones that were pruned are
/// gone: what is left is what the session still has.
pub async fn traces_of_session(
    root: &Path,
    session: &str,
) -> Result<Vec<TraceSummary>, TraceError> {
    let mut traces: Vec<TraceSummary> = list_traces(root)
        .await?
        .into_iter()
        .filter(|trace| {
            trace
                .session_id
                .as_deref()
                .is_some_and(|id| session_matches(id, session))
        })
        .collect();
    traces.sort_by_key(|trace| trace.started_at.unwrap_or_default());
    Ok(traces)
}

/// The traces of the runs of one session, oldest first.
pub async fn read_session(
    root: &Path,
    session: &str,
    tail: Option<usize>,
) -> Result<Vec<Trace>, TraceError> {
    let mut traces = Vec::new();
    for summary in traces_of_session(root, session).await? {
        traces.push(read_trace(root, &summary.id, tail).await?);
    }
    Ok(traces)
}

/// Whether a query names a session id.
///
/// The query may be the whole id, a prefix of it, or the short form a
/// listing shows: the last few characters of the id. A suffix shorter than
/// that is no name — it would match more than one id.
pub fn session_matches(session: &str, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    if session == query || session.starts_with(query) {
        return true;
    }
    query.chars().count() >= id::SHORT_ID_LEN && session.ends_with(query)
}

/// The trace of one run, read back as the events it holds.
#[derive(Debug, Clone)]
pub struct Trace {
    /// The run that wrote the trace, when the file names it.
    pub run: Option<TraceRun>,
    /// The events, oldest first.
    pub events: Vec<AgentEventEnvelope>,
    /// How many lines could not be read. A run that was killed mid-write
    /// leaves one behind; the rest of the trace is still worth reading.
    pub damaged: usize,
    /// How far into the run the first event happened, in the milliseconds
    /// the run counts from. The times of a timeline are counted from it,
    /// so a tail of a trace still says where it sits in the run.
    pub base_elapsed_ms: u64,
}

/// Reads the trace of a run.
///
/// The id names the run, or is a prefix of it that names one run and no
/// other. `tail` keeps the last events only, so a long trace does not have
/// to be held in memory whole; the moment the run started at is kept all
/// the same, so a tail still says where it sits in the run.
pub async fn read_trace(root: &Path, id: &str, tail: Option<usize>) -> Result<Trace, TraceError> {
    let path = find_trace(root, id).await?;
    read_file(&path, tail).await
}

/// The file of the run named by `id`, or by a unique prefix of it.
async fn find_trace(root: &Path, id: &str) -> Result<PathBuf, TraceError> {
    if !id::is_safe_id(id) {
        return Err(TraceError::BadId { id: id.to_owned() });
    }
    let exact = trace_path(root, id);
    if fs::try_exists(&exact).await.map_err(|e| io(&exact, e))? {
        return Ok(exact);
    }
    let dir = trace_dir(root);
    let mut entries = match fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(TraceError::NoSuchTrace { id: id.to_owned() });
        }
        Err(error) => return Err(io(&dir, error)),
    };
    let mut found: Vec<PathBuf> = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|e| io(&dir, e))? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.starts_with(id) {
            found.push(path);
        }
    }
    match found.len() {
        0 => Err(TraceError::NoSuchTrace { id: id.to_owned() }),
        1 => Ok(found.remove(0)),
        _ => {
            let candidates = found
                .iter()
                .filter_map(|path| path.file_stem())
                .filter_map(|stem| stem.to_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(TraceError::AmbiguousTrace {
                id: id.to_owned(),
                candidates,
            })
        }
    }
}

/// Reads a trace file, line by line.
async fn read_file(path: &Path, tail: Option<usize>) -> Result<Trace, TraceError> {
    let file = fs::File::open(path).await.map_err(|e| io(path, e))?;
    let mut lines = BufReader::new(file).lines();
    let mut trace = Trace {
        run: None,
        events: Vec::new(),
        damaged: 0,
        base_elapsed_ms: 0,
    };
    let mut events: VecDeque<AgentEventEnvelope> = VecDeque::new();
    let mut first_damage: Option<(usize, serde_json::Error)> = None;
    let mut base: Option<u64> = None;
    let mut number = 0usize;
    while let Some(line) = lines.next_line().await.map_err(|e| io(path, e))? {
        number += 1;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AgentEventEnvelope>(&line) {
            Ok(envelope) => {
                base.get_or_insert(envelope.elapsed_ms);
                events.push_back(envelope);
            }
            Err(error) => match serde_json::from_str::<TraceRun>(&line) {
                Ok(run) if run.version() > TRACE_FORMAT_VERSION => {
                    return Err(TraceError::NewerFormat {
                        path: path.to_path_buf(),
                        version: run.version(),
                    });
                }
                Ok(run) => trace.run = Some(run),
                Err(_) => {
                    trace.damaged += 1;
                    first_damage.get_or_insert((number, error));
                }
            },
        }
        if let Some(limit) = tail {
            while events.len() > limit {
                events.pop_front();
            }
        }
    }
    if trace.damaged > 0 && trace.run.is_none() && events.is_empty() {
        let (line, source) = first_damage.expect("a damaged line was counted");
        return Err(TraceError::Parse {
            path: path.to_path_buf(),
            line,
            source,
        });
    }
    trace.events = events.into_iter().collect();
    trace.base_elapsed_ms = base.unwrap_or_default();
    Ok(trace)
}

/// The run a trace file names, when its first line names one.
async fn read_run(path: &Path) -> Option<TraceRun> {
    let file = fs::File::open(path).await.ok()?;
    let mut lines = BufReader::new(file).lines();
    let line = lines.next_line().await.ok()??;
    serde_json::from_str(&line).ok()
}

/// Removes the trace of a run.
pub async fn remove_trace(root: &Path, id: &str) -> Result<(), TraceError> {
    let path = find_trace(root, id).await?;
    fs::remove_file(&path).await.map_err(|e| io(&path, e))
}

/// Removes the traces that are not worth keeping, and says how many went.
///
/// `keep` keeps the newest traces and drops the rest; zero keeps none of
/// them. `older_than` drops the traces that were last written longer ago
/// than that.
pub async fn prune_traces(
    root: &Path,
    keep: Option<usize>,
    older_than: Option<Duration>,
) -> Result<usize, TraceError> {
    let traces = list_traces(root).await?;
    let mut removed = 0;
    for (index, trace) in traces.iter().enumerate() {
        let too_many = keep.is_some_and(|keep| index >= keep);
        let too_old = older_than.is_some_and(|age| {
            trace.modified.is_some_and(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default()
                    > age
            })
        });
        if !too_many && !too_old {
            continue;
        }
        removed += remove_file(root, &trace.id).await;
    }
    Ok(removed)
}

/// Keeps the newest traces of a workspace and removes the rest.
///
/// The trace of the run being closed is one of the newest whatever its
/// age: it is not over yet when the pruning starts, and it counts toward
/// what a workspace keeps — the clocks of two files agreeing on a moment
/// must not decide it is not.
async fn prune_to_newest(root: &Path, keep: usize, current: &str) -> Result<usize, TraceError> {
    let traces = list_traces(root).await?;
    let others_kept = keep.saturating_sub(1);
    let mut kept = 0;
    let mut removed = 0;
    for trace in &traces {
        if trace.id == current {
            continue;
        }
        if kept < others_kept {
            kept += 1;
            continue;
        }
        removed += remove_file(root, &trace.id).await;
    }
    Ok(removed)
}

/// Removes the file of a run, and says whether it went.
async fn remove_file(root: &Path, id: &str) -> usize {
    if fs::remove_file(trace_path(root, id)).await.is_ok() {
        1
    } else {
        0
    }
}

/// What the trace file keeps of the run.
///
/// A quiet run keeps no trace at all, so it has no writer: this is what a
/// writer that exists keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kept {
    /// The shape of the run: what happened, when, and how it ended.
    Skeleton,
    /// Everything the run said, as it said it.
    Full,
}

/// Records the agent's events into a trace file, one per line.
///
/// The file is the run as it happened: the events, in the order they
/// happened, with the moment each one landed. It sits beside the
/// workspace rather than in the session file, because the session file is
/// the conversation and has to stay resumable.
pub struct TraceWriter {
    root: PathBuf,
    run: TraceRun,
    events: mpsc::UnboundedReceiver<Arc<AppEvent>>,
    /// What the file keeps of the run.
    kept: Kept,
    /// How many traces a workspace keeps. Zero keeps them all.
    keep: usize,
    file: Option<BufWriter<fs::File>>,
    /// The events that are written but not yet on the disk.
    pending: usize,
    lost: usize,
}

impl TraceWriter {
    /// The writer of a trace, when the run keeps one at all.
    ///
    /// A quiet run keeps no trace, so it has no writer: this is `None`.
    pub fn new(
        root: PathBuf,
        run: TraceRun,
        events: mpsc::UnboundedReceiver<Arc<AppEvent>>,
        settings: TraceSettings,
    ) -> Option<Self> {
        let kept = match settings.mode {
            TraceMode::Quiet => return None,
            TraceMode::Skeleton => Kept::Skeleton,
            TraceMode::Full => Kept::Full,
        };
        Some(Self {
            root,
            run,
            events,
            kept,
            keep: settings.keep,
            file: None,
            pending: 0,
            lost: 0,
        })
    }

    /// The file the run's trace lives in.
    fn path(&self) -> PathBuf {
        trace_path(&self.root, self.run.id())
    }

    /// Writes the events as they arrive, until the bus stops publishing.
    ///
    /// A write that fails does not end the trace: the event is lost, the
    /// rest of the run is still worth having, and the loss is reported when
    /// the run ends.
    pub async fn run(mut self) {
        if let Err(error) = self.open().await {
            tracing::warn!(
                path = %self.path().display(),
                error = %error,
                "the trace could not be opened"
            );
            return;
        }
        while let Some(event) = self.events.recv().await {
            let AppEventKind::Agent(envelope) = &event.kind else {
                continue;
            };
            if let Err(error) = self.append(envelope).await {
                self.lost += 1;
                tracing::warn!(
                    path = %self.path().display(),
                    error = %error,
                    lost = self.lost,
                    "the trace could not be written"
                );
                continue;
            }
            if ends_turn(envelope) {
                self.flush().await;
            }
        }
        self.flush().await;
        if self.lost > 0 {
            tracing::error!(
                path = %self.path().display(),
                lost = self.lost,
                "the trace is incomplete: some events were lost"
            );
        }
        self.prune().await;
    }

    /// Keeps the newest traces of the workspace and drops the older ones,
    /// so the space they take is bounded.
    async fn prune(&self) {
        if self.keep == 0 {
            return;
        }
        if let Err(error) = prune_to_newest(&self.root, self.keep, self.run.id()).await {
            tracing::warn!(
                error = %error,
                "the older traces could not be removed"
            );
        }
    }

    /// Opens the trace and writes its header.
    ///
    /// The file exists from the start of the run, so a run that raised no
    /// event at all is still a run that happened.
    async fn open(&mut self) -> std::io::Result<()> {
        let path = self.path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).await?;
            fs::write(dir.join(".gitignore"), GITIGNORE).await?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        let mut line = serde_json::to_string(&self.run).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = BufWriter::with_capacity(WRITE_BUFFER_BYTES, file);
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        self.file = Some(file);
        Ok(())
    }

    /// Appends one event.
    async fn append(&mut self, envelope: &AgentEventEnvelope) -> std::io::Result<()> {
        let Some(line) = self.line(envelope)? else {
            return Ok(());
        };
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("the trace file is not open"))?;
        file.write_all(line.as_bytes()).await?;
        self.pending += 1;
        Ok(())
    }

    /// The line the event is written as: the event itself when the trace
    /// keeps everything, and the shape of it when the trace keeps the
    /// shape. An event the shape leaves out is not written at all.
    fn line(&self, envelope: &AgentEventEnvelope) -> Result<Option<String>, std::io::Error> {
        let mut line = match self.kept {
            Kept::Full => serde_json::to_string(envelope),
            Kept::Skeleton => match skeleton(envelope) {
                Some(event) => serde_json::to_string(&event),
                None => return Ok(None),
            },
        }
        .map_err(std::io::Error::other)?;
        line.push('\n');
        Ok(Some(line))
    }

    /// Puts what is buffered on the disk.
    ///
    /// The events that were waiting for it are lost when the flush fails,
    /// and they are counted: the trace says how much of it is missing.
    async fn flush(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        match file.flush().await {
            Ok(()) => self.pending = 0,
            Err(error) => {
                self.lost += self.pending;
                self.pending = 0;
                tracing::warn!(
                    path = %self.path().display(),
                    error = %error,
                    lost = self.lost,
                    "the trace could not be flushed"
                );
            }
        }
    }
}

/// Whether the event ends a turn.
///
/// The trace is flushed there, so the file is current at the end of every
/// turn and not only at the end of the run.
fn ends_turn(envelope: &AgentEventEnvelope) -> bool {
    matches!(
        envelope.event,
        AgentEvent::Turn(
            TurnEvent::Completed { .. }
                | TurnEvent::Failed { .. }
                | TurnEvent::Cancelled { .. }
                | TurnEvent::LoopLimitReached { .. }
        )
    )
}

/// The event the trace keeps when it keeps the shape of the run.
///
/// The text as it streamed is left out: it is the bulk of a trace, and the
/// session file holds the text already. Every other event is kept, with
/// its bodies cut, so a tool that printed a megabyte does not fill the
/// file.
fn skeleton(envelope: &AgentEventEnvelope) -> Option<AgentEventEnvelope> {
    let event = match &envelope.event {
        AgentEvent::Turn(turn) => AgentEvent::Turn(cut_turn(turn)),
        AgentEvent::Stream(stream) => AgentEvent::Stream(cut_stream(stream)?),
        AgentEvent::Tool(tool) => AgentEvent::Tool(cut_tool(tool)?),
        AgentEvent::TodosChanged { todos } => AgentEvent::TodosChanged {
            todos: todos.clone(),
        },
        AgentEvent::Usage { usage } => AgentEvent::Usage { usage: *usage },
    };
    Some(AgentEventEnvelope {
        agent_id: envelope.agent_id,
        turn_id: envelope.turn_id,
        at: envelope.at,
        elapsed_ms: envelope.elapsed_ms,
        event,
    })
}

/// The turn, with its bodies cut.
fn cut_turn(turn: &TurnEvent) -> TurnEvent {
    match turn {
        TurnEvent::Prompt { text } => TurnEvent::Prompt {
            text: cut(text).into_owned(),
        },
        TurnEvent::UserAppended { text } => TurnEvent::UserAppended {
            text: cut(text).into_owned(),
        },
        TurnEvent::NoticeAppended { text } => TurnEvent::NoticeAppended {
            text: cut(text).into_owned(),
        },
        TurnEvent::StepRejected {
            index,
            error,
            stop_reason,
            blocks,
            usage,
            provider_ms,
        } => TurnEvent::StepRejected {
            index: *index,
            error: cut_error(error),
            stop_reason: *stop_reason,
            blocks: *blocks,
            usage: *usage,
            provider_ms: *provider_ms,
        },
        TurnEvent::Failed { error, duration_ms } => TurnEvent::Failed {
            error: cut_error(error),
            duration_ms: *duration_ms,
        },
        other => other.clone(),
    }
}

/// The stream event, when the shape of the run keeps it.
fn cut_stream(stream: &StreamEvent) -> Option<StreamEvent> {
    match stream {
        StreamEvent::TextDelta { .. } | StreamEvent::ThinkingDelta { .. } => None,
        StreamEvent::ThinkingDone { duration_ms } => Some(StreamEvent::ThinkingDone {
            duration_ms: *duration_ms,
        }),
    }
}

/// The tool event, when the shape of the run keeps it.
fn cut_tool(tool: &ToolEvent) -> Option<ToolEvent> {
    match tool {
        ToolEvent::OutputDelta { .. } => None,
        ToolEvent::ApprovalRequested {
            request_id,
            call_id,
            name,
            view,
        } => Some(ToolEvent::ApprovalRequested {
            request_id: *request_id,
            call_id: *call_id,
            name: name.clone(),
            view: cut_view(view),
        }),
        ToolEvent::Started {
            call_id,
            name,
            view,
        } => Some(ToolEvent::Started {
            call_id: *call_id,
            name: name.clone(),
            view: cut_view(view),
        }),
        ToolEvent::QuestionAsked {
            request_id,
            question,
        } => Some(ToolEvent::QuestionAsked {
            request_id: *request_id,
            question: question.clone(),
        }),
        ToolEvent::Finished {
            call_id,
            name,
            result,
            duration_ms,
            detail,
        } => Some(ToolEvent::Finished {
            call_id: *call_id,
            name: name.clone(),
            result: cut_result(result),
            duration_ms: *duration_ms,
            detail: detail.as_deref().map(|detail| cut(detail).into_owned()),
        }),
    }
}

/// The view of a tool, with its body cut.
fn cut_view(view: &ToolView) -> ToolView {
    ToolView {
        summary: cut(&view.summary).into_owned(),
        collapse: view.collapse,
        detail: view
            .detail
            .as_deref()
            .map(|detail| cut(detail).into_owned()),
    }
}

/// The result of a tool, with its body cut.
fn cut_result(result: &ToolResult) -> ToolResult {
    match result {
        ToolResult::Success { output } => ToolResult::Success {
            output: cut(output).into_owned(),
        },
        ToolResult::Failed { error, output } => ToolResult::Failed {
            error: cut(error).into_owned(),
            output: output.as_deref().map(|output| cut(output).into_owned()),
        },
        ToolResult::Rejected { reason } => ToolResult::Rejected {
            reason: cut(reason).into_owned(),
        },
        ToolResult::Cancelled => ToolResult::Cancelled,
    }
}

/// The error of a turn, with its body cut.
fn cut_error(error: &AgentError) -> AgentError {
    AgentError {
        message: cut(&error.message).into_owned(),
    }
}

/// A body, cut to [`MAX_BODY_BYTES`].
fn cut(body: &str) -> Cow<'_, str> {
    if body.len() <= MAX_BODY_BYTES {
        return Cow::Borrowed(body);
    }
    let head = &body[..body.floor_char_boundary(MAX_BODY_BYTES)];
    Cow::Owned(format!("{head}{CUT_MARK}"))
}

/// Logs one agent event.
pub fn project(envelope: &AgentEventEnvelope) {
    match &envelope.event {
        AgentEvent::Turn(event) => project_turn(envelope, event),
        AgentEvent::Stream(event) => project_stream(envelope, event),
        AgentEvent::Tool(event) => project_tool(envelope, event),
        AgentEvent::TodosChanged { todos } => {
            event!(info, envelope, items = todos.items.len(), "todos changed")
        }
        AgentEvent::Usage { usage } => event!(
            info,
            envelope,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            cache_read_tokens = usage.cache_read_tokens,
            reasoning_tokens = usage.reasoning_tokens,
            "usage"
        ),
    }
}

fn project_turn(envelope: &AgentEventEnvelope, event: &TurnEvent) {
    match event {
        TurnEvent::Started => event!(info, envelope, "turn started"),
        TurnEvent::Prompt { text } => event!(info, envelope, text = %text, "turn prompt"),
        TurnEvent::StepStarted { index } => event!(info, envelope, index, "step started"),
        TurnEvent::StepFinished {
            index,
            stop,
            provider_ms,
        } => event!(
            info,
            envelope,
            index,
            stop = ?stop,
            provider_ms,
            "step finished"
        ),
        TurnEvent::StepRejected {
            index,
            error,
            stop_reason,
            blocks,
            usage,
            provider_ms,
        } => event!(
            warn,
            envelope,
            index,
            error = %error.message,
            stop_reason = ?stop_reason,
            blocks,
            input_tokens = usage.map(|usage| usage.input_tokens),
            output_tokens = usage.map(|usage| usage.output_tokens),
            provider_ms,
            "the reply cannot end the turn"
        ),
        TurnEvent::UserAppended { text } => {
            event!(info, envelope, text = %text, "user message appended")
        }
        TurnEvent::NoticeAppended { text } => {
            event!(info, envelope, text = %text, "notice appended")
        }
        TurnEvent::Completed { usage, duration_ms } => event!(
            info,
            envelope,
            duration_ms,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            cache_read_tokens = usage.cache_read_tokens,
            reasoning_tokens = usage.reasoning_tokens,
            "turn completed"
        ),
        TurnEvent::ProviderRetried {
            retry,
            waited_ms,
            status,
        } => event!(
            info,
            envelope,
            retry,
            waited_ms,
            status = ?status,
            "provider asked again"
        ),
        TurnEvent::Cancelled { duration_ms } => {
            event!(info, envelope, duration_ms, "turn cancelled")
        }
        TurnEvent::Failed { error, duration_ms } => {
            event!(warn, envelope, duration_ms, error = %error.message, "turn failed")
        }
        TurnEvent::LoopLimitReached {
            request_id,
            max_iters,
        } => event!(
            warn,
            envelope,
            request_id = request_id.0,
            max_iters,
            "loop limit reached"
        ),
    }
}

fn project_stream(envelope: &AgentEventEnvelope, event: &StreamEvent) {
    match event {
        StreamEvent::TextDelta { text } => event!(trace, envelope, text = %text, "text delta"),
        StreamEvent::ThinkingDelta { text } => {
            event!(trace, envelope, text = %text, "thinking delta")
        }
        StreamEvent::ThinkingDone { duration_ms } => {
            event!(info, envelope, duration_ms, "thinking done")
        }
    }
}

fn project_tool(envelope: &AgentEventEnvelope, event: &ToolEvent) {
    match event {
        ToolEvent::ApprovalRequested {
            request_id,
            call_id,
            name,
            view,
        } => event!(
            info,
            envelope,
            request_id = request_id.0,
            call_id = call_id.0,
            name = %name,
            view = %view.summary,
            "tool approval requested"
        ),
        ToolEvent::Started {
            call_id,
            name,
            view,
        } => event!(
            info,
            envelope,
            call_id = call_id.0,
            name = %name,
            view = %view.summary,
            "tool started"
        ),
        ToolEvent::QuestionAsked {
            request_id,
            question,
        } => event!(
            info,
            envelope,
            request_id = request_id.0,
            question = %question.question,
            "tool asked a question"
        ),
        ToolEvent::OutputDelta {
            call_id,
            stream,
            text,
        } => event!(
            trace,
            envelope,
            call_id = call_id.0,
            stream = ?stream,
            text = %text,
            "tool output"
        ),
        ToolEvent::Finished {
            call_id,
            name,
            result,
            duration_ms,
            ..
        } => match result {
            ToolResult::Success { .. } => event!(
                info,
                envelope,
                call_id = call_id.0,
                name = %name,
                duration_ms,
                "tool finished"
            ),
            ToolResult::Failed { error, .. } => event!(
                warn,
                envelope,
                call_id = call_id.0,
                name = %name,
                duration_ms,
                error = %error,
                "tool failed"
            ),
            ToolResult::Rejected { reason } => event!(
                info,
                envelope,
                call_id = call_id.0,
                name = %name,
                reason = %reason,
                "tool rejected"
            ),
            ToolResult::Cancelled => {
                event!(info, envelope, call_id = call_id.0, name = %name, "tool cancelled")
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use oven_llm::{StopReason, Usage};
    use oven_proto::{AgentError, AgentId, TurnId};
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    /// Keeps what the projection wrote, so a test can read it back.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture(projected: impl FnOnce()) -> String {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_target(false)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, projected);
        let written = capture.0.lock().unwrap().clone();
        String::from_utf8(written).expect("the log is text")
    }

    fn envelope(event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            agent_id: AgentId(1),
            turn_id: TurnId(2),
            at: 0,
            elapsed_ms: 0,
            event,
        }
    }

    fn event(index: usize) -> AppEvent {
        AppEvent::new(AppEventKind::Agent(envelope(AgentEvent::Turn(
            TurnEvent::StepStarted { index },
        ))))
    }

    /// An event that happened at a known moment of the run.
    fn timed(index: usize, elapsed_ms: u64) -> AppEvent {
        AppEvent::new(AppEventKind::Agent(AgentEventEnvelope {
            agent_id: AgentId(1),
            turn_id: TurnId(2),
            at: 0,
            elapsed_ms,
            event: AgentEvent::Turn(TurnEvent::StepStarted { index }),
        }))
    }

    /// The event that ends a turn.
    fn completed() -> AppEvent {
        AppEvent::new(AppEventKind::Agent(envelope(AgentEvent::Turn(
            TurnEvent::Completed {
                usage: Usage::default(),
                duration_ms: 12,
            },
        ))))
    }

    /// Waits for the trace to hold `marker`, the way a reader of a run in
    /// progress sees it.
    async fn wait_for(path: &Path, marker: &str) -> String {
        for _ in 0..200 {
            if let Ok(written) = std::fs::read_to_string(path)
                && written.contains(marker)
            {
                return written;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the trace did not reach the disk: {path:?}");
    }

    /// Writes a trace of two events, and returns the file it wrote.
    async fn write_trace(dir: &Path) -> PathBuf {
        write_trace_at(dir, TraceSettings::default()).await
    }

    /// Writes a trace of two events, as deep as it is asked for, and
    /// returns the file it wrote.
    async fn write_trace_at(dir: &Path, settings: TraceSettings) -> PathBuf {
        let run = start(Some("session-1"));
        let path = trace_path(dir, run.id());
        let (sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(dir.to_path_buf(), run, events, settings)
            .expect("the run keeps a trace");
        let task = tokio::spawn(writer.run());
        for index in 1..=2 {
            sender.send(Arc::new(event(index))).unwrap();
        }
        drop(sender);
        task.await.expect("the writer finishes when the bus stops");
        path
    }

    #[tokio::test]
    async fn the_trace_is_on_the_disk_at_the_end_of_a_turn() {
        let dir = tempdir::TempDir::new("oven-trace-flush").unwrap();
        let run = start(Some("session-1"));
        let path = trace_path(dir.path(), run.id());
        let (sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(
            dir.path().to_path_buf(),
            run,
            events,
            TraceSettings::default(),
        )
        .expect("the run keeps a trace");
        let task = tokio::spawn(writer.run());

        sender.send(Arc::new(event(1))).unwrap();
        sender.send(Arc::new(completed())).unwrap();

        let written = wait_for(&path, "\"kind\":\"completed\"").await;
        assert!(written.contains("\"index\":1"), "{written}");

        drop(sender);
        task.await.expect("the writer finishes when the bus stops");
    }

    #[test]
    fn a_retried_call_is_one_line() {
        let line = capture(|| {
            project(&envelope(AgentEvent::Turn(TurnEvent::ProviderRetried {
                retry: 1,
                waited_ms: 30_000,
                status: Some(429),
            })));
        });

        assert!(line.contains("provider asked again"), "{line}");
        assert!(line.contains("agent_id=1"), "{line}");
        assert!(line.contains("turn_id=2"), "{line}");
        assert!(line.contains("retry=1"), "{line}");
        assert!(line.contains("waited_ms=30000"), "{line}");
        assert!(line.contains("status=Some(429)"), "{line}");
    }

    #[test]
    fn a_rejected_reply_says_what_the_provider_sent() {
        let line = capture(|| {
            project(&envelope(AgentEvent::Turn(TurnEvent::StepRejected {
                index: 2,
                error: AgentError::from("the model returned no answer"),
                stop_reason: Some(StopReason::EndTurn),
                blocks: 1,
                usage: Some(Usage {
                    input_tokens: 1200,
                    output_tokens: 653,
                    cache_read_tokens: 0,
                    reasoning_tokens: 653,
                }),
                provider_ms: 410,
            })));
        });

        assert!(line.contains("the reply cannot end the turn"), "{line}");
        assert!(line.contains("index=2"), "{line}");
        assert!(
            line.contains("error=the model returned no answer"),
            "{line}"
        );
        assert!(line.contains("stop_reason=Some(EndTurn)"), "{line}");
        assert!(line.contains("blocks=1"), "{line}");
        assert!(line.contains("output_tokens=653"), "{line}");
        assert!(line.contains("provider_ms=410"), "{line}");
    }

    #[test]
    fn a_finished_call_says_how_long_it_took() {
        let line = capture(|| {
            project(&envelope(AgentEvent::Tool(ToolEvent::Finished {
                call_id: oven_proto::ToolCallId(7),
                name: "file_read".into(),
                result: ToolResult::Success {
                    output: "ok".into(),
                },
                detail: None,
                duration_ms: 42,
            })));
        });

        assert!(line.contains("tool finished"), "{line}");
        assert!(line.contains("name=file_read"), "{line}");
        assert!(line.contains("duration_ms=42"), "{line}");
    }

    #[tokio::test]
    async fn the_trace_holds_the_events_in_order() {
        let dir = tempdir::TempDir::new("oven-trace").unwrap();
        let path = write_trace(dir.path()).await;
        assert_eq!(
            path.parent(),
            Some(dir.path().join(".oven").join("trace").as_path()),
            "the trace sits beside the workspace"
        );

        let written = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = written.lines().collect();
        assert_eq!(lines.len(), 3, "{written}");
        assert!(lines[0].contains("\"kind\":\"run\""), "{}", lines[0]);
        assert!(lines[1].contains("\"index\":1"), "{}", lines[1]);
        assert!(lines[2].contains("\"index\":2"), "{}", lines[2]);
        assert!(lines[1].contains("\"turn_id\":2"), "{}", lines[1]);
    }

    #[tokio::test]
    async fn a_trace_is_read_back_as_the_events_it_holds() {
        let dir = tempdir::TempDir::new("oven-trace-read").unwrap();
        let path = write_trace(dir.path()).await;
        let id = path.file_stem().unwrap().to_str().unwrap().to_owned();

        let trace = read_trace(dir.path(), &id, None).await.unwrap();
        assert_eq!(trace.events.len(), 2);
        assert!(
            matches!(
                trace.events[0].event,
                AgentEvent::Turn(TurnEvent::StepStarted { index: 1 })
            ),
            "the event comes back as it went in"
        );
        assert_eq!(trace.run.as_ref().unwrap().session_id(), Some("session-1"));

        let traces = list_traces(dir.path()).await.unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].id, id);
        assert_eq!(traces[0].session_id.as_deref(), Some("session-1"));
        assert!(traces[0].bytes > 0);
    }

    #[tokio::test]
    async fn a_run_without_events_is_still_a_run() {
        let dir = tempdir::TempDir::new("oven-trace-empty").unwrap();
        let run = start(Some("session-1"));
        let (sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(
            dir.path().to_path_buf(),
            run,
            events,
            TraceSettings::default(),
        )
        .expect("the run keeps a trace");
        let task = tokio::spawn(writer.run());
        drop(sender);
        task.await.unwrap();

        let traces = list_traces(dir.path()).await.unwrap();
        assert_eq!(traces.len(), 1, "the run left a trace behind");
        assert_eq!(traces[0].session_id.as_deref(), Some("session-1"));
    }

    #[tokio::test]
    async fn a_damaged_line_does_not_hide_the_rest_of_the_trace() {
        let dir = tempdir::TempDir::new("oven-trace-damaged").unwrap();
        let path = write_trace(dir.path()).await;
        let mut written = std::fs::read_to_string(&path).unwrap();
        written.push_str("{\"agent_id\":1,\"turn_id\":2,\"at\":0,\"even");
        std::fs::write(&path, written).unwrap();
        let id = path.file_stem().unwrap().to_str().unwrap().to_owned();

        let trace = read_trace(dir.path(), &id, None).await.unwrap();
        assert_eq!(trace.events.len(), 2, "the good events are still there");
        assert_eq!(trace.damaged, 1, "the damaged line is counted");
    }

    #[tokio::test]
    async fn a_trace_of_nothing_but_damage_is_an_error() {
        let dir = tempdir::TempDir::new("oven-trace-broken").unwrap();
        let path = trace_path(dir.path(), "broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"agent_id\":1,\"turn_id\":2,\"at\":0,\"even").unwrap();

        let error = read_trace(dir.path(), "broken", None).await.unwrap_err();
        assert!(
            matches!(error, TraceError::Parse { line: 1, .. }),
            "{error}"
        );
    }

    /// The trace as the first build that wrote traces wrote it: a header
    /// without a version, envelopes without `elapsed_ms`, and `attempts`
    /// where the field is now called `retry`.
    const LEGACY_TRACE: &str = r#"{"kind":"run","id":"0193c2a1b4d5","session_id":"session-1","started_at":1000}
{"agent_id":1,"turn_id":2,"at":1001,"event":{"kind":"turn","data":{"kind":"started"}}}
{"agent_id":1,"turn_id":2,"at":1002,"event":{"kind":"turn","data":{"kind":"step_finished","index":1,"stop":"final_answer","provider_ms":5}}}
{"agent_id":1,"turn_id":2,"at":1003,"event":{"kind":"turn","data":{"kind":"provider_retried","attempts":2,"waited_ms":300,"status":429}}}
{"agent_id":1,"turn_id":2,"at":1004,"event":{"kind":"tool","data":{"kind":"finished","call_id":7,"name":"bash","result":{"kind":"success","output":"hi"},"detail":null,"duration_ms":42}}}
"#;

    #[tokio::test]
    async fn a_trace_written_before_the_format_named_itself_still_reads() {
        let dir = tempdir::TempDir::new("oven-trace-legacy").unwrap();
        let path = trace_path(dir.path(), "legacy");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, LEGACY_TRACE).unwrap();

        let trace = read_trace(dir.path(), "legacy", None).await.unwrap();
        assert_eq!(trace.damaged, 0, "every line of the old shape reads");
        assert_eq!(trace.events.len(), 4);
        assert_eq!(
            trace.run.as_ref().unwrap().version(),
            0,
            "the header names no version"
        );
        assert_eq!(
            trace.events[0].elapsed_ms, 0,
            "the moment the old trace did not carry reads as zero"
        );
        assert!(
            matches!(
                &trace.events[2].event,
                AgentEvent::Turn(TurnEvent::ProviderRetried { retry: 2, .. })
            ),
            "the field the old trace called attempts reads as the retry number: {:?}",
            trace.events[2].event
        );
    }

    /// The lines of a trace, as this build writes them. The shape is
    /// persisted: changing it moves the version, rather than breaking the
    /// files that are already out there.
    const PINNED_RUN: &str = r#"{"kind":"run","version":1,"id":"0193c2a1b4d5","session_id":"session-1","started_at":1000}"#;
    const PINNED_FINISHED: &str = r#"{"agent_id":1,"turn_id":2,"at":3,"elapsed_ms":4,"event":{"kind":"tool","data":{"kind":"finished","call_id":7,"name":"bash","result":{"kind":"success","output":"hi"},"detail":null,"duration_ms":42}}}"#;
    const PINNED_COMPLETED: &str = r#"{"agent_id":1,"turn_id":2,"at":5,"elapsed_ms":6,"event":{"kind":"turn","data":{"kind":"completed","usage":{"input_tokens":1,"output_tokens":2},"duration_ms":7}}}"#;

    #[test]
    fn the_lines_of_a_trace_keep_their_shape() {
        let run = TraceRun::Run {
            version: TRACE_FORMAT_VERSION,
            id: "0193c2a1b4d5".to_owned(),
            session_id: Some("session-1".to_owned()),
            started_at: 1000,
        };
        assert_eq!(&serde_json::to_string(&run).unwrap(), PINNED_RUN);

        let finished = AgentEventEnvelope {
            agent_id: AgentId(1),
            turn_id: TurnId(2),
            at: 3,
            elapsed_ms: 4,
            event: AgentEvent::Tool(ToolEvent::Finished {
                call_id: oven_proto::ToolCallId(7),
                name: "bash".into(),
                result: ToolResult::Success {
                    output: "hi".to_owned(),
                },
                detail: None,
                duration_ms: 42,
            }),
        };
        assert_eq!(&serde_json::to_string(&finished).unwrap(), PINNED_FINISHED);

        let completed = AgentEventEnvelope {
            agent_id: AgentId(1),
            turn_id: TurnId(2),
            at: 5,
            elapsed_ms: 6,
            event: AgentEvent::Turn(TurnEvent::Completed {
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    ..Usage::default()
                },
                duration_ms: 7,
            }),
        };
        assert_eq!(
            &serde_json::to_string(&completed).unwrap(),
            PINNED_COMPLETED
        );
    }

    #[tokio::test]
    async fn a_trace_of_a_newer_oven_is_refused_with_its_version() {
        let dir = tempdir::TempDir::new("oven-trace-newer").unwrap();
        let path = trace_path(dir.path(), "newer");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let header = TraceRun::Run {
            version: TRACE_FORMAT_VERSION + 1,
            id: "newer".to_owned(),
            session_id: None,
            started_at: 1000,
        };
        std::fs::write(&path, serde_json::to_string(&header).unwrap()).unwrap();

        let error = read_trace(dir.path(), "newer", None).await.unwrap_err();
        assert!(
            matches!(
                error,
                TraceError::NewerFormat { version, .. }
                    if version == TRACE_FORMAT_VERSION + 1
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_trace_id_cannot_leave_the_trace_directory() {
        let dir = tempdir::TempDir::new("oven-trace-escape").unwrap();
        let error = read_trace(dir.path(), "../../../outside", None)
            .await
            .unwrap_err();
        assert!(matches!(error, TraceError::BadId { .. }), "{error}");
    }

    #[tokio::test]
    async fn the_tail_keeps_the_last_events_only() {
        let dir = tempdir::TempDir::new("oven-trace-tail").unwrap();
        let path = write_trace(dir.path()).await;
        let id = path.file_stem().unwrap().to_str().unwrap().to_owned();

        let trace = read_trace(dir.path(), &id, Some(1)).await.unwrap();
        assert_eq!(trace.events.len(), 1);
        assert!(matches!(
            trace.events[0].event,
            AgentEvent::Turn(TurnEvent::StepStarted { index: 2 })
        ));
    }

    #[tokio::test]
    async fn a_tail_still_says_where_the_run_started() {
        let dir = tempdir::TempDir::new("oven-trace-tail-base").unwrap();
        let run = start(Some("session-1"));
        let path = trace_path(dir.path(), run.id());
        let (sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(
            dir.path().to_path_buf(),
            run,
            events,
            TraceSettings::default(),
        )
        .expect("the run keeps a trace");
        let task = tokio::spawn(writer.run());
        for (index, elapsed_ms) in [(1, 100), (2, 200), (3, 300)] {
            sender.send(Arc::new(timed(index, elapsed_ms))).unwrap();
        }
        drop(sender);
        task.await.unwrap();
        let id = path.file_stem().unwrap().to_str().unwrap().to_owned();

        let trace = read_trace(dir.path(), &id, Some(1)).await.unwrap();
        assert_eq!(trace.events.len(), 1, "the tail keeps the last event");
        assert_eq!(
            trace.base_elapsed_ms, 100,
            "the run still starts at its first event"
        );
    }

    #[tokio::test]
    async fn a_trace_can_be_removed_and_pruned() {
        let dir = tempdir::TempDir::new("oven-trace-prune").unwrap();
        let first = write_trace(dir.path()).await;
        let second = write_trace(dir.path()).await;
        let id = second.file_stem().unwrap().to_str().unwrap().to_owned();

        remove_trace(dir.path(), &id).await.unwrap();
        assert!(!second.exists(), "the trace is gone");
        assert!(first.exists(), "the other trace is not touched");

        let removed = prune_traces(dir.path(), Some(0), None).await.unwrap();
        assert_eq!(removed, 1);
        assert!(!first.exists());
    }

    #[tokio::test]
    async fn the_run_being_closed_counts_as_one_of_the_newest() {
        let dir = tempdir::TempDir::new("oven-trace-prune-current").unwrap();
        let other = write_run(dir.path(), run_of("run-a", "session-1", 100)).await;
        let current = write_run(dir.path(), run_of("run-b", "session-1", 200)).await;
        // The clocks of the files are not the clocks of the runs: they can
        // agree on a moment, and a correction can leave a finished run's
        // file reading as the newest.
        set_modified(&other, SystemTime::now() + Duration::from_secs(3600));

        let removed = prune_to_newest(dir.path(), 1, "run-b").await.unwrap();

        assert_eq!(removed, 1);
        assert!(!other.exists(), "the run that is over goes");
        assert!(
            current.exists(),
            "the run being closed is one of the newest whatever its age"
        );
    }

    #[tokio::test]
    async fn a_workspace_without_traces_lists_none() {
        let dir = tempdir::TempDir::new("oven-trace-none").unwrap();
        assert!(list_traces(dir.path()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_workspace_keeps_only_the_newest_traces() {
        let dir = tempdir::TempDir::new("oven-trace-keep").unwrap();
        for _ in 0..3 {
            write_trace(dir.path()).await;
        }

        let traces = list_traces(dir.path()).await.unwrap();
        assert_eq!(traces.len(), DEFAULT_KEEP, "the older traces are gone");
    }

    #[tokio::test]
    async fn a_workspace_keeps_as_many_traces_as_it_is_told() {
        let dir = tempdir::TempDir::new("oven-trace-keep-three").unwrap();
        let settings = TraceSettings {
            keep: 3,
            ..TraceSettings::default()
        };
        for _ in 0..4 {
            write_trace_at(dir.path(), settings).await;
        }

        let traces = list_traces(dir.path()).await.unwrap();
        assert_eq!(traces.len(), 3, "the older traces are gone");
    }

    #[tokio::test]
    async fn a_workspace_that_keeps_no_limit_keeps_every_trace() {
        let dir = tempdir::TempDir::new("oven-trace-keep-all").unwrap();
        let settings = TraceSettings {
            keep: 0,
            ..TraceSettings::default()
        };
        for _ in 0..3 {
            write_trace_at(dir.path(), settings).await;
        }

        let traces = list_traces(dir.path()).await.unwrap();
        assert_eq!(traces.len(), 3, "nothing is pruned");
    }

    /// Writes a trace of one event, and returns the file it wrote.
    async fn write_event(dir: &Path, settings: TraceSettings, event: AppEvent) -> PathBuf {
        let run = start(Some("session-1"));
        let path = trace_path(dir, run.id());
        let (sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(dir.to_path_buf(), run, events, settings)
            .expect("the run keeps a trace");
        let task = tokio::spawn(writer.run());
        sender.send(Arc::new(event)).unwrap();
        drop(sender);
        task.await.expect("the writer finishes when the bus stops");
        path
    }

    /// The event that carries a word as it was said.
    fn said(text: &str) -> AppEvent {
        AppEvent::new(AppEventKind::Agent(envelope(AgentEvent::Stream(
            StreamEvent::TextDelta {
                text: text.to_owned(),
            },
        ))))
    }

    /// The event that says a tool printed something.
    fn printed(output: &str) -> AppEvent {
        AppEvent::new(AppEventKind::Agent(envelope(AgentEvent::Tool(
            ToolEvent::Finished {
                call_id: oven_proto::ToolCallId(7),
                name: "bash".into(),
                result: ToolResult::Success {
                    output: output.to_owned(),
                },
                detail: None,
                duration_ms: 42,
            },
        ))))
    }

    #[tokio::test]
    async fn the_trace_directory_ignores_itself() {
        let dir = tempdir::TempDir::new("oven-trace-gitignore").unwrap();
        let path = write_trace(dir.path()).await;

        let ignore = path.parent().unwrap().join(".gitignore");
        let written = std::fs::read_to_string(&ignore).unwrap();
        assert_eq!(written, "*\n", "{written}");
        assert!(
            !written.contains("!.gitignore"),
            "the rule ignores the directory itself, so git does not report the workspace \
             as changed: {written}"
        );
    }

    #[tokio::test]
    async fn the_skeleton_leaves_out_the_text_as_it_streamed() {
        let dir = tempdir::TempDir::new("oven-trace-shape").unwrap();
        let path = write_event(dir.path(), TraceSettings::default(), said("hello")).await;

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("text_delta"), "{written}");
        assert!(!written.contains("hello"), "{written}");
    }

    #[tokio::test]
    async fn the_full_trace_keeps_every_word() {
        let dir = tempdir::TempDir::new("oven-trace-full").unwrap();
        let path = write_event(
            dir.path(),
            TraceSettings {
                mode: TraceMode::Full,
                ..TraceSettings::default()
            },
            said("hello"),
        )
        .await;

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("text_delta"), "{written}");
        assert!(written.contains("hello"), "{written}");
    }

    #[tokio::test]
    async fn a_quiet_run_has_no_writer() {
        let dir = tempdir::TempDir::new("oven-trace-quiet").unwrap();
        let run = start(Some("session-1"));
        let path = trace_path(dir.path(), run.id());
        let (_sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(
            dir.path().to_path_buf(),
            run,
            events,
            TraceSettings {
                mode: TraceMode::Quiet,
                ..TraceSettings::default()
            },
        );

        assert!(writer.is_none(), "a quiet run writes no trace at all");
        assert!(!path.exists(), "the run left no file behind");
    }

    #[tokio::test]
    async fn the_skeleton_cuts_a_body_that_is_too_long() {
        let dir = tempdir::TempDir::new("oven-trace-cut").unwrap();
        let body = "x".repeat(MAX_BODY_BYTES * 2);
        let path = write_event(dir.path(), TraceSettings::default(), printed(&body)).await;
        let id = path.file_stem().unwrap().to_str().unwrap().to_owned();

        let trace = read_trace(dir.path(), &id, None).await.unwrap();
        let AgentEvent::Tool(ToolEvent::Finished {
            result: ToolResult::Success { output },
            ..
        }) = &trace.events[0].event
        else {
            panic!("the event is not the one that was written");
        };
        assert!(
            output.len() <= MAX_BODY_BYTES + CUT_MARK.len(),
            "the body is cut: {}",
            output.len()
        );
        assert!(output.ends_with(CUT_MARK), "the cut says so");
    }

    /// The moment a file was last written, as the test wants it.
    fn set_modified(path: &Path, when: SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    /// Writes the trace of a run that belonged to a session, and returns
    /// the file it wrote.
    async fn write_run(dir: &Path, run: TraceRun) -> PathBuf {
        let path = trace_path(dir, run.id());
        let (sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(
            dir.to_path_buf(),
            run,
            events,
            TraceSettings {
                keep: 0,
                ..TraceSettings::default()
            },
        )
        .expect("the run keeps a trace");
        let task = tokio::spawn(writer.run());
        sender.send(Arc::new(event(1))).unwrap();
        drop(sender);
        task.await.expect("the writer finishes when the bus stops");
        path
    }

    /// A run of a session, named and placed in time.
    fn run_of(id: &str, session: &str, started_at: u64) -> TraceRun {
        TraceRun::Run {
            version: TRACE_FORMAT_VERSION,
            id: id.to_owned(),
            session_id: Some(session.to_owned()),
            started_at,
        }
    }

    #[test]
    fn a_session_is_named_by_its_id_a_prefix_of_it_or_its_short_form() {
        let id = "0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678";

        assert!(session_matches(id, id));
        assert!(session_matches(id, "0193c2a1"));
        assert!(session_matches(id, "3e4f5678"));
        assert!(
            !session_matches(id, "3e4f567"),
            "a shorter tail names no id"
        );
        assert!(!session_matches(id, ""));
        assert!(!session_matches(id, "9a0b1c2d"));
    }

    #[tokio::test]
    async fn the_runs_of_a_session_are_the_traces_that_name_it() {
        let dir = tempdir::TempDir::new("oven-trace-session").unwrap();
        write_run(dir.path(), run_of("run-1", "session-1", 200)).await;
        write_run(dir.path(), run_of("run-2", "session-2", 300)).await;
        write_run(dir.path(), run_of("run-3", "session-1", 100)).await;

        let runs = traces_of_session(dir.path(), "session-1").await.unwrap();

        assert_eq!(runs.len(), 2, "the runs of the session: {runs:?}");
        assert_eq!(runs[0].id, "run-3", "the oldest run comes first");
        assert_eq!(runs[1].id, "run-1");
        assert_eq!(runs[0].started_at, Some(100));
    }

    #[tokio::test]
    async fn the_runs_of_a_session_are_read_back_together() {
        let dir = tempdir::TempDir::new("oven-trace-read-session").unwrap();
        write_run(dir.path(), run_of("run-1", "session-1", 200)).await;
        write_run(dir.path(), run_of("run-2", "session-1", 100)).await;

        let traces = read_session(dir.path(), "session-1", None).await.unwrap();

        assert_eq!(traces.len(), 2);
        assert_eq!(traces[0].run.as_ref().map(|run| run.id()), Some("run-2"));
        assert_eq!(traces[1].run.as_ref().map(|run| run.id()), Some("run-1"));
    }
}
