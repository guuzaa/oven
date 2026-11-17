//! `oven trace`: the runs of this workspace, and what one of them did.

use std::fmt::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::{Args as ClapArgs, Subcommand};
use oven_app::trace::{self, TraceSummary};
use oven_app::{
    AgentEvent, AgentEventEnvelope, AppError, StepStop, StreamEvent, ToolEvent, ToolResult,
    TurnEvent,
};

use crate::commands::report;

const NO_TRACES: &str = "no traces";
const TIME_HEADER: &str = "  time     a/t  what happened";
const TEXT_WIDTH: usize = 160;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: TraceCommand,
}

#[derive(Debug, Subcommand)]
enum TraceCommand {
    /// List the runs of this workspace, newest first
    Ls,
    /// Show what one run did, in the order it did it
    Show(ShowArgs),
}

#[derive(Debug, ClapArgs)]
struct ShowArgs {
    /// The session id of the run
    #[arg(value_name = "ID")]
    id: String,
    /// Include what was said: the text of the turn and its reasoning
    #[arg(long)]
    text: bool,
}

pub(crate) async fn run(args: &Args, root: &Path) -> ExitCode {
    report(execute(args, root).await)
}

async fn execute(args: &Args, root: &Path) -> Result<String, AppError> {
    match &args.command {
        TraceCommand::Ls => Ok(render_list(&trace::list_traces(root).await?)),
        TraceCommand::Show(show) => {
            let events = trace::read_trace(root, &show.id).await?;
            Ok(render_timeline(&events, show.text))
        }
    }
}

/// The runs of the workspace, one per line: which run, how much it wrote,
/// and when it was last written.
fn render_list(traces: &[TraceSummary]) -> String {
    if traces.is_empty() {
        return NO_TRACES.to_owned();
    }
    let mut out = String::new();
    for trace in traces {
        let _ = writeln!(
            out,
            "{:<36}  {:>9}  {}",
            trace.id,
            format_bytes(trace.bytes),
            format_age(trace.modified)
        );
    }
    out.trim_end().to_owned()
}

/// The run as a timeline: what happened, how long it took, and who did it.
fn render_timeline(events: &[AgentEventEnvelope], text: bool) -> String {
    if events.is_empty() {
        return NO_TRACES.to_owned();
    }
    let start = events.first().map(|event| event.at).unwrap_or_default();
    let mut out = String::new();
    let _ = writeln!(out, "{TIME_HEADER}");
    for event in events {
        let Some(line) = describe(event, text) else {
            continue;
        };
        let _ = writeln!(
            out,
            "{:>8}  {:>4}  {}",
            format_offset(event.at.saturating_sub(start)),
            format!("{}/{}", event.agent_id.0, event.turn_id.0),
            line
        );
    }
    out.trim_end().to_owned()
}

/// What one event means in one line. `None` when the event says nothing the
/// timeline needs.
fn describe(event: &AgentEventEnvelope, with_text: bool) -> Option<String> {
    match &event.event {
        AgentEvent::Turn(turn) => describe_turn(turn),
        AgentEvent::Stream(stream) => describe_stream(stream, with_text),
        AgentEvent::Tool(tool) => describe_tool(tool, with_text),
        AgentEvent::TodosChanged { todos } => {
            Some(format!("todos changed · {} items", todos.items.len()))
        }
        AgentEvent::Usage { usage } => Some(format!(
            "usage · {} in, {} out",
            usage.input_tokens, usage.output_tokens
        )),
    }
}

fn describe_turn(turn: &TurnEvent) -> Option<String> {
    Some(match turn {
        TurnEvent::Started => "turn started".to_owned(),
        TurnEvent::Prompt { text } => format!("turn prompt · {}", brief(text)),
        TurnEvent::StepStarted { index } => format!("step {index} started"),
        TurnEvent::StepFinished {
            index,
            stop,
            provider_ms,
        } => format!(
            "step {index} finished · {} · provider {}",
            match stop {
                StepStop::FinalAnswer => "final answer",
                StepStop::ToolUse => "tool use",
            },
            format_duration(*provider_ms)
        ),
        TurnEvent::ProviderRetried {
            attempts,
            waited_ms,
            status,
        } => format!(
            "provider retried {attempts}× · waited {} · status {}",
            format_duration(*waited_ms),
            match status {
                Some(code) => code.to_string(),
                None => "unknown".to_owned(),
            }
        ),
        TurnEvent::UserAppended { text } => {
            format!("user message appended · {}", brief(text))
        }
        TurnEvent::NoticeAppended { text } => format!("notice appended · {}", brief(text)),
        TurnEvent::Completed { usage, duration_ms } => format!(
            "turn completed in {} · {} in, {} out",
            format_duration(*duration_ms),
            usage.input_tokens,
            usage.output_tokens
        ),
        TurnEvent::Cancelled { duration_ms } => {
            format!("turn cancelled after {}", format_duration(*duration_ms))
        }
        TurnEvent::Failed { error, duration_ms } => format!(
            "turn failed after {} · {}",
            format_duration(*duration_ms),
            error.message
        ),
        TurnEvent::LoopLimitReached { max_iters, .. } => {
            format!("loop limit reached · {max_iters} iterations")
        }
    })
}

fn describe_stream(stream: &StreamEvent, with_text: bool) -> Option<String> {
    match stream {
        StreamEvent::TextDelta { text } => with_text.then(|| format!("text · {}", brief(text))),
        StreamEvent::ThinkingDelta { text } => {
            with_text.then(|| format!("thinking · {}", brief(text)))
        }
        StreamEvent::ThinkingDone { duration_ms } => Some(format!(
            "thinking done in {}",
            format_duration(*duration_ms)
        )),
    }
}

fn describe_tool(tool: &ToolEvent, with_text: bool) -> Option<String> {
    Some(match tool {
        ToolEvent::ApprovalRequested { name, view, .. } => {
            format!("approval asked for {name} · {}", view.summary)
        }
        ToolEvent::Started { name, view, .. } => format!("{name} started · {}", view.summary),
        ToolEvent::QuestionAsked { question, .. } => {
            format!("a tool asked · {}", brief(&question.question))
        }
        ToolEvent::OutputDelta { text, .. } => {
            with_text.then(|| format!("tool output · {}", brief(text)))?
        }
        ToolEvent::Finished {
            name,
            result,
            duration_ms,
            ..
        } => match result {
            ToolResult::Success { .. } => {
                format!("{name} finished in {}", format_duration(*duration_ms))
            }
            ToolResult::Failed { error, .. } => format!(
                "{name} failed in {} · {error}",
                format_duration(*duration_ms)
            ),
            ToolResult::Rejected { reason } => format!("{name} rejected · {reason}"),
            ToolResult::Cancelled => format!("{name} cancelled"),
        },
    })
}

/// How long ago the trace was written.
fn format_age(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "unknown".to_owned();
    };
    let Ok(since) = SystemTime::now().duration_since(modified) else {
        return "just now".to_owned();
    };
    let minutes = since.as_secs() / 60;
    match minutes {
        0 => "just now".to_owned(),
        1 => "a minute ago".to_owned(),
        n if n < 60 => format!("{n} minutes ago"),
        n if n < 60 * 24 => format!("{} hours ago", n / 60),
        n => format!("{} days ago", n / (60 * 24)),
    }
}

/// How much a trace holds.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    match bytes {
        b if b < KB => format!("{b} B"),
        b if b < KB * 1024 => format!("{:.1} KB", b as f64 / KB as f64),
        b => format!("{:.1} MB", b as f64 / (KB * 1024) as f64),
    }
}

/// The time an event happened, counted from the first one.
fn format_offset(millis: u64) -> String {
    if millis < 1000 {
        format!("+{millis}ms")
    } else {
        format!("+{:.2}s", millis as f64 / 1000.0)
    }
}

/// How long something took.
fn format_duration(millis: u64) -> String {
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.2}s", millis as f64 / 1000.0)
    }
}

/// The short form of a piece of text: enough to tell what it was.
fn brief(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= TEXT_WIDTH {
        return text.to_owned();
    }
    let head: String = text.chars().take(TEXT_WIDTH).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use oven_app::{AgentId, TurnId};
    use oven_llm::Usage;

    use super::*;

    fn envelope(at: u64, event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            agent_id: AgentId(1),
            turn_id: TurnId(2),
            at,
            event,
        }
    }

    #[test]
    fn the_timeline_says_what_happened_and_when() {
        let events = vec![
            envelope(1_000, AgentEvent::Turn(TurnEvent::Started)),
            envelope(
                1_500,
                AgentEvent::Turn(TurnEvent::StepFinished {
                    index: 1,
                    stop: StepStop::FinalAnswer,
                    provider_ms: 400,
                }),
            ),
            envelope(
                2_600,
                AgentEvent::Turn(TurnEvent::Completed {
                    usage: Usage::default(),
                    duration_ms: 1_600,
                }),
            ),
        ];

        let timeline = render_timeline(&events, false);

        assert!(timeline.contains("+0ms"), "{timeline}");
        assert!(timeline.contains("+500ms"), "{timeline}");
        assert!(timeline.contains("+1.60s"), "{timeline}");
        assert!(timeline.contains("1/2"), "{timeline}");
        assert!(
            timeline.contains("step 1 finished · final answer · provider 400ms"),
            "{timeline}"
        );
    }

    #[test]
    fn what_was_said_is_left_out_unless_it_is_asked_for() {
        let events = vec![envelope(
            1_000,
            AgentEvent::Stream(StreamEvent::TextDelta {
                text: "the answer".into(),
            }),
        )];

        assert!(!render_timeline(&events, false).contains("the answer"));
        assert!(render_timeline(&events, true).contains("the answer"));
    }
}
