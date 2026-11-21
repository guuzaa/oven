//! How a trace reads on the screen.

mod draw;
mod format;
mod label;
mod list;
mod model;

pub use list::list;

use std::fmt::Write;
use std::time::{Duration, SystemTime};

use oven_app::trace::{Trace, TraceRun};

use self::format::{age, plural, short_id};

const NO_EVENTS: &str = "the run raised no events";
const NO_RUNS: &str = "the session has no runs left";

/// The run as a tree: which agent, which turn, which step, and what happened
/// inside them. Every step says when it started, counted from the first
/// event of the run.
pub fn tree(trace: &Trace, text: bool) -> String {
    let mut out = String::new();
    if let Some(run) = &trace.run {
        let _ = writeln!(
            out,
            "run {} · session {} · started {}",
            run.id(),
            short_id(run.session_id()),
            age(started_at(run))
        );
    }
    if trace.events.is_empty() {
        let note = if trace.damaged == 0 {
            NO_EVENTS.to_owned()
        } else {
            damaged_note(trace.damaged)
        };
        let _ = writeln!(out, "{note}");
        return out.trim_end().to_owned();
    }
    let forest = model::build(trace, text);
    for line in draw::draw(&forest, &label::label) {
        let _ = writeln!(out, "{line}");
    }
    if trace.damaged > 0 {
        let _ = writeln!(out, "{}", damaged_note(trace.damaged));
    }
    out.trim_end().to_owned()
}

/// The runs of one session, oldest first: each run a tree of its own.
///
/// A session that is resumed writes a run of its own, so a session reads as
/// the runs it went through, one after the other.
pub fn session(traces: &[Trace], text: bool) -> String {
    if traces.is_empty() {
        return NO_RUNS.to_owned();
    }
    traces
        .iter()
        .map(|trace| tree(trace, text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// When the run started, as a moment on the wall clock.
fn started_at(run: &TraceRun) -> Option<SystemTime> {
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(run.started_at()))
}

/// What the reader says about the lines it could not read.
fn damaged_note(damaged: usize) -> String {
    let word = plural(damaged as u64, "line was", "lines were");
    format!("{damaged} {word} damaged and left out")
}

#[cfg(test)]
mod tests {
    use oven_app::trace::TRACE_FORMAT_VERSION;
    use oven_app::{
        AgentEvent, AgentEventEnvelope, AgentId, StepStop, StreamEvent, ToolCallId, ToolEvent,
        ToolOutputStream, ToolView, TurnEvent, TurnId,
    };
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
    fn the_tree_says_what_happened_and_when() {
        let trace = trace(vec![
            envelope(1_000, AgentEvent::Turn(TurnEvent::Started)),
            envelope(1_930, AgentEvent::Turn(TurnEvent::StepStarted { index: 1 })),
            envelope(
                2_430,
                AgentEvent::Turn(TurnEvent::StepFinished {
                    index: 1,
                    stop: StepStop::FinalAnswer,
                    provider_ms: 400,
                }),
            ),
            envelope(
                3_530,
                AgentEvent::Turn(TurnEvent::Completed {
                    usage: Usage::default(),
                    duration_ms: 1_600,
                }),
            ),
        ]);

        let tree = tree(&trace, false);

        assert!(tree.contains("agent 1"), "{tree}");
        assert!(tree.contains("turn 2"), "{tree}");
        assert!(
            tree.contains("step 1 · +930ms · 500ms · final answer · provider 400ms"),
            "{tree}"
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

        assert!(!tree(&trace, false).contains("the answer"));
        assert!(tree(&trace, true).contains("the answer"));
    }

    #[test]
    fn a_run_without_events_says_so() {
        assert_eq!(tree(&trace(Vec::new()), false), NO_EVENTS);
    }

    #[test]
    fn a_damaged_line_is_reported() {
        let mut trace = trace(vec![envelope(0, AgentEvent::Turn(TurnEvent::Started))]);
        trace.damaged = 2;

        assert!(
            tree(&trace, false).contains("2 lines were damaged"),
            "{trace:?}"
        );
    }

    /// A run of a session, as the trace file names it.
    fn run_of(id: &str) -> TraceRun {
        TraceRun::Run {
            version: TRACE_FORMAT_VERSION,
            id: id.to_owned(),
            session_id: Some("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678".to_owned()),
            started_at: unix_ms(SystemTime::now() - Duration::from_secs(60)),
        }
    }

    #[test]
    fn the_runs_of_a_session_read_one_after_the_other() {
        let mut first = trace(vec![envelope(0, AgentEvent::Turn(TurnEvent::Started))]);
        first.run = Some(run_of("a3f9c2b1d4e7"));
        let mut second = trace(vec![envelope(0, AgentEvent::Turn(TurnEvent::Started))]);
        second.run = Some(run_of("b2c1d0e9f8a7"));

        let session = session(&[first, second], false);

        assert!(session.contains("run a3f9c2b1d4e7"), "{session}");
        assert!(session.contains("run b2c1d0e9f8a7"), "{session}");
        assert!(
            session.find("a3f9c2b1d4e7").unwrap() < session.find("b2c1d0e9f8a7").unwrap(),
            "{session}"
        );
    }

    #[test]
    fn a_session_without_runs_says_so() {
        assert_eq!(session(&[], false), NO_RUNS);
    }

    #[test]
    fn the_tree_puts_the_events_back_in_the_order_they_happened() {
        let mut trace = trace(vec![
            envelope(2_000, AgentEvent::Turn(TurnEvent::StepStarted { index: 2 })),
            envelope(1_000, AgentEvent::Turn(TurnEvent::StepStarted { index: 1 })),
        ]);
        trace.base_elapsed_ms = 1_000;

        let tree = tree(&trace, false);

        let first = tree.find("step 1").expect("the first step");
        let second = tree.find("step 2").expect("the second step");
        assert!(first < second, "{tree}");
    }

    #[test]
    fn the_tree_says_when_the_run_started() {
        let mut trace = trace(vec![envelope(0, AgentEvent::Turn(TurnEvent::Started))]);
        trace.run = Some(TraceRun::Run {
            version: TRACE_FORMAT_VERSION,
            id: "a3f9c2b1d4e7".to_owned(),
            session_id: Some("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678".to_owned()),
            started_at: unix_ms(SystemTime::now() - Duration::from_secs(60 * 60)),
        });

        let tree = tree(&trace, false);

        assert!(
            tree.contains("run a3f9c2b1d4e7 · session 3e4f5678 · started 1 hour ago"),
            "{tree}"
        );
    }

    /// A moment on the wall clock, in Unix milliseconds.
    fn unix_ms(at: SystemTime) -> u64 {
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Where the line that says what is looked for stands.
    fn line_of(lines: &[&str], what: &str) -> usize {
        lines
            .iter()
            .position(|line| line.contains(what))
            .unwrap_or_else(|| panic!("{what} is not in the tree"))
    }

    /// How deep the text of a line sits: one step of the tree per level.
    fn depth(line: &str) -> usize {
        let prefix = line.chars().take_while(|c| " │├└─".contains(*c)).count();
        prefix / 4
    }

    #[test]
    fn the_agent_the_turn_and_the_step_nest() {
        let trace = trace(vec![
            envelope(0, AgentEvent::Turn(TurnEvent::Started)),
            envelope(0, AgentEvent::Turn(TurnEvent::StepStarted { index: 1 })),
            envelope(
                900,
                AgentEvent::Tool(ToolEvent::Started {
                    call_id: ToolCallId(1),
                    name: "file_read".to_owned(),
                    view: ToolView {
                        summary: "Read src/lib.rs".to_owned(),
                        collapse: true,
                        detail: None,
                    },
                }),
            ),
            envelope(
                1_000,
                AgentEvent::Turn(TurnEvent::Completed {
                    usage: Usage::default(),
                    duration_ms: 1_000,
                }),
            ),
        ]);

        let tree = tree(&trace, false);
        let lines: Vec<&str> = tree.lines().collect();
        let agent = line_of(&lines, "agent 1");
        let turn = line_of(&lines, "turn 2");
        let step = line_of(&lines, "step 1");
        let tool = line_of(&lines, "file_read");
        let completed = line_of(&lines, "turn completed");

        assert!(agent < turn && turn < step && step < tool, "{tree}");
        assert_eq!(depth(lines[turn]), depth(lines[agent]) + 1, "{tree}");
        assert_eq!(depth(lines[step]), depth(lines[turn]) + 1, "{tree}");
        assert_eq!(depth(lines[tool]), depth(lines[step]) + 1, "{tree}");
        assert_eq!(depth(lines[completed]), depth(lines[step]), "{tree}");
    }

    #[test]
    fn the_output_of_a_call_sits_under_the_call() {
        let trace = trace(vec![
            envelope(0, AgentEvent::Turn(TurnEvent::StepStarted { index: 1 })),
            envelope(
                10,
                AgentEvent::Tool(ToolEvent::Started {
                    call_id: ToolCallId(1),
                    name: "bash".to_owned(),
                    view: ToolView {
                        summary: "Ran ls".to_owned(),
                        collapse: true,
                        detail: None,
                    },
                }),
            ),
            envelope(
                20,
                AgentEvent::Tool(ToolEvent::OutputDelta {
                    call_id: ToolCallId(1),
                    stream: ToolOutputStream::Stdout,
                    text: "a.txt".to_owned(),
                }),
            ),
        ]);

        let tree = tree(&trace, true);
        let lines: Vec<&str> = tree.lines().collect();
        let call = line_of(&lines, "bash");
        let output = line_of(&lines, "output");

        assert!(call < output, "{tree}");
        assert_eq!(depth(lines[output]), depth(lines[call]) + 1, "{tree}");
    }
}
