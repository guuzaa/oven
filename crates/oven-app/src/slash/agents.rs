use std::fmt::Write;

use oven_agent::NodeInfo;

use super::{CommandContext, CommandOutcome, SlashCommand};
use crate::AppError;

const NO_SUBAGENTS: &str = "no subagents have run in this session";
const STOP: &str = "stop";
const FORGET: &str = "forget";
const ALL: &str = "all";
const USAGE: &str = "usage: /agents [<name> | stop <name|all> | forget <name>]";

/// `/agents` — list subagents, open one, or stop and drop them. The registry
/// lives in the runtime, so the command reads and cancels through it and
/// leaves the view handling to the frontend.
pub struct Agents;

impl SlashCommand for Agents {
    fn name(&self) -> &str {
        "agents"
    }

    fn description(&self) -> &str {
        "list, inspect or stop subagents"
    }

    fn execute(&self, cx: &mut CommandContext<'_>, args: &str) -> Result<CommandOutcome, AppError> {
        let subagents = cx.subagents;
        match args.split_whitespace().collect::<Vec<_>>().as_slice() {
            [] => Ok(CommandOutcome::Reply(list(&subagents.snapshot()))),
            [STOP, ALL] => {
                let stopped = subagents.active();
                subagents.cancel_all();
                Ok(CommandOutcome::Reply(format!(
                    "stopped {stopped} subagents"
                )))
            }
            [STOP, name] => {
                let id = subagents.resolve(name).map_err(AppError::Runtime)?;
                let stopped = subagents.cancel(id);
                Ok(CommandOutcome::Reply(stop_notice(name, stopped)))
            }
            [FORGET, name] => {
                let id = subagents.resolve(name).map_err(AppError::Runtime)?;
                let dropped = subagents.forget(id);
                Ok(CommandOutcome::Reply(match dropped {
                    true => format!("dropped {name}"),
                    false => format!("{name} is already gone"),
                }))
            }
            [name] => Ok(CommandOutcome::FocusSubagent {
                id: subagents.resolve(name).map_err(AppError::Runtime)?,
            }),
            _ => Ok(CommandOutcome::Reply(USAGE.to_string())),
        }
    }
}

fn stop_notice(name: &str, stopped: bool) -> String {
    match stopped {
        true => format!("stopping {name}"),
        false => format!("{name} is not running"),
    }
}

/// One line per subagent: the address a user types back, what it is doing,
/// what it has spent, and the label it was spawned with.
fn list(subagents: &[NodeInfo]) -> String {
    if subagents.is_empty() {
        return NO_SUBAGENTS.to_string();
    }
    let mut out = String::new();
    for (index, info) in subagents.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let _ = write!(
            out,
            "{}. {} · {} · {} steps, {} tool calls · {} — {}",
            index + 1,
            info.name,
            info.status.label(),
            info.steps,
            info.tool_calls,
            elapsed(info),
            info.label
        );
    }
    out
}

fn elapsed(info: &NodeInfo) -> String {
    format!("{:.1}s", info.elapsed_ms() as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::SlashRegistry;
    use crate::subagent::Subagents;
    use oven_agent::{Agent, AgentId, NodeStatus, TurnId};
    use oven_llm::Router;

    const LABEL: &str = "find the spawn path";

    fn info(status: NodeStatus) -> NodeInfo {
        let started_at = oven_host::now_ms();
        let finished_at = (!status.is_active()).then_some(started_at + 32_400);
        NodeInfo {
            id: AgentId::next(),
            name: "explore#1".into(),
            role: "explore".into(),
            label: LABEL.into(),
            background: false,
            parent: AgentId::next(),
            status,
            usage: Default::default(),
            tool_calls: 3,
            steps: 4,
            started_at,
            finished_at,
        }
    }

    fn run(args: &str) -> Result<CommandOutcome, AppError> {
        let mut agent = Agent::new(Router::new(), Vec::new());
        let subagents = Subagents::bare(agent.id(), agent.router_handle());
        let mut cx = CommandContext::with_agent(&mut agent, &subagents);
        SlashRegistry::with_builtin().run(&mut cx, "agents", args)
    }

    #[test]
    fn an_empty_registry_says_so() {
        assert_eq!(
            list(&[]),
            NO_SUBAGENTS,
            "a listing with nothing in it should say so"
        );
    }

    #[test]
    fn listing_numbers_names_status_and_label() {
        let text = list(&[
            info(NodeStatus::Running {
                turn_id: TurnId::next(),
            }),
            info(NodeStatus::Completed),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("1. explore#1 · running · 4 steps, 3 tool calls"),
            "{text}"
        );
        assert!(lines[1].contains("· done ·"), "{text}");
        assert!(lines[1].contains("32.4s"), "{text}");
        assert!(lines[1].ends_with(LABEL), "{text}");
    }

    #[test]
    fn too_many_arguments_explain_the_grammar() {
        match run("stop the thing now") {
            Ok(CommandOutcome::Reply(text)) => assert_eq!(text, USAGE),
            other => panic!("expected usage, got {other:?}"),
        }
    }
}
