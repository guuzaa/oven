//! The agent strip: the driver, then what it has delegated, one row each
//! above the composer. The driver is only there while a subagent is, so the
//! highlight has a row for the transcript on screen and ↑↓ can reach it.

use std::fmt::Write;

use oven_app::{AgentId, NodeInfo, NodeStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::list::cycle_selected;
use crate::core::theme;

const MAX_ROWS: usize = 3;
const RUNNING: &str = "◆";
const SETTLED: &str = "◇";
const MAIN: &str = "main";

/// Rows the strip wants: the driver, then one per subagent shown.
pub fn height(agents: &[NodeInfo], show_settled: bool) -> u16 {
    let subs = rows(agents, show_settled).len();
    if subs == 0 {
        return 0;
    }
    u16::try_from(subs + 1).unwrap_or(u16::MAX)
}

pub fn draw(
    f: &mut Frame<'_>,
    area: Rect,
    main: AgentId,
    busy: bool,
    agents: &[NodeInfo],
    show_settled: bool,
    selected: Option<AgentId>,
) {
    let shown = rows(agents, show_settled);
    if shown.is_empty() {
        return;
    }
    paint(
        f,
        area,
        0,
        main_line(busy),
        main_style(selected == Some(main)),
    );
    let hidden = agents.len().saturating_sub(shown.len());
    for (index, agent) in shown.iter().enumerate() {
        let mut text = line_text(agent);
        if hidden > 0 && index + 1 == shown.len() {
            let _ = write!(text, "  +{hidden}");
        }
        paint(
            f,
            area,
            index + 1,
            text,
            style(agent, selected == Some(agent.id)),
        );
    }
}

/// Moves the highlight among the rows on screen. The driver is the first
/// row. With nothing highlighted, Down lands on the first subagent and Up
/// on the last — the driver is already the transcript on screen. An id that
/// has left the strip starts over the same way. `None` when the strip is
/// not showing.
pub fn cycle(
    main: AgentId,
    agents: &[NodeInfo],
    selected: Option<AgentId>,
    up: bool,
    show_settled: bool,
) -> Option<AgentId> {
    let shown = strip_ids(main, agents, show_settled);
    let n = shown.len();
    if n == 0 {
        return None;
    }
    let found = selected.and_then(|id| shown.iter().position(|row| *row == id));
    let index = match found {
        Some(index) => {
            let mut index = index;
            cycle_selected(&mut index, n, up);
            index
        }
        None if up => n - 1,
        None => 1.min(n - 1),
    };
    Some(shown[index])
}

pub fn on_strip(main: AgentId, agents: &[NodeInfo], id: AgentId, show_settled: bool) -> bool {
    strip_ids(main, agents, show_settled).contains(&id)
}

/// The agent whose row sits at `y`. The first row is the driver, so a click
/// there can leave a subagent's transcript.
pub fn row_at(
    area: Rect,
    main: AgentId,
    agents: &[NodeInfo],
    y: u16,
    show_settled: bool,
) -> Option<AgentId> {
    let offset = usize::from(y.checked_sub(area.y)?);
    strip_ids(main, agents, show_settled).get(offset).copied()
}

/// The driver, then the subagents [`rows`] keeps. Empty when the strip is
/// hidden, so the driver is never a row by itself.
fn strip_ids(main: AgentId, agents: &[NodeInfo], show_settled: bool) -> Vec<AgentId> {
    let shown = rows(agents, show_settled);
    if shown.is_empty() {
        return Vec::new();
    }
    let mut ids = Vec::with_capacity(shown.len() + 1);
    ids.push(main);
    ids.extend(shown.iter().map(|agent| agent.id));
    ids
}

pub(crate) fn main_line(busy: bool) -> String {
    let marker = match busy {
        true => RUNNING,
        false => SETTLED,
    };
    format!("{marker} {MAIN}")
}

fn main_style(selected: bool) -> Style {
    match selected {
        true => theme::accent().add_modifier(Modifier::REVERSED),
        false => Style::default(),
    }
}

fn paint(f: &mut Frame<'_>, area: Rect, index: usize, text: String, row_style: Style) {
    let Ok(offset) = u16::try_from(index) else {
        return;
    };
    if offset >= area.height {
        return;
    }
    let line = Rect {
        y: area.y + offset,
        height: 1,
        ..area
    };
    f.render_widget(Paragraph::new(Span::styled(text, row_style)), line);
}

/// Active subagents first — they are what the user is waiting on — then the
/// finished ones, newest first, capped so the strip never crowds the
/// transcript. With nothing still running the strip is hidden, unless the
/// caller is holding it open so a transcript can be opened again. Row order
/// is stable between redraws, which is what makes clicking a row mean the
/// same thing twice.
fn rows(agents: &[NodeInfo], show_settled: bool) -> Vec<&NodeInfo> {
    if !show_settled && !agents.iter().any(|agent| agent.status.is_active()) {
        return Vec::new();
    }
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

/// A running subagent fixture. `started_at` is now, so a drawn elapsed time
/// stays near zero instead of counting from the unix epoch.
#[cfg(test)]
pub fn running(name: &str) -> NodeInfo {
    use oven_app::TurnId;
    NodeInfo {
        id: AgentId::next(),
        name: name.into(),
        role: "explore".into(),
        label: String::new(),
        background: false,
        parent: AgentId::next(),
        status: NodeStatus::Running {
            turn_id: TurnId::next(),
        },
        usage: Default::default(),
        tool_calls: 3,
        steps: 4,
        started_at: oven_host::now_ms(),
        finished_at: None,
    }
}

fn style(agent: &NodeInfo, selected: bool) -> Style {
    if selected {
        return theme::accent().add_modifier(Modifier::REVERSED);
    }
    match agent.status {
        NodeStatus::Failed { .. } => theme::fail(),
        NodeStatus::Completed | NodeStatus::Cancelled => theme::dim(),
        NodeStatus::Running { .. } | NodeStatus::Pending => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, status: NodeStatus, label: &str) -> NodeInfo {
        let mut info = super::running(name);
        info.status = status;
        info.label = label.into();
        info
    }

    fn done(name: &str, ms: u64) -> NodeInfo {
        let mut info = agent(name, NodeStatus::Completed, "");
        info.finished_at = Some(info.started_at + ms);
        info
    }

    #[test]
    fn an_unselected_running_row_is_not_accented() {
        let agent = running("a#1");
        assert_eq!(
            style(&agent, false),
            Style::default(),
            "running is not a highlight"
        );
        assert_eq!(
            style(&agent, true),
            theme::accent().add_modifier(Modifier::REVERSED)
        );
    }

    #[test]
    fn no_subagents_take_no_rows() {
        assert_eq!(height(&[], false), 0);
    }

    #[test]
    fn settled_subagents_take_no_rows() {
        let agents = vec![done("done#1", 1), done("done#2", 2)];
        assert_eq!(height(&agents, false), 0);
        assert!(rows(&agents, false).is_empty());
    }

    #[test]
    fn a_held_strip_keeps_settled_subagents() {
        let main = AgentId::next();
        let agents = vec![done("done#1", 1), done("done#2", 2)];
        assert_eq!(height(&agents, true), 3);
        assert_eq!(cycle(main, &agents, None, false, true), Some(agents[1].id));
    }

    #[test]
    fn the_driver_leads_the_strip_and_arrows_return_to_it() {
        let main = AgentId::next();
        let agents = vec![running("a#1")];
        assert_eq!(height(&agents, false), 2);
        assert_eq!(main_line(false), "◇ main");
        assert_eq!(main_line(true), "◆ main");
        assert!(on_strip(main, &agents, main, false));
        assert_eq!(
            cycle(main, &agents, Some(main), false, false),
            Some(agents[0].id)
        );
        assert_eq!(
            cycle(main, &agents, Some(agents[0].id), true, false),
            Some(main)
        );
    }

    #[test]
    fn one_row_per_subagent_up_to_the_cap() {
        let agents: Vec<NodeInfo> = (0..5).map(|i| running(&format!("a{i}"))).collect();
        assert_eq!(height(&agents, false), MAX_ROWS as u16 + 1);
    }

    #[test]
    fn running_subagents_come_before_finished_ones() {
        let agents = vec![done("done#1", 1), running("live#1"), done("done#2", 2)];
        let text: Vec<String> = rows(&agents, false).iter().map(|a| line_text(a)).collect();
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
    fn arrows_walk_the_visible_rows_and_wrap() {
        let main = AgentId::next();
        let agents = vec![running("a#1"), running("b#1"), running("c#1")];
        let first = cycle(main, &agents, None, false, false);
        assert_eq!(first, Some(agents[0].id));
        let second = cycle(main, &agents, first, false, false);
        assert_eq!(second, Some(agents[1].id));
        assert_eq!(
            cycle(main, &agents, Some(agents[2].id), false, false),
            Some(main),
            "down from the last subagent wraps to the driver"
        );
        assert_eq!(
            cycle(main, &agents, Some(main), false, false),
            Some(agents[0].id)
        );
        assert_eq!(cycle(main, &agents, None, true, false), Some(agents[2].id));
        assert_eq!(
            cycle(main, &agents, Some(agents[0].id), true, false),
            Some(main)
        );
        assert_eq!(
            cycle(main, &agents, Some(main), true, false),
            Some(agents[2].id)
        );
    }

    #[test]
    fn an_id_that_left_the_strip_starts_over() {
        let main = AgentId::next();
        let agents = vec![running("a#1")];
        let gone = AgentId::next();
        assert_eq!(
            cycle(main, &agents, Some(gone), false, false),
            Some(agents[0].id)
        );
        assert!(!on_strip(main, &agents, gone, false));
        assert!(on_strip(main, &agents, main, false));
    }

    #[test]
    fn arrows_do_nothing_once_the_strip_is_gone() {
        let main = AgentId::next();
        let agents = vec![done("done#1", 1)];
        assert_eq!(cycle(main, &agents, None, false, false), None);
        assert!(!on_strip(main, &agents, agents[0].id, false));
        assert!(!on_strip(main, &agents, main, false));
    }

    #[test]
    fn a_click_maps_back_to_its_row() {
        let main = AgentId::next();
        let agents = vec![running("a#1"), done("b#1", 1)];
        let area = Rect::new(0, 10, 40, 3);
        assert_eq!(row_at(area, main, &agents, 10, false), Some(main));
        assert_eq!(row_at(area, main, &agents, 11, false), Some(agents[0].id));
        assert_eq!(row_at(area, main, &agents, 12, false), Some(agents[1].id));
        assert_eq!(row_at(area, main, &agents, 9, false), None);
    }
}
