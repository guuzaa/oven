use std::time::{Duration, Instant};

use oven_host::{as_ms, now_ms};

use crate::core::event::{AgentEvent, StreamEvent, ToolResult};
use crate::core::identity::ToolCallId;
use crate::core::sink::EventSink;

/// Cap on a tool's output as it enters the conversation, keeping a single
/// huge `file_read`/`bash` result from being carried (and re-encoded on
/// every request) forever.
pub(super) const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct ThinkingSpan {
    started_at: Option<u64>,
    ended_at: Option<u64>,
}

impl ThinkingSpan {
    pub(super) fn note(&mut self) {
        if self.started_at.is_none() {
            self.started_at = Some(now_ms());
        }
    }

    /// Ends the reasoning phase and reports its duration to the transcript, so
    /// the window closes before the answer or tool call that ended it streams.
    /// No-op once closed, or when the phase was too short to time.
    pub(super) fn close(&mut self, sink: &mut impl EventSink) {
        if self.started_at.is_none() || self.ended_at.is_some() {
            return;
        }
        self.ended_at = Some(now_ms());
        if let Some((_, duration_ms)) = self.span() {
            sink.emit(AgentEvent::Stream(StreamEvent::ThinkingDone {
                duration_ms,
            }));
        }
    }

    /// Closes the span, reports it to the transcript and returns it for the
    /// persisted history. Call once per span, after the stream is drained.
    pub(super) fn finish(&mut self, sink: &mut impl EventSink) -> Option<(u64, u64)> {
        self.close(sink);
        self.span()
    }

    pub(super) fn span(&self) -> Option<(u64, u64)> {
        let start = self.started_at?;
        let duration_ms = self.ended_at.unwrap_or(start).saturating_sub(start);
        (duration_ms > 0).then_some((start, duration_ms))
    }
}

/// A whole non-streaming response arrives as one shot: its wall-clock duration
/// is all the thinking window that can be observed. Sub-millisecond durations
/// round up so timed thinking is never mistaken for untimed.
pub(super) fn thinking_span(elapsed: Duration) -> (u64, u64) {
    let duration_ms = as_ms(elapsed).max(1);
    (now_ms().saturating_sub(duration_ms), duration_ms)
}

pub(super) fn log_tool_started(name: &str, call_id: ToolCallId) {
    tracing::info!(name, call_id = call_id.0, "tool started");
}

pub(super) fn log_tool_finished(
    name: &str,
    call_id: ToolCallId,
    result: &ToolResult,
    started: Instant,
) {
    let duration_ms = as_ms(started.elapsed());
    let (ok, error) = match result {
        ToolResult::Success { .. } => (true, None),
        ToolResult::Failed { error, .. } => (false, Some(error.as_str())),
        ToolResult::Rejected { reason } => (false, Some(reason.as_str())),
        ToolResult::Cancelled => (false, Some("cancelled")),
    };
    tracing::info!(
        name,
        call_id = call_id.0,
        ok,
        error,
        duration_ms,
        "tool finished"
    );
}

/// Keeps the head and the tail of an oversized output, dropping only the
/// middle: the opening lines carry context, the closing lines carry the
/// result (exit status, summary, last diff hunk).
pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = s.floor_char_boundary(max / 2);
    let tail_start = s.floor_char_boundary(s.len() - (max - max / 2));
    format!("{}\n...[truncated]\n{}", &s[..head], &s[tail_start..])
}

#[cfg(test)]
mod truncate_tests {
    use super::{MAX_TOOL_OUTPUT_BYTES, truncate};

    const HEAD: &str = "HEAD";
    const TAIL: &str = "TAIL";
    const MARKER: &str = "\n...[truncated]\n";

    #[test]
    fn short_output_is_returned_unchanged() {
        assert_eq!(truncate("hello", MAX_TOOL_OUTPUT_BYTES), "hello");
    }

    #[test]
    fn oversized_output_keeps_head_and_tail() {
        let body = format!(
            "{}{}",
            HEAD.repeat(MAX_TOOL_OUTPUT_BYTES),
            TAIL.repeat(MAX_TOOL_OUTPUT_BYTES)
        );
        let out = truncate(&body, MAX_TOOL_OUTPUT_BYTES);
        assert!(
            out.len() <= MAX_TOOL_OUTPUT_BYTES + MARKER.len(),
            "cap must bound the stored output: {}",
            out.len()
        );
        assert!(out.starts_with(HEAD), "the head must survive: {out:?}");
        assert!(out.ends_with(TAIL), "the tail must survive: {out:?}");
        assert!(out.contains(MARKER));
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let body = "é".repeat(4 * MAX_TOOL_OUTPUT_BYTES);
        let out = truncate(&body, MAX_TOOL_OUTPUT_BYTES - 1);
        assert!(out.contains(MARKER));
        assert!(out.len() <= MAX_TOOL_OUTPUT_BYTES - 1 + MARKER.len());
        assert!(out.starts_with('é'), "must start on a char boundary");
        assert!(out.ends_with('é'), "must end on a char boundary");
    }
}
