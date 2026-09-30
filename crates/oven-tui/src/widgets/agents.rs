//! The subagent strip: what has been delegated and how it is doing, one row
//! per subagent above the composer.

use std::fmt::Write;

use oven_app::{AgentId, NodeInfo, NodeStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use crate::core::theme;

const MAX_ROWS: usize = 3;
const RUNNING: &str = "◆";
const SETTLED: &str = "◇";

/// Rows the strip wants: one per subagent shown, plus an overflow marker.
pub fn height(agents: &[NodeInfo]) -> u16 {
    match agents.is_empty() {
        true => 0,
        false => u16::try_from(rows(agents).len()).unwrap_or(u16::MAX),
    }
}

pub fn draw(f: &mut Frame<'_>, area: Rect, agents: &[NodeInfo]) {
    let shown = rows(agents);
    let hidden = agents.len().saturating_sub(shown.len());
    for (index, agent) in shown.iter().enumerate() {
        let Ok(offset) = u16::try_from(index) else {
            break;
        };
        if offset >= area.height {
            break;
        }
        let mut text = line_text(agent);
        if hidden > 0 && index + 1 == shown.len() {
            let _ = write!(text, "  +{hidden}");
        }
        let line = Rect {
            y: area.y + offset,
            height: 1,
            ..area
        };
        f.render_widget(Paragraph::new(Span::styled(text, style(agent))), line);
    }
}

/// The subagent whose row sits at `y`, so a click can open its transcript.
pub fn row_at(area: Rect, agents: &[NodeInfo], y: u16) -> Option<AgentId> {
    let offset = usize::from(y.checked_sub(area.y)?);
    rows(agents).get(offset).map(|agent| agent.id)
}

/// Active subagents first — they are what the user is waiting on — then the
/// finished ones, newest first, capped so the strip never crowds the
/// transcript. Row order is stable between redraws, which is what makes
/// clicking a row mean the same thing twice.
fn rows(agents: &[NodeInfo]) -> Vec<&NodeInfo> {
    let mut ordered: Vec<&NodeInfo> = agents.iter().filter(|a| a.status.is_active()).collect();
    ordered.extend(agents.iter().rev().filter(|a| !a.status.is_active()));
    ordered.truncate(MAX_ROWS);
    ordered
}

/// The row that replaces the composer while a subagent's transcript owns
/// the screen: what is open, and what the viewer answers to.
pub fn draw_hint(f: &mut Frame<'_>, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(Span::styled(text.to_string(), theme::accent())),
        area,
    );
}

fn line_text(agent: &NodeInfo) -> String {
    let marker = match agent.status.is_active() {
        true => RUNNING,
        false => SETTLED,
    };
    let mut parts = vec![
        agent.name.clone(),
        format!("{} {}", agent.status.label(), elapsed(agent)),
    ];
    if agent.tool_calls > 0 {
        parts.push(format!("{} tools", agent.tool_calls));
    }
    if !agent.label.is_empty() {
        parts.push(agent.label.clone());
    }
    format!("{marker} {}", parts.join(" · "))
}

fn elapsed(agent: &NodeInfo) -> String {
    format!("{:.1}s", agent.elapsed_ms() as f64 / 1000.0)
}

fn style(agent: &NodeInfo) -> ratatui::style::Style {
    match agent.status {
        NodeStatus::Failed { .. } => theme::fail(),
        NodeStatus::Running { .. } | NodeStatus::Pending => theme::accent(),
        NodeStatus::Completed | NodeStatus::Cancelled => theme::dim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_app::TurnId;

    fn agent(name: &str, status: NodeStatus, label: &str) -> NodeInfo {
        NodeInfo {
            id: AgentId::next(),
            name: name.into(),
            role: "explore".into(),
            label: label.into(),
            background: false,
            parent: AgentId::next(),
            status,
            usage: Default::default(),
            tool_calls: 3,
            steps: 4,
            started_at: oven_host::now_ms(),
            finished_at: None,
        }
    }

    fn running(name: &str) -> NodeInfo {
        agent(
            name,
            NodeStatus::Running {
                turn_id: TurnId::next(),
            },
            "",
        )
    }

    fn done(name: &str, ms: u64) -> NodeInfo {
        let mut info = agent(name, NodeStatus::Completed, "");
        info.finished_at = Some(info.started_at + ms);
        info
    }

    #[test]
    fn no_subagents_take_no_rows() {
        assert_eq!(height(&[]), 0);
    }

    #[test]
    fn one_row_per_subagent_up_to_the_cap() {
        let agents: Vec<NodeInfo> = (0..5).map(|i| running(&format!("a{i}"))).collect();
        assert_eq!(height(&agents), MAX_ROWS as u16);
    }

    #[test]
    fn running_subagents_come_before_finished_ones() {
        let agents = vec![done("done#1", 1), running("live#1"), done("done#2", 2)];
        let text: Vec<String> = rows(&agents).iter().map(|a| line_text(a)).collect();
        assert!(text[0].starts_with("◆ live#1"), "{text:?}");
        assert!(
            text[1].starts_with("◇ done#2"),
            "newest finished first: {text:?}"
        );
        assert!(text[2].starts_with("◇ done#1"), "{text:?}");
    }

    #[test]
    fn a_row_names_status_elapsed_tools_and_label() {
        let mut info = agent("explore#1", NodeStatus::Completed, "find the spawn path");
        info.finished_at = Some(info.started_at + 32_400);
        assert_eq!(
            line_text(&info),
            "◇ explore#1 · done 32.4s · 3 tools · find the spawn path"
        );
    }

    #[test]
    fn a_row_without_tools_or_a_label_stays_tidy() {
        let mut info = running("explore#1");
        info.tool_calls = 0;
        assert!(
            line_text(&info).starts_with("◆ explore#1 · running "),
            "{}",
            line_text(&info)
        );
        assert!(!line_text(&info).contains("tools"), "{}", line_text(&info));
    }

    #[test]
    fn a_click_maps_back_to_its_row() {
        let agents = vec![running("a#1"), done("b#1", 1)];
        let area = Rect::new(0, 10, 40, 3);
        assert_eq!(row_at(area, &agents, 10), Some(agents[0].id));
        assert_eq!(row_at(area, &agents, 11), Some(agents[1].id));
        assert_eq!(row_at(area, &agents, 9), None);
    }
}
