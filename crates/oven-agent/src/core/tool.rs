//! The protocol one callable capability speaks.
//!
//! The protocol lives here, the capabilities live in the app layer: this
//! crate drives turns and never names a tool it was not handed.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::turn::TurnContext;
use crate::core::view::{ToolCaps, ToolEffect, ToolOutcome};
use oven_proto::AgentError;
use oven_proto::ToolView;

/// Close enough to a name to be offered as what it meant.
const CLOSE_ENOUGH: f64 = 0.85;

/// One capability the model can call. `run` receives the whole
/// [`TurnContext`] rather than a bag of options, so a tool that starts work
/// of its own — a subagent, a nested loop — inherits the run it was called
/// from instead of guessing.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// The tool's JSON Schema. Shared, so a caller that keeps it does not
    /// copy the schema tree, and a tool that never changes it can hand out
    /// the same one every time.
    fn schema(&self) -> Arc<Value>;
    /// How this call reads in a frontend. A tool that knows better than its
    /// own name overrides it.
    fn view(&self, _input: &Value) -> ToolView {
        ToolView::named(self.name())
    }
    /// How this call reads before it runs. A call that waits for the user's
    /// approval names itself in the future tense: `run ls -la`, not
    /// `Ran ls -la`.
    fn request_view(&self, input: &Value) -> ToolView {
        self.view(input)
    }
    fn caps(&self) -> ToolCaps {
        ToolCaps::default()
    }
    /// What this call leaves behind for the driver to record, e.g. a
    /// checklist the model rewrote. `None` for a call that leaves nothing.
    fn effect(&self, _input: &Value) -> Option<ToolEffect> {
        None
    }
    async fn run(&self, args: &Value, cx: &TurnContext) -> Result<ToolOutcome, AgentError>;
}

/// The mounted tool a name most likely meant.
///
/// A model that asks for `file_writes` meant `file_write`: naming the tool
/// that exists turns a dead call into a retry. `None` when nothing close is
/// mounted, which is better than pointing at a tool the model did not mean.
///
/// The closest name wins. Among names that are equally close, the one that
/// says least beyond what the model wrote wins, then the shorter name, then
/// the first name.
pub fn suggest<'a>(wanted: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let wanted = wanted.to_lowercase();
    let wanted_words = words(&wanted);
    if wanted_words.is_empty() {
        return None;
    }
    candidates
        .filter_map(|candidate| {
            let score = score(&wanted, &wanted_words, candidate)?;
            Some((score, candidate))
        })
        .max_by(|left, right| {
            left.0
                .closeness
                .total_cmp(&right.0.closeness)
                .then_with(|| right.0.extra.cmp(&left.0.extra))
                .then_with(|| right.1.len().cmp(&left.1.len()))
                .then_with(|| right.1.cmp(left.1))
        })
        .map(|(_, candidate)| candidate)
}

/// How much a mounted tool looks like what the model meant.
struct Score {
    /// How alike the two names are.
    closeness: f64,
    /// How many words the tool's name says beyond what the model wrote.
    extra: usize,
}

/// How likely `candidate` is what `wanted` meant.
///
/// The tool's own name, the tool's words in another order, and a name that
/// holds the wanted words are all the tool the model meant. Any other name
/// has to look like it.
fn score(wanted: &str, wanted_words: &[String], candidate: &str) -> Option<Score> {
    let lower = candidate.to_lowercase();
    let candidate_words = words(&lower);
    let extra = candidate_words.len().saturating_sub(wanted_words.len());
    let closeness = closeness(wanted, &lower, &candidate_words);
    let alike = lower == wanted
        || wanted_words == candidate_words
        || covers(wanted_words, &candidate_words);
    if !alike && closeness < CLOSE_ENOUGH {
        return None;
    }
    Some(Score { closeness, extra })
}

/// How alike two names are: as whole names, and word by word, so a name that
/// lost its separator, or misspelled a word, still finds its tool.
fn closeness(wanted: &str, candidate: &str, candidate_words: &[String]) -> f64 {
    let whole = strsim::jaro_winkler(wanted, candidate);
    candidate_words
        .iter()
        .map(|word| strsim::jaro_winkler(wanted, word))
        .fold(whole, f64::max)
}

/// The words in a name, sorted, so `read_file` and `file_read` are the same
/// words.
fn words(name: &str) -> Vec<String> {
    let mut words: Vec<String> = name
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    words.sort();
    words
}

/// Whether every wanted word is also a word of the candidate.
fn covers(wanted: &[String], candidate: &[String]) -> bool {
    let mut rest = candidate.iter();
    wanted.iter().all(|word| rest.any(|other| other == word))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: &[&str] = &[
        "answer",
        "bash",
        "file_edit",
        "file_read",
        "file_write",
        "glob",
        "grep",
        "list_models",
        "memory_forget",
        "memory_read",
        "memory_write",
        "skill_read",
        "task",
        "task_output",
        "todo_write",
        "web_fetch",
    ];

    #[test]
    fn a_name_in_another_case_points_at_the_tool() {
        assert_eq!(
            suggest("File_Read", TOOLS.iter().copied()),
            Some("file_read")
        );
    }

    #[test]
    fn the_same_words_in_another_order_point_at_the_tool() {
        assert_eq!(
            suggest("read_file", TOOLS.iter().copied()),
            Some("file_read")
        );
    }

    #[test]
    fn a_word_the_tool_carries_points_at_the_tool() {
        assert_eq!(suggest("write", TOOLS.iter().copied()), Some("file_write"));
    }

    #[test]
    fn the_closest_tool_wins() {
        assert_eq!(
            suggest("file_writes", TOOLS.iter().copied()),
            Some("file_write")
        );
    }

    #[test]
    fn a_word_misspelled_still_points_at_the_tool() {
        assert_eq!(suggest("wirte", TOOLS.iter().copied()), Some("file_write"));
    }

    #[test]
    fn a_name_that_lost_its_separator_still_points_at_the_tool() {
        assert_eq!(
            suggest("readfiles", TOOLS.iter().copied()),
            Some("file_read")
        );
    }

    #[test]
    fn a_tie_falls_to_the_first_name() {
        assert_eq!(suggest("file", TOOLS.iter().copied()), Some("file_edit"));
    }

    #[test]
    fn a_tie_does_not_depend_on_the_order_the_tools_are_mounted_in() {
        const REVERSED: &[&str] = &[
            "web_fetch",
            "todo_write",
            "task_output",
            "task",
            "skill_read",
            "memory_write",
            "memory_read",
            "memory_forget",
            "list_models",
            "grep",
            "glob",
            "file_write",
            "file_read",
            "file_edit",
            "bash",
            "answer",
        ];
        assert_eq!(suggest("file", REVERSED.iter().copied()), Some("file_edit"));
    }

    #[test]
    fn a_name_nothing_resembles_is_left_alone() {
        assert_eq!(suggest("browser", TOOLS.iter().copied()), None);
    }

    #[test]
    fn a_name_without_a_word_points_at_nothing() {
        assert_eq!(suggest("", TOOLS.iter().copied()), None);
        assert_eq!(suggest("___", TOOLS.iter().copied()), None);
    }
}
