//! How a subagent's result reads to the agent that delegated to it: as the
//! `task` result it waited for, or as the notice it is told when it did not
//! wait. Both quote the same tally and the same clamped report.
//!
//! The error is the one thing the two paths place differently. A `task`
//! result is read whole, so its error sits on the line under the tally. A
//! notice is read by its first line alone — the frontend draws that line and
//! nothing else — so its error rides there instead, and is cut to one line:
//! an error that runs off the row is no line at all.

use std::fmt::Write;

use oven_proto::{NodeInfo, NodeStatus};

/// A subagent's report reaches the caller's context like any tool result, so
/// it is capped first.
pub(crate) const MAX_REPORT_BYTES: usize = 16 * 1024;
pub(crate) const TRUNCATION_MARK: &str = "\n...[truncated]";

/// How much of an error a line holds. Whatever the error says, the line it
/// sits on stays readable.
const MAX_ERROR_CHARS: usize = 200;

/// What a finished subagent nobody was waiting on tells the turn. The first
/// line is all a frontend draws, so a failure names its reason there.
pub(crate) fn render_notice(info: &NodeInfo, report: &str, now_ms: u64) -> String {
    let head = format!("{}: {}", info.name, info.progress(now_ms));
    match &info.status {
        NodeStatus::Failed { error } => {
            let error = one_line(error);
            if error.is_empty() {
                head
            } else {
                format!("{head} — {error}")
            }
        }
        status => with_report(head, status, report),
    }
}

/// `head`, then what the subagent left: its error, or its clamped report.
pub(crate) fn with_report(mut text: String, status: &NodeStatus, report: &str) -> String {
    match status {
        NodeStatus::Failed { error } => {
            let error = one_line(error);
            if !error.is_empty() {
                let _ = write!(text, "\n{error}");
            }
        }
        _ => {
            let report = report.trim();
            if !report.is_empty() {
                let _ = write!(text, "\n\n{}", clamp(report, MAX_REPORT_BYTES));
            }
        }
    }
    text
}

/// `text` as one line: its whitespace collapsed, and cut to
/// [`MAX_ERROR_CHARS`] with a mark that says it was cut.
fn one_line(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_ERROR_CHARS {
        return text;
    }
    let head: String = text.chars().take(MAX_ERROR_CHARS).collect();
    format!("{head}…")
}

pub(crate) fn clamp(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    format!(
        "{}{TRUNCATION_MARK}",
        &text[..text.floor_char_boundary(max)]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_llm::Usage;
    use oven_proto::AgentId;

    const REPORT: &str = "found it in runtime/mod.rs";
    const ERROR: &str = "provider exploded";

    fn info(status: NodeStatus) -> NodeInfo {
        NodeInfo {
            id: AgentId::next(),
            name: "explore#1".into(),
            role: "explore".into(),
            label: "find it".into(),
            background: true,
            parent: AgentId::next(),
            status,
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 40,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            },
            tool_calls: 3,
            steps: 4,
            started_at: 0,
            finished_at: Some(1_500),
        }
    }

    #[test]
    fn a_notice_leads_with_its_tally_and_carries_the_report() {
        let text = render_notice(&info(NodeStatus::Completed), REPORT, 0);
        assert_eq!(
            text.lines().next(),
            Some("explore#1: done · 4 steps, 3 tool calls · 1040 tokens · 1.5s")
        );
        assert!(text.ends_with(REPORT), "{text}");
    }

    #[test]
    fn a_failed_notice_names_its_reason_on_the_line_a_frontend_draws() {
        let failed = info(NodeStatus::Failed {
            error: ERROR.into(),
        });
        let text = render_notice(&failed, "", 0);
        assert_eq!(
            text,
            format!("explore#1: failed · 4 steps, 3 tool calls · 1040 tokens · 1.5s — {ERROR}")
        );
    }

    #[test]
    fn an_error_is_one_line_whatever_it_says() {
        let failed = info(NodeStatus::Failed {
            error: "the proxy hung up\nand said nothing".into(),
        });
        let text = render_notice(&failed, "", 0);
        assert_eq!(
            text,
            "explore#1: failed · 4 steps, 3 tool calls · 1040 tokens · 1.5s — the proxy hung up and said nothing"
        );
        assert!(
            !text.contains('\n'),
            "the line a frontend draws is one line"
        );
    }

    #[test]
    fn a_very_long_error_is_cut_short() {
        let failed = info(NodeStatus::Failed {
            error: "x".repeat(MAX_ERROR_CHARS * 2),
        });
        let text = render_notice(&failed, "", 0);
        let (_, error) = text.split_once(" — ").expect("the error is on the line");
        assert_eq!(error.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(error.ends_with('…'), "the cut says so");
    }

    #[test]
    fn an_error_with_nothing_to_say_leaves_the_line_alone() {
        let failed = info(NodeStatus::Failed { error: "  ".into() });
        let text = render_notice(&failed, "", 0);
        assert_eq!(
            text,
            "explore#1: failed · 4 steps, 3 tool calls · 1040 tokens · 1.5s"
        );
    }

    #[test]
    fn a_long_report_is_clamped_on_a_char_boundary() {
        let text = clamp(&"é".repeat(MAX_REPORT_BYTES), MAX_REPORT_BYTES);
        assert!(text.ends_with(TRUNCATION_MARK));
        assert!(text.len() <= MAX_REPORT_BYTES + TRUNCATION_MARK.len());
    }
}
