//! What a run did, as a tree: the agents, the turns each took, the steps of
//! each turn, and what happened inside them. Nothing here is text for a
//! reader: the words are chosen in `label`.

use std::collections::HashMap;

use oven_app::trace::Trace;
use oven_app::{
    AgentEvent, AgentEventEnvelope, StepStop, StreamEvent, ToolEvent, ToolResult, TurnEvent,
};

/// One node of the run, and what happened under it.
pub struct Node {
    pub kind: Kind,
    pub children: Vec<Node>,
}

pub enum Kind {
    Agent {
        id: u64,
    },
    Turn {
        id: u64,
    },
    Step {
        index: usize,
        /// When the step started, counted from the first event of the run.
        /// Unknown when the trace holds its end but not its start.
        at_ms: Option<u64>,
        end: Option<StepEnd>,
    },
    Tool(ToolCall),
    Note(Note),
}

/// How a step ended.
pub struct StepEnd {
    /// How long the step lasted. Unknown when the trace holds its end but
    /// not its start.
    pub span_ms: Option<u64>,
    pub stop: StepStop,
    pub provider_ms: u64,
}

/// A tool call: what it was asked, and how it ended once it has.
pub struct ToolCall {
    pub name: String,
    pub summary: String,
    pub end: Option<ToolEnd>,
}

pub struct ToolEnd {
    pub outcome: Outcome,
    pub duration_ms: u64,
}

pub enum Outcome {
    Success,
    Failed { error: String },
    Rejected { reason: String },
    Cancelled,
}

/// Something that happened, with the words it carried.
pub enum Note {
    Prompt {
        text: String,
    },
    Completed {
        duration_ms: u64,
        input: u64,
        output: u64,
    },
    Cancelled {
        duration_ms: u64,
    },
    Failed {
        duration_ms: u64,
        message: String,
    },
    LoopLimit {
        max_iters: usize,
    },
    Retry {
        retry: u32,
        waited_ms: u64,
        status: Option<String>,
    },
    UserAppended {
        text: String,
    },
    NoticeAppended {
        text: String,
    },
    Said {
        text: String,
    },
    Thinking {
        text: String,
    },
    ThinkingDone {
        duration_ms: u64,
    },
    Todos {
        items: usize,
    },
    Usage {
        input: u64,
        output: u64,
    },
    Approval {
        name: String,
        summary: String,
    },
    Question {
        text: String,
    },
    Output {
        text: String,
    },
    /// A reply the provider sent that cannot end the turn: it carried no
    /// answer, or it was cut off. It says what the provider sent.
    Rejected {
        message: String,
        stop_reason: Option<String>,
        blocks: usize,
        output_tokens: Option<u64>,
        provider_ms: u64,
    },
}

/// Which kind of words a note holds.
#[derive(Debug, Clone, Copy)]
enum Words {
    Said,
    Thinking,
    Output,
}

impl Words {
    /// The note that holds these words.
    fn note(self, text: &str) -> Note {
        match self {
            Words::Said => Note::Said {
                text: text.to_owned(),
            },
            Words::Thinking => Note::Thinking {
                text: text.to_owned(),
            },
            Words::Output => Note::Output {
                text: text.to_owned(),
            },
        }
    }

    /// The words the note holds, when it holds that kind of words.
    fn held_by(self, note: &mut Note) -> Option<&mut String> {
        match (self, note) {
            (Words::Said, Note::Said { text }) | (Words::Thinking, Note::Thinking { text }) => {
                Some(text)
            }
            (Words::Output, Note::Output { text }) => Some(text),
            _ => None,
        }
    }
}

/// The run as a forest, one root per agent. The events are put back in the
/// order they happened first: an event the agent reports late can carry an
/// earlier moment.
pub fn build(trace: &Trace, with_text: bool) -> Vec<Node> {
    let mut events: Vec<&AgentEventEnvelope> = trace.events.iter().collect();
    events.sort_by_key(|event| event.elapsed_ms);
    let mut builder = Builder::new(with_text);
    for event in events {
        let at_ms = event.elapsed_ms.saturating_sub(trace.base_elapsed_ms);
        builder.add(event, at_ms);
    }
    builder.finish()
}

/// A node while the tree is still growing: its children are slots.
struct Slot {
    kind: Kind,
    children: Vec<usize>,
}

/// A turn that is open: its node, the steps it took, and the step of it
/// that is open.
struct TurnState {
    node: usize,
    steps: HashMap<usize, OpenStep>,
    current: Option<usize>,
}

/// A step of a turn: the node it is drawn as, and the moment it started.
struct OpenStep {
    node: usize,
    at_ms: u64,
}

/// The agent and the turn an event belongs to.
type TurnKey = (u64, u64);

struct Builder {
    with_text: bool,
    slots: Vec<Option<Slot>>,
    roots: Vec<usize>,
    agents: HashMap<u64, usize>,
    turns: HashMap<TurnKey, TurnState>,
    /// The tool calls that wait for their end, by agent and call id.
    calls: HashMap<(u64, u64), usize>,
}

impl Builder {
    fn new(with_text: bool) -> Self {
        Self {
            with_text,
            slots: Vec::new(),
            roots: Vec::new(),
            agents: HashMap::new(),
            turns: HashMap::new(),
            calls: HashMap::new(),
        }
    }

    fn slot(&mut self, at: usize) -> &mut Slot {
        self.slots[at]
            .as_mut()
            .expect("a slot is taken only at the end")
    }

    /// Add a node under `parent`, or as a root.
    fn push(&mut self, parent: Option<usize>, kind: Kind) -> usize {
        let at = self.slots.len();
        self.slots.push(Some(Slot {
            kind,
            children: Vec::new(),
        }));
        match parent {
            Some(parent) => self.slot(parent).children.push(at),
            None => self.roots.push(at),
        }
        at
    }

    /// The agent and the turn of an event, opened the first time they show.
    fn open(&mut self, agent: u64, turn: u64) -> TurnKey {
        let agent_node = match self.agents.get(&agent) {
            Some(&node) => node,
            None => {
                let node = self.push(None, Kind::Agent { id: agent });
                self.agents.insert(agent, node);
                node
            }
        };
        if !self.turns.contains_key(&(agent, turn)) {
            let node = self.push(Some(agent_node), Kind::Turn { id: turn });
            self.turns.insert(
                (agent, turn),
                TurnState {
                    node,
                    steps: HashMap::new(),
                    current: None,
                },
            );
        }
        (agent, turn)
    }

    /// Where something that happened inside the turn goes: the step that is
    /// open, or the turn when no step is.
    fn inside(&self, key: TurnKey) -> usize {
        let turn = &self.turns[&key];
        match turn.current {
            Some(index) => turn.steps[&index].node,
            None => turn.node,
        }
    }

    fn note_in_turn(&mut self, key: TurnKey, note: Note) {
        let node = self.turns[&key].node;
        self.push(Some(node), Kind::Note(note));
    }

    fn note_inside(&mut self, key: TurnKey, note: Note) {
        let parent = self.inside(key);
        self.push(Some(parent), Kind::Note(note));
    }

    /// Puts what was said under the turn it was said in.
    ///
    /// The words of a turn arrive in pieces, and a reader wants the
    /// sentence rather than the pieces: two deltas that follow each other
    /// are one line, not two.
    fn say(&mut self, key: TurnKey, text: &str, words: Words) {
        let parent = self.inside(key);
        self.say_under(parent, text, words);
    }

    /// Puts what was said under `parent`, into the note that is already
    /// there when it holds the same kind of words.
    fn say_under(&mut self, parent: usize, text: &str, words: Words) {
        let last = self
            .slots
            .get(parent)
            .and_then(|slot| slot.as_ref())
            .and_then(|slot| slot.children.last())
            .copied();
        if let Some(node) = last
            && let Kind::Note(note) = &mut self.slot(node).kind
            && let Some(held) = words.held_by(note)
        {
            held.push_str(text);
            return;
        }
        self.push(Some(parent), Kind::Note(words.note(text)));
    }

    fn add(&mut self, event: &AgentEventEnvelope, at_ms: u64) {
        let key = self.open(event.agent_id.0, event.turn_id.0);
        match &event.event {
            AgentEvent::Turn(turn) => self.turn_event(key, turn, at_ms),
            AgentEvent::Stream(stream) => self.stream_event(key, stream),
            AgentEvent::Tool(tool) => self.tool_event(key, tool),
            AgentEvent::TodosChanged { todos } => {
                self.note_inside(
                    key,
                    Note::Todos {
                        items: todos.items.len(),
                    },
                );
            }
            AgentEvent::Usage { usage } => self.note_inside(
                key,
                Note::Usage {
                    input: usage.input_tokens as u64,
                    output: usage.output_tokens as u64,
                },
            ),
        }
    }

    fn turn_event(&mut self, key: TurnKey, turn: &TurnEvent, at_ms: u64) {
        match turn {
            TurnEvent::Started => {}
            TurnEvent::StepStarted { index } => {
                let node = self.turns[&key].node;
                let step = self.push(
                    Some(node),
                    Kind::Step {
                        index: *index,
                        at_ms: Some(at_ms),
                        end: None,
                    },
                );
                let turn = self.turns.get_mut(&key).expect("opened");
                turn.steps.insert(*index, OpenStep { node: step, at_ms });
                turn.current = Some(*index);
            }
            TurnEvent::StepFinished {
                index,
                stop,
                provider_ms,
            } => {
                let open = self
                    .turns
                    .get_mut(&key)
                    .expect("opened")
                    .steps
                    .remove(index);
                let end = |span_ms| StepEnd {
                    span_ms,
                    stop: *stop,
                    provider_ms: *provider_ms,
                };
                match open {
                    Some(step) => {
                        let span = Some(at_ms.saturating_sub(step.at_ms));
                        if let Kind::Step { end: slot, .. } = &mut self.slot(step.node).kind {
                            *slot = Some(end(span));
                        }
                        let turn = self.turns.get_mut(&key).expect("opened");
                        if turn.current == Some(*index) {
                            turn.current = None;
                        }
                    }
                    None => {
                        let node = self.turns[&key].node;
                        self.push(
                            Some(node),
                            Kind::Step {
                                index: *index,
                                at_ms: None,
                                end: Some(end(None)),
                            },
                        );
                    }
                }
            }
            TurnEvent::Prompt { text } => {
                self.note_in_turn(key, Note::Prompt { text: text.clone() });
            }
            TurnEvent::ProviderRetried {
                retry,
                waited_ms,
                status,
            } => self.note_inside(
                key,
                Note::Retry {
                    retry: *retry,
                    waited_ms: *waited_ms,
                    status: status.map(|code| code.to_string()),
                },
            ),
            TurnEvent::UserAppended { text } => {
                self.note_inside(key, Note::UserAppended { text: text.clone() });
            }
            TurnEvent::StepRejected {
                error,
                stop_reason,
                blocks,
                usage,
                provider_ms,
                ..
            } => self.note_inside(
                key,
                Note::Rejected {
                    message: error.message.clone(),
                    stop_reason: stop_reason.map(|reason| reason.to_string()),
                    blocks: *blocks,
                    output_tokens: usage.map(|usage| usage.output_tokens as u64),
                    provider_ms: *provider_ms,
                },
            ),
            TurnEvent::NoticeAppended { text } => {
                self.note_inside(key, Note::NoticeAppended { text: text.clone() });
            }
            TurnEvent::Completed { usage, duration_ms } => self.note_in_turn(
                key,
                Note::Completed {
                    duration_ms: *duration_ms,
                    input: usage.input_tokens as u64,
                    output: usage.output_tokens as u64,
                },
            ),
            TurnEvent::Cancelled { duration_ms } => {
                self.note_in_turn(
                    key,
                    Note::Cancelled {
                        duration_ms: *duration_ms,
                    },
                );
            }
            TurnEvent::Failed { error, duration_ms } => self.note_in_turn(
                key,
                Note::Failed {
                    duration_ms: *duration_ms,
                    message: error.message.clone(),
                },
            ),
            TurnEvent::LoopLimitReached { max_iters, .. } => {
                self.note_in_turn(
                    key,
                    Note::LoopLimit {
                        max_iters: *max_iters,
                    },
                );
            }
        }
    }

    fn stream_event(&mut self, key: TurnKey, stream: &StreamEvent) {
        match stream {
            StreamEvent::TextDelta { text } if self.with_text => {
                self.say(key, text, Words::Said);
            }
            StreamEvent::ThinkingDelta { text } if self.with_text => {
                self.say(key, text, Words::Thinking);
            }
            StreamEvent::ThinkingDone { duration_ms } => {
                self.note_inside(
                    key,
                    Note::ThinkingDone {
                        duration_ms: *duration_ms,
                    },
                );
            }
            _ => {}
        }
    }

    fn tool_event(&mut self, key: TurnKey, tool: &ToolEvent) {
        match tool {
            ToolEvent::ApprovalRequested { name, view, .. } => self.note_inside(
                key,
                Note::Approval {
                    name: name.clone(),
                    summary: view.summary.clone(),
                },
            ),
            ToolEvent::Started {
                call_id,
                name,
                view,
            } => {
                let parent = self.inside(key);
                let call = ToolCall {
                    name: name.clone(),
                    summary: view.summary.clone(),
                    end: None,
                };
                let node = self.push(Some(parent), Kind::Tool(call));
                self.calls.insert((key.0, call_id.0), node);
            }
            ToolEvent::QuestionAsked { question, .. } => {
                self.note_inside(
                    key,
                    Note::Question {
                        text: question.question.clone(),
                    },
                );
            }
            ToolEvent::OutputDelta { call_id, text, .. } if self.with_text => {
                let parent = self
                    .calls
                    .get(&(key.0, call_id.0))
                    .copied()
                    .unwrap_or_else(|| self.inside(key));
                self.say_under(parent, text, Words::Output);
            }
            ToolEvent::OutputDelta { .. } => {}
            ToolEvent::Finished {
                call_id,
                name,
                result,
                duration_ms,
                ..
            } => {
                let end = ToolEnd {
                    outcome: outcome_of(result),
                    duration_ms: *duration_ms,
                };
                match self.calls.remove(&(key.0, call_id.0)) {
                    Some(node) => {
                        if let Kind::Tool(call) = &mut self.slot(node).kind {
                            call.end = Some(end);
                        }
                    }
                    None => {
                        let parent = self.inside(key);
                        let call = ToolCall {
                            name: name.clone(),
                            summary: String::new(),
                            end: Some(end),
                        };
                        self.push(Some(parent), Kind::Tool(call));
                    }
                }
            }
        }
    }

    /// Turn the slots into the nodes they stand for.
    fn finish(mut self) -> Vec<Node> {
        let roots = std::mem::take(&mut self.roots);
        roots.into_iter().map(|root| self.take(root)).collect()
    }

    fn take(&mut self, at: usize) -> Node {
        let slot = self.slots[at].take().expect("a node has one parent");
        let children = slot
            .children
            .into_iter()
            .map(|child| self.take(child))
            .collect();
        Node {
            kind: slot.kind,
            children,
        }
    }
}

fn outcome_of(result: &ToolResult) -> Outcome {
    match result {
        ToolResult::Success { .. } => Outcome::Success,
        ToolResult::Failed { error, .. } => Outcome::Failed {
            error: error.clone(),
        },
        ToolResult::Rejected { reason } => Outcome::Rejected {
            reason: reason.clone(),
        },
        ToolResult::Cancelled => Outcome::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use oven_app::{AgentError, AgentId, ToolCallId, ToolOutputStream, ToolView, TurnId};
    use oven_llm::{StopReason, Usage};

    use super::*;

    fn at(agent: u64, turn: u64, elapsed_ms: u64, event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            agent_id: AgentId(agent),
            turn_id: TurnId(turn),
            at: 0,
            elapsed_ms,
            event,
        }
    }

    fn trace(events: Vec<AgentEventEnvelope>) -> Trace {
        Trace {
            run: None,
            events,
            damaged: 0,
            base_elapsed_ms: 0,
        }
    }

    fn started(call: u64) -> AgentEvent {
        AgentEvent::Tool(ToolEvent::Started {
            call_id: ToolCallId(call),
            name: "bash".to_owned(),
            view: ToolView {
                summary: "ls".to_owned(),
                collapse: true,
                detail: None,
            },
        })
    }

    fn finished(call: u64) -> AgentEvent {
        AgentEvent::Tool(ToolEvent::Finished {
            call_id: ToolCallId(call),
            name: "bash".to_owned(),
            result: ToolResult::Success {
                output: String::new(),
            },
            detail: None,
            duration_ms: 12,
        })
    }

    #[test]
    fn a_tool_call_is_one_node_that_holds_its_end() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(1, 1, 5, started(1)),
                at(1, 1, 9, finished(1)),
            ]),
            false,
        );

        let step = &forest[0].children[0].children[0];
        assert_eq!(step.children.len(), 1);
        let Kind::Tool(call) = &step.children[0].kind else {
            panic!("a tool call");
        };
        assert_eq!(call.end.as_ref().map(|end| end.duration_ms), Some(12));
    }

    #[test]
    fn a_step_knows_how_long_it_lasted() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    100,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(
                    1,
                    1,
                    700,
                    AgentEvent::Turn(TurnEvent::StepFinished {
                        index: 1,
                        stop: StepStop::ToolUse,
                        provider_ms: 300,
                    }),
                ),
            ]),
            false,
        );

        let Kind::Step { end: Some(end), .. } = &forest[0].children[0].children[0].kind else {
            panic!("a finished step");
        };
        assert_eq!(end.span_ms, Some(600));
    }

    #[test]
    fn a_step_knows_when_it_started() {
        let forest = build(
            &trace(vec![at(
                1,
                1,
                500,
                AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
            )]),
            false,
        );

        let Kind::Step { at_ms, .. } = &forest[0].children[0].children[0].kind else {
            panic!("a step");
        };
        assert_eq!(*at_ms, Some(500));
    }

    #[test]
    fn a_step_ends_by_the_index_it_was_started_with() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(
                    1,
                    1,
                    10,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 2 }),
                ),
                at(
                    1,
                    1,
                    30,
                    AgentEvent::Turn(TurnEvent::StepFinished {
                        index: 1,
                        stop: StepStop::FinalAnswer,
                        provider_ms: 100,
                    }),
                ),
                at(
                    1,
                    1,
                    50,
                    AgentEvent::Turn(TurnEvent::StepFinished {
                        index: 2,
                        stop: StepStop::ToolUse,
                        provider_ms: 200,
                    }),
                ),
            ]),
            false,
        );

        let turn = &forest[0].children[0];
        assert_eq!(turn.children.len(), 2);
        let Kind::Step {
            index: first_index,
            end: Some(first),
            ..
        } = &turn.children[0].kind
        else {
            panic!("the first step");
        };
        let Kind::Step {
            index: second_index,
            end: Some(second),
            ..
        } = &turn.children[1].kind
        else {
            panic!("the second step");
        };
        assert_eq!(*first_index, 1);
        assert_eq!(first.stop, StepStop::FinalAnswer);
        assert_eq!(first.span_ms, Some(30));
        assert_eq!(*second_index, 2);
        assert_eq!(second.stop, StepStop::ToolUse);
        assert_eq!(second.span_ms, Some(40));
    }

    #[test]
    fn a_step_that_never_started_says_nothing_about_when_it_began() {
        let forest = build(
            &trace(vec![at(
                1,
                1,
                20,
                AgentEvent::Turn(TurnEvent::StepFinished {
                    index: 2,
                    stop: StepStop::ToolUse,
                    provider_ms: 100,
                }),
            )]),
            false,
        );

        let Kind::Step {
            at_ms,
            end: Some(end),
            ..
        } = &forest[0].children[0].children[0].kind
        else {
            panic!("a step without a start");
        };
        assert_eq!(*at_ms, None);
        assert_eq!(end.span_ms, None);
    }

    #[test]
    fn the_output_of_a_call_sits_under_the_call() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(1, 1, 5, started(1)),
                at(
                    1,
                    1,
                    9,
                    AgentEvent::Tool(ToolEvent::OutputDelta {
                        call_id: ToolCallId(1),
                        stream: ToolOutputStream::Stdout,
                        text: "a.txt".to_owned(),
                    }),
                ),
            ]),
            true,
        );

        let step = &forest[0].children[0].children[0];
        let Kind::Tool(_) = &step.children[0].kind else {
            panic!("a tool call");
        };
        assert_eq!(step.children[0].children.len(), 1);
    }

    #[test]
    fn a_rejected_reply_sits_under_the_step_it_rejected() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(
                    1,
                    1,
                    410,
                    AgentEvent::Turn(TurnEvent::StepRejected {
                        index: 1,
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
                    }),
                ),
            ]),
            false,
        );

        let step = &forest[0].children[0].children[0];
        let Kind::Note(Note::Rejected {
            message,
            stop_reason,
            blocks,
            output_tokens,
            ..
        }) = &step.children[0].kind
        else {
            panic!("the rejection sits under its step");
        };
        assert_eq!(message, "the model returned no answer");
        assert_eq!(stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(*blocks, 1);
        assert_eq!(*output_tokens, Some(653));
    }

    #[test]
    fn an_agent_that_comes_back_is_the_same_node() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::Prompt { text: "a".into() }),
                ),
                at(
                    2,
                    1,
                    1,
                    AgentEvent::Turn(TurnEvent::Prompt { text: "b".into() }),
                ),
                at(
                    1,
                    1,
                    2,
                    AgentEvent::Turn(TurnEvent::Prompt { text: "c".into() }),
                ),
            ]),
            false,
        );

        assert_eq!(forest.len(), 2);
        assert_eq!(forest[0].children[0].children.len(), 2);
    }

    /// The words of a turn, as they arrived.
    fn said(text: &str) -> AgentEvent {
        AgentEvent::Stream(StreamEvent::TextDelta {
            text: text.to_owned(),
        })
    }

    #[test]
    fn what_was_said_is_one_line_and_not_one_line_per_piece() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(1, 1, 5, said("It has a ")),
                at(1, 1, 10, said("main function.")),
            ]),
            true,
        );

        let step = &forest[0].children[0].children[0];
        assert_eq!(step.children.len(), 1, "the pieces are one line");
        let Kind::Note(Note::Said { text }) = &step.children[0].kind else {
            panic!("what was said");
        };
        assert_eq!(text, "It has a main function.");
    }

    #[test]
    fn what_was_said_keeps_its_own_line_when_something_else_came_between() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(1, 1, 5, said("It has a ")),
                at(1, 1, 10, said("main function.")),
                at(
                    1,
                    1,
                    15,
                    AgentEvent::Usage {
                        usage: Default::default(),
                    },
                ),
                at(1, 1, 20, said("And more.")),
            ]),
            true,
        );

        let step = &forest[0].children[0].children[0];
        let said_lines = step
            .children
            .iter()
            .filter(|child| matches!(child.kind, Kind::Note(Note::Said { .. })))
            .count();
        assert_eq!(said_lines, 2, "the words around the usage are two lines");
    }

    #[test]
    fn the_output_of_a_call_is_one_line_and_not_one_line_per_piece() {
        let forest = build(
            &trace(vec![
                at(
                    1,
                    1,
                    0,
                    AgentEvent::Turn(TurnEvent::StepStarted { index: 1 }),
                ),
                at(1, 1, 5, started(1)),
                at(
                    1,
                    1,
                    9,
                    AgentEvent::Tool(ToolEvent::OutputDelta {
                        call_id: ToolCallId(1),
                        stream: ToolOutputStream::Stdout,
                        text: "a".into(),
                    }),
                ),
                at(
                    1,
                    1,
                    10,
                    AgentEvent::Tool(ToolEvent::OutputDelta {
                        call_id: ToolCallId(1),
                        stream: ToolOutputStream::Stdout,
                        text: "b".into(),
                    }),
                ),
            ]),
            true,
        );

        let call = &forest[0].children[0].children[0].children[0];
        assert_eq!(call.children.len(), 1, "the pieces are one line");
        let Kind::Note(Note::Output { text }) = &call.children[0].kind else {
            panic!("the output");
        };
        assert_eq!(text, "ab");
    }
}
