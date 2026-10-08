use std::collections::HashMap;

use super::collapsible::{Collapsible, OpenState, Section};
use super::kinds::LineKind;
use super::tools::{TITLE_SEPARATOR, ToolBurst};
use super::wrap::RESULT_LABEL;

/// One run of thinking and tool calls, drawn as a single collapsed row.
///
/// Every collapsing tool call lands in the same [`ToolBurst`], so a later step's
/// `file_edit` adds to the total instead of starting a new "Edited 1 file".
/// Thoughts stay out of that title. The body is the timeline: a thought, then
/// the calls that followed it, then the next thought.
#[derive(Default)]
pub(super) struct Activity {
    burst: ToolBurst,
    segments: Vec<Segment>,
    /// Standalone calls still waiting for a result, keyed by call id.
    pending: HashMap<String, usize>,
    thinking_live: bool,
    /// The next paint should fold unpinned thoughts. Set when a call starts,
    /// applied after the row's open state has been absorbed.
    fold_thoughts: bool,
}

enum Segment {
    Thinking {
        title: String,
        text: String,
        open: OpenState,
    },
    /// Index into [`Activity::burst`], plus the open state of its diff body.
    Call { idx: usize, open: OpenState },
    Standalone {
        kind: LineKind,
        title: String,
        body: String,
        open: OpenState,
    },
    Result {
        ok: bool,
        output: String,
        open: OpenState,
    },
}

impl Activity {
    pub(super) fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub(super) fn thinking_live(&self) -> bool {
        self.thinking_live
    }

    pub(super) fn tools_running(&self) -> bool {
        self.burst.is_running() || !self.pending.is_empty()
    }

    /// Section of the thought still streaming, once thoughts sit beside calls.
    pub(super) fn live_thinking_index(&self) -> Option<usize> {
        if !self.thinking_live || !self.is_timeline() {
            return None;
        }
        self.segments
            .iter()
            .rposition(|segment| matches!(segment, Segment::Thinking { .. }))
    }

    /// Appends to the current thought when one is already open.
    ///
    /// `true` means the timeline did not change, so the row can take the delta
    /// without rebuilding every tool section.
    pub(super) fn push_thinking(&mut self, title: &str, text: &str, live: bool) -> bool {
        if let Some(Segment::Thinking {
            title: current,
            text: body,
            ..
        }) = self.segments.last_mut()
        {
            *current = title.to_string();
            body.push_str(text);
            if live {
                self.thinking_live = true;
            }
            return true;
        }
        self.segments.push(Segment::Thinking {
            title: title.to_string(),
            text: text.to_string(),
            open: OpenState::expanded(),
        });
        self.thinking_live = live;
        false
    }

    /// Settles the live thought. Returns whether one was live.
    pub(super) fn retire_thinking(&mut self, title: &str) -> bool {
        if !self.thinking_live {
            return false;
        }
        self.thinking_live = false;
        if let Some(Segment::Thinking {
            title: current,
            open,
            ..
        }) = self.segments.last_mut()
        {
            *current = title.to_string();
            if !open.pinned {
                open.expanded = false;
            }
        }
        true
    }

    pub(super) fn start_call(&mut self, call_id: String, summary: &str, detail: Option<&str>) {
        self.fold_thoughts = true;
        let idx = self.burst.start(call_id, summary, detail);
        self.segments.push(Segment::Call {
            idx,
            open: OpenState::expanded(),
        });
    }

    pub(super) fn finish_call(
        &mut self,
        call_id: &str,
        detail: Option<&str>,
        failed: bool,
        error: Option<&str>,
    ) -> bool {
        self.burst.finish(call_id, detail, failed, error)
    }

    pub(super) fn start_standalone(
        &mut self,
        call_id: String,
        kind: LineKind,
        title: String,
        body: String,
    ) {
        self.fold_thoughts = true;
        let idx = self.segments.len();
        self.pending.insert(call_id, idx);
        let open = if body.is_empty() {
            OpenState::collapsed()
        } else {
            OpenState::expanded()
        };
        self.segments.push(Segment::Standalone {
            kind,
            title,
            body,
            open,
        });
    }

    /// Records the outcome of a standalone call. `output` is what to show;
    /// `None` means a successful call with nothing to add.
    pub(super) fn finish_standalone(
        &mut self,
        call_id: &str,
        ok: bool,
        output: Option<&str>,
    ) -> bool {
        if self.pending.remove(call_id).is_none() {
            return false;
        }
        if let Some(output) = output {
            self.segments.push(Segment::Result {
                ok,
                output: output.to_string(),
                open: OpenState::expanded(),
            });
        }
        true
    }

    /// Copies each nested item's open state off the row before it is rebuilt.
    pub(super) fn absorb(&mut self, sections: &[Section]) {
        for (segment, section) in self.segments.iter_mut().zip(sections) {
            let Section::Item { detail, .. } = section else {
                continue;
            };
            let shown = detail.open_state();
            match segment {
                Segment::Thinking { open, .. }
                | Segment::Call { open, .. }
                | Segment::Standalone { open, .. }
                | Segment::Result { open, .. } => *open = shown,
            }
        }
    }

    /// Folds unpinned thoughts after [`Self::absorb`], so a call that just
    /// started closes them without dropping a pin.
    pub(super) fn apply_pending_fold(&mut self) {
        if !self.fold_thoughts {
            return;
        }
        self.fold_thoughts = false;
        for segment in &mut self.segments {
            if let Segment::Thinking { open, .. } = segment
                && !open.pinned
            {
                open.expanded = false;
            }
        }
    }

    pub(super) fn project(&self) -> (String, Vec<Section>) {
        if self.is_flat_thinking() {
            let Segment::Thinking { title, text, .. } = &self.segments[0] else {
                return (String::new(), Vec::new());
            };
            return (title.clone(), vec![Section::Text(text.clone())]);
        }
        if self.is_flat_tools() {
            return (self.burst.title(), self.call_sections());
        }
        (self.timeline_title(), self.timeline_sections())
    }

    pub(super) fn is_flat_thinking(&self) -> bool {
        !self.segments.is_empty()
            && self
                .segments
                .iter()
                .all(|segment| matches!(segment, Segment::Thinking { .. }))
    }

    fn is_flat_tools(&self) -> bool {
        !self.segments.is_empty()
            && self
                .segments
                .iter()
                .all(|segment| matches!(segment, Segment::Call { .. }))
    }

    fn is_timeline(&self) -> bool {
        !self.segments.is_empty() && !self.is_flat_thinking() && !self.is_flat_tools()
    }

    /// Tool totals once, at the first call, then each standalone call in place.
    fn timeline_title(&self) -> String {
        let mut parts = Vec::new();
        let mut counted = false;
        for segment in &self.segments {
            match segment {
                Segment::Call { .. } if !counted => {
                    counted = true;
                    let title = self.burst.title();
                    if !title.is_empty() {
                        parts.push(title);
                    }
                }
                Segment::Standalone { title, .. } => parts.push(title.clone()),
                Segment::Thinking { .. } | Segment::Call { .. } | Segment::Result { .. } => {}
            }
        }
        parts.join(TITLE_SEPARATOR)
    }

    fn call_sections(&self) -> Vec<Section> {
        self.segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Call { idx, open } => self
                    .burst
                    .section_at(*idx)
                    .map(|section| section.with_open(*open)),
                _ => None,
            })
            .collect()
    }

    fn timeline_sections(&self) -> Vec<Section> {
        self.segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Thinking { title, text, open } => Some(Section::Item {
                    kind: LineKind::Thinking,
                    title: title.clone(),
                    detail: Collapsible::new(text.clone()).with_open_state(*open),
                }),
                Segment::Call { idx, open } => self
                    .burst
                    .section_at(*idx)
                    .map(|section| section.with_open(*open)),
                Segment::Standalone {
                    kind,
                    title,
                    body,
                    open,
                } => Some(Section::Item {
                    kind: *kind,
                    title: title.clone(),
                    detail: Collapsible::new(body.clone()).with_open_state(*open),
                }),
                Segment::Result { ok, output, open } => Some(Section::Item {
                    kind: LineKind::ToolResult(*ok),
                    title: RESULT_LABEL.to_string(),
                    detail: Collapsible::new(output.clone()).with_open_state(*open),
                }),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Activity;

    #[test]
    fn later_edits_add_to_the_same_total() {
        let mut activity = Activity::default();
        activity.push_thinking("Thought for 1s", "look", false);
        activity.start_call("1".into(), "Edited src/a.rs", Some("- old\n+ new"));
        activity.push_thinking("Thought for 1s", "again", false);
        activity.start_call("2".into(), "Edited src/b.rs", Some("- x\n+ y"));

        assert_eq!(activity.project().0, "Edited 2 files");
    }

    #[test]
    fn a_thought_between_calls_stays_out_of_the_title() {
        let mut activity = Activity::default();
        activity.start_call("1".into(), "Ran ls", None);
        activity.push_thinking("Thought for 1.5s", "hmm", false);
        activity.start_call("2".into(), "Ran pwd", None);

        let (title, sections) = activity.project();
        assert_eq!(title, "Ran 2 commands");
        assert_eq!(sections.len(), 3);
    }
}
