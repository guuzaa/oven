//! How a trace reads on the screen.

use std::fmt::Write;
use std::time::{Duration, SystemTime};

use oven_app::trace::{Trace, TraceRun, TraceSummary};
use oven_app::{
    AgentEvent, AgentEventEnvelope, StepStop, StreamEvent, ToolEvent, ToolResult, TurnEvent,
};

const NO_TRACES: &str = "no traces";
const NO_EVENTS: &str = "the run raised no events";
const TIME_HEADER: &str = "  time     a/t  what happened";
const TEXT_WIDTH: usize = 160;
const MINUTES_PER_HOUR: u64 = 60;
const MINUTES_PER_DAY: u64 = 60 * 24;

/// The runs of the workspace, one per line: which run, which session, how
/// much it wrote, and when it was last written.
pub fn list(traces: &[TraceSummary]) -> String {
    if traces.is_empty() {
        return NO_TRACES.to_owned();
    }
    let sessions: Vec<String> = traces
        .iter()
        .map(|trace| short_id(trace.session_id.as_deref()))
        .collect();
    let sizes: Vec<String> = traces
        .iter()
        .map(|trace| format_bytes(trace.bytes))
        .collect();
    let id_width = width_of(traces.iter().map(|trace| trace.id.as_str()), "run");
    let session_width = width_of(sessions.iter().map(|id| id.as_str()), "session");
    let size_width = width_of(sizes.iter().map(|size| size.as_str()), "size");

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<id_width$}  {:<session_width$}  {:>size_width$}  age",
        "run", "session", "size",
    );
    for ((trace, session), size) in traces.iter().zip(sessions.iter()).zip(sizes.iter()) {
        let _ = writeln!(
            out,
            "{:<id_width$}  {:<session_width$}  {:>size_width$}  {}",
            trace.id,
            session,
            size,
            format_age(trace.modified),
        );
    }
    out.trim_end().to_owned()
}

/// The run as a timeline: what happened, how long it took, and who did it.
pub fn timeline(trace: &Trace, text: bool) -> String {
    let mut out = String::new();
    if let Some(run) = &trace.run {
        let _ = writeln!(
            out,
            "run {} · session {} · started {}",
            run.id(),
            short_id(Some(run.session_id())),
            format_age(started_at(run))
        );
    }
    if trace.events.is_empty() {
        if trace.damaged == 0 {
            let _ = writeln!(out, "{NO_EVENTS}");
            return out.trim_end().to_owned();
        }
        let _ = writeln!(out, "{}", damaged_note(trace.damaged));
        return out.trim_end().to_owned();
    }
    // The events are put back in the order they happened: an event the agent
    // reports late can carry an earlier moment, and a timeline that goes
    // backwards cannot be read.
    let mut ordered: Vec<&AgentEventEnvelope> = trace.events.iter().collect();
    ordered.sort_by_key(|event| event.elapsed_ms);
    let _ = writeln!(out, "{TIME_HEADER}");
    for event in ordered {
        let Some(line) = describe(event, text) else {
            continue;
        };
        let _ = writeln!(
            out,
            "{:>8}  {:>4}  {}",
            format_offset(event.elapsed_ms.saturating_sub(trace.base_elapsed_ms)),
            format!("{}/{}", event.agent_id.0, event.turn_id.0),
            line
        );
    }
    if trace.damaged > 0 {
        let _ = writeln!(out, "{}", damaged_note(trace.damaged));
    }
    out.trim_end().to_owned()
}

/// When the run started, as a moment on the wall clock.
fn started_at(run: &TraceRun) -> Option<SystemTime> {
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(run.started_at()))
}

/// What the reader says about the lines it could not read.
fn damaged_note(damaged: usize) -> String {
    let plural = if damaged == 1 {
        "line was"
    } else {
        "lines were"
    };
    format!("{damaged} {plural} damaged and left out")
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
            retry,
            waited_ms,
            status,
        } => format!(
            "provider asked again (retry {retry}) · waited {} · status {}",
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

/// The width of the widest of the given strings, or of the header.
fn width_of<'a>(values: impl Iterator<Item = &'a str>, header: &str) -> usize {
    values
        .map(|value| value.chars().count())
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or(0)
}

/// The short form of an id: the last few characters, the way the app names
/// a session in a log line.
fn short_id(id: Option<&str>) -> String {
    match id {
        Some(id) => oven_app::session::short_session_id(id).to_owned(),
        None => "-".to_owned(),
    }
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
        n if n < MINUTES_PER_HOUR => format!("{n} minutes ago"),
        n if n < MINUTES_PER_DAY => {
            let hours = n / MINUTES_PER_HOUR;
            let plural = if hours == 1 { "hour" } else { "hours" };
            format!("{hours} {plural} ago")
        }
        n => {
            let days = n / MINUTES_PER_DAY;
            let plural = if days == 1 { "day" } else { "days" };
            format!("{days} {plural} ago")
        }
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

    fn envelope(elapsed_ms: u64, event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            agent_id: AgentId(1),
            turn_id: TurnId(2),
            at: 0,
            elapsed_ms,
            event,
        }
    }

    fn trace(events: Vec<AgentEventEnvelope>) -> Trace {
        let base_elapsed_ms = events
            .first()
            .map(|event| event.elapsed_ms)
            .unwrap_or_default();
        Trace {
            run: None,
            events,
            damaged: 0,
            base_elapsed_ms,
        }
    }

    #[test]
    fn the_timeline_says_what_happened_and_when() {
        let trace = trace(vec![
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
        ]);

        let timeline = timeline(&trace, false);

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
        let trace = trace(vec![envelope(
            1_000,
            AgentEvent::Stream(StreamEvent::TextDelta {
                text: "the answer".into(),
            }),
        )]);

        assert!(!timeline(&trace, false).contains("the answer"));
        assert!(timeline(&trace, true).contains("the answer"));
    }

    #[test]
    fn a_run_without_events_says_so() {
        assert_eq!(timeline(&trace(Vec::new()), false), NO_EVENTS);
    }

    #[test]
    fn a_damaged_line_is_reported() {
        let mut trace = trace(vec![envelope(0, AgentEvent::Turn(TurnEvent::Started))]);
        trace.damaged = 2;

        assert!(
            timeline(&trace, false).contains("2 lines were damaged"),
            "{trace:?}"
        );
    }

    #[test]
    fn the_list_has_a_header_and_the_short_ids() {
        let traces = vec![TraceSummary {
            id: "a3f9c2b1d4e7".to_owned(),
            session_id: Some("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678".to_owned()),
            bytes: 2048,
            modified: None,
        }];

        let list = list(&traces);

        assert!(list.starts_with("run"), "{list}");
        assert!(list.contains("a3f9c2b1d4e7"), "{list}");
        assert!(list.contains("3e4f5678"), "{list}");
        assert!(list.contains("2.0 KB"), "{list}");
    }

    #[test]
    fn an_age_of_one_hour_is_one_hour() {
        let hour_ago = SystemTime::now() - std::time::Duration::from_secs(60 * 60);
        assert_eq!(format_age(Some(hour_ago)), "1 hour ago");
    }

    #[test]
    fn the_timeline_puts_the_events_back_in_the_order_they_happened() {
        let mut trace = trace(vec![
            envelope(2_000, AgentEvent::Turn(TurnEvent::StepStarted { index: 2 })),
            envelope(1_000, AgentEvent::Turn(TurnEvent::StepStarted { index: 1 })),
        ]);
        trace.base_elapsed_ms = 1_000;

        let timeline = timeline(&trace, false);

        let first = timeline.find("step 1 started").expect("the first step");
        let second = timeline.find("step 2 started").expect("the second step");
        assert!(first < second, "{timeline}");
    }

    #[test]
    fn the_timeline_says_when_the_run_started() {
        let mut trace = trace(vec![envelope(0, AgentEvent::Turn(TurnEvent::Started))]);
        trace.run = Some(TraceRun::Run {
            id: "a3f9c2b1d4e7".to_owned(),
            session_id: "0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678".to_owned(),
            started_at: unix_ms(SystemTime::now() - Duration::from_secs(60 * 60)),
        });

        let timeline = timeline(&trace, false);

        assert!(
            timeline.contains("run a3f9c2b1d4e7 · session 3e4f5678 · started 1 hour ago"),
            "{timeline}"
        );
    }

    /// A moment on the wall clock, in Unix milliseconds.
    fn unix_ms(at: SystemTime) -> u64 {
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
}
