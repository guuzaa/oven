//! What a node says: every word of the timeline is chosen here.

use oven_app::StepStop;

use super::format::{brief, headline, span};
use super::model::{Kind, Note, Outcome, StepEnd, ToolCall};

/// The line of a node.
pub fn label(kind: &Kind) -> String {
    match kind {
        Kind::Agent { id } => format!("agent {id}"),
        Kind::Turn { id } => format!("turn {id}"),
        Kind::Step { index, at_ms, end } => step(*index, *at_ms, end.as_ref()),
        Kind::Tool(call) => tool(call),
        Kind::Note(note) => self::note(note),
    }
}

fn step(index: usize, at_ms: Option<u64>, end: Option<&StepEnd>) -> String {
    let started = at_ms
        .map(|ms| format!(" · +{}", span(ms)))
        .unwrap_or_default();
    let Some(end) = end else {
        return format!("step {index}{started}");
    };
    let took = end
        .span_ms
        .map(|ms| format!(" · {}", span(ms)))
        .unwrap_or_default();
    format!(
        "step {index}{started}{took} · {} · provider {}",
        stop_name(&end.stop),
        span(end.provider_ms)
    )
}

fn stop_name(stop: &StepStop) -> &'static str {
    match stop {
        StepStop::FinalAnswer => "final answer",
        StepStop::ToolUse => "tool use",
    }
}

/// A tool call: how it stands, the tool, what it was asked, how it ended.
fn tool(call: &ToolCall) -> String {
    let (mark, tail) = match call.end.as_ref() {
        None => ("→", None),
        Some(end) => match &end.outcome {
            Outcome::Success => ("✓", Some(span(end.duration_ms))),
            Outcome::Failed { error } => (
                "✗",
                Some(format!("{} · {}", span(end.duration_ms), headline(error))),
            ),
            Outcome::Rejected { reason } => ("⊘", Some(format!("rejected · {}", headline(reason)))),
            Outcome::Cancelled => ("⊘", Some("cancelled".to_owned())),
        },
    };
    let mut text = format!("{mark} {}", call.name);
    for part in [Some(headline(&call.summary)), tail].into_iter().flatten() {
        if !part.is_empty() {
            text.push_str(" · ");
            text.push_str(&part);
        }
    }
    text
}

fn note(note: &Note) -> String {
    match note {
        Note::Prompt { text } => format!("› prompt · {}", brief(text)),
        Note::Completed {
            duration_ms,
            input,
            output,
        } => {
            format!(
                "✓ turn completed in {} · {input} in, {output} out",
                span(*duration_ms)
            )
        }
        Note::Cancelled { duration_ms } => {
            format!("⊘ turn cancelled after {}", span(*duration_ms))
        }
        Note::Failed {
            duration_ms,
            message,
        } => {
            format!(
                "✗ turn failed after {} · {}",
                span(*duration_ms),
                brief(message)
            )
        }
        Note::LoopLimit { max_iters } => {
            format!("✗ loop limit reached · {max_iters} iterations")
        }
        Note::Retry {
            retry,
            waited_ms,
            status,
        } => format!(
            "↻ provider retry {retry} · waited {} · status {}",
            span(*waited_ms),
            status.as_deref().unwrap_or("unknown")
        ),
        Note::UserAppended { text } => format!("+ user message appended · {}", brief(text)),
        Note::NoticeAppended { text } => format!("+ notice appended · {}", brief(text)),
        Note::Said { text } => format!("“ {}”", brief(text)),
        Note::Thinking { text } => format!("… thinking · {}", brief(text)),
        Note::ThinkingDone { duration_ms } => {
            format!("… thinking done in {}", span(*duration_ms))
        }
        Note::Todos { items } => format!("☰ todos changed · {items} items"),
        Note::Usage { input, output } => format!("Σ usage · {input} in, {output} out"),
        Note::Approval { name, summary } => {
            format!("? approval asked for {name} · {}", headline(summary))
        }
        Note::Question { text } => format!("? a tool asked · {}", brief(text)),
        Note::Output { text } => format!("▸ output · {}", brief(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::trace::render::model::ToolEnd;

    fn call(summary: &str, end: Option<ToolEnd>) -> Kind {
        Kind::Tool(ToolCall {
            name: "bash".to_owned(),
            summary: summary.to_owned(),
            end,
        })
    }

    fn step(index: usize, at_ms: Option<u64>, end: Option<StepEnd>) -> Kind {
        Kind::Step { index, at_ms, end }
    }

    #[test]
    fn a_step_says_when_it_started_and_how_long_it_took() {
        let end = StepEnd {
            span_ms: Some(1_500),
            stop: StepStop::ToolUse,
            provider_ms: 400,
        };

        assert_eq!(
            label(&step(1, Some(930), Some(end))),
            "step 1 · +930ms · 1.50s · tool use · provider 400ms"
        );
    }

    #[test]
    fn a_step_that_never_ended_says_when_it_started() {
        assert_eq!(label(&step(2, Some(1_000), None)), "step 2 · +1.00s");
    }

    #[test]
    fn a_step_that_never_started_says_nothing_about_when() {
        assert_eq!(label(&step(2, None, None)), "step 2");
    }

    #[test]
    fn what_was_said_is_quoted() {
        let said = Kind::Note(Note::Said {
            text: "the answer".to_owned(),
        });

        assert_eq!(label(&said), "“ the answer”");
    }

    #[test]
    fn the_output_of_a_call_is_marked_as_one() {
        let output = Kind::Note(Note::Output {
            text: "a.txt".to_owned(),
        });

        assert_eq!(label(&output), "▸ output · a.txt");
    }

    #[test]
    fn a_tool_summary_that_holds_lines_stays_one_line() {
        let text = label(&call("Ran git commit\nwith a message", None));

        assert_eq!(text, "→ bash · Ran git commit …");
    }

    #[test]
    fn a_failed_call_says_how_long_it_ran_and_why_it_failed() {
        let end = ToolEnd {
            outcome: Outcome::Failed {
                error: "exit 1".to_owned(),
            },
            duration_ms: 1_200,
        };

        assert_eq!(
            label(&call("ls", Some(end))),
            "✗ bash · ls · 1.20s · exit 1"
        );
    }
}
