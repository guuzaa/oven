//! The tracing projection of the agent event stream.
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
//! The same stream is also written down, in full, as a trace file: the run
//! as it happened, one event per line. The log is the readable view; the
//! file is the record.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oven_proto::{
    AgentEvent, AgentEventEnvelope, AppEvent, AppEventKind, StreamEvent, ToolEvent, ToolResult,
    TurnEvent,
};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// The target every projected event is logged under.
const TARGET: &str = "oven.agent";

/// Logs one event under the agent target, with the ids every line carries.
macro_rules! event {
    ($level:ident, $envelope:expr, $($field:tt)*) => {
        tracing::$level!(
            target: TARGET,
            agent_id = $envelope.agent_id.0,
            turn_id = $envelope.turn_id.0,
            $($field)*
        )
    };
}

/// Where the trace of a session lives, under the workspace.
pub fn trace_path(root: &Path, session: &str) -> PathBuf {
    root.join(".oven")
        .join("trace")
        .join(format!("{session}.jsonl"))
}

/// Records the agent's events into a trace file, one per line.
///
/// The file is the run as it happened: the events, in the order they
/// happened, with the moment each one landed. It sits beside the
/// workspace rather than in the session file, because the session file is
/// the conversation and has to stay resumable.
pub struct TraceWriter {
    path: PathBuf,
    events: mpsc::UnboundedReceiver<Arc<AppEvent>>,
    file: Option<fs::File>,
}

impl TraceWriter {
    pub fn new(path: PathBuf, events: mpsc::UnboundedReceiver<Arc<AppEvent>>) -> Self {
        Self {
            path,
            events,
            file: None,
        }
    }

    /// Writes the events as they arrive, until the bus stops publishing.
    pub async fn run(mut self) {
        while let Some(event) = self.events.recv().await {
            let AppEventKind::Agent(envelope) = &event.kind else {
                continue;
            };
            if let Err(error) = self.append(envelope).await {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %error,
                    "the trace could not be written"
                );
                return;
            }
        }
    }

    /// Appends one event. The file is created on the first one.
    async fn append(&mut self, envelope: &AgentEventEnvelope) -> std::io::Result<()> {
        if self.file.is_none() {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir).await?;
            }
            self.file = Some(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .await?,
            );
        }
        let mut line = serde_json::to_string(envelope).map_err(std::io::Error::other)?;
        line.push('\n');
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("the trace file is not open"))?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await
    }
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
            attempts,
            waited_ms,
            status,
        } => event!(
            info,
            envelope,
            attempts,
            waited_ms,
            status = ?status,
            "provider retried"
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

    use oven_proto::{AgentId, TurnId};
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
            event,
        }
    }

    #[test]
    fn a_retried_call_is_one_line() {
        let line = capture(|| {
            project(&envelope(AgentEvent::Turn(TurnEvent::ProviderRetried {
                attempts: 2,
                waited_ms: 30_000,
                status: Some(429),
            })));
        });

        assert!(line.contains("provider retried"), "{line}");
        assert!(line.contains("agent_id=1"), "{line}");
        assert!(line.contains("turn_id=2"), "{line}");
        assert!(line.contains("attempts=2"), "{line}");
        assert!(line.contains("waited_ms=30000"), "{line}");
        assert!(line.contains("status=Some(429)"), "{line}");
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
        let path = trace_path(dir.path(), "s1");
        assert_eq!(
            path,
            dir.path().join(".oven").join("trace").join("s1.jsonl"),
            "the trace sits beside the workspace"
        );

        let (sender, events) = mpsc::unbounded_channel();
        let writer = TraceWriter::new(path.clone(), events);
        let task = tokio::spawn(writer.run());
        for index in 1..=2 {
            let event = AppEvent::new(AppEventKind::Agent(envelope(AgentEvent::Turn(
                TurnEvent::StepStarted { index },
            ))));
            sender.send(Arc::new(event)).unwrap();
        }
        drop(sender);
        task.await.expect("the writer finishes when the bus stops");

        let written = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = written.lines().collect();
        assert_eq!(lines.len(), 2, "{written}");
        assert!(lines[0].contains("\"index\":1"), "{}", lines[0]);
        assert!(lines[1].contains("\"index\":2"), "{}", lines[1]);
        assert!(lines[0].contains("\"turn_id\":2"), "{}", lines[0]);
    }
}
