use ratatui::layout::{Constraint, Direction, Layout, Rect};

const TRANSCRIPT_MIN: u16 = 1;
const STATUS_H: u16 = 1;

pub struct Regions {
    pub transcript: Rect,
    pub queue: Option<Rect>,
    pub todos: Option<Rect>,
    pub input: Rect,
    pub overlay: Option<Rect>,
    pub status: Rect,
}

pub fn split(
    area: Rect,
    mut input_h: u16,
    mut queue_h: u16,
    mut todos_h: u16,
    mut overlay_h: u16,
) -> Regions {
    let body = area.height.saturating_sub(STATUS_H);
    let transcript_min = TRANSCRIPT_MIN.min(body);
    input_h = input_h.min(body.saturating_sub(transcript_min));
    queue_h = queue_h.min(body.saturating_sub(transcript_min + input_h));
    todos_h = todos_h.min(body.saturating_sub(transcript_min + input_h + queue_h));
    overlay_h = overlay_h.min(body.saturating_sub(transcript_min + input_h + queue_h + todos_h));

    let mut constraints = vec![Constraint::Min(transcript_min)];
    if queue_h > 0 {
        constraints.push(Constraint::Length(queue_h));
    }
    if todos_h > 0 {
        constraints.push(Constraint::Length(todos_h));
    }
    constraints.push(Constraint::Length(input_h));
    if overlay_h > 0 {
        constraints.push(Constraint::Length(overlay_h));
    }
    constraints.push(Constraint::Length(STATUS_H));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut i = 0;
    let transcript = chunks[i];
    i += 1;
    let queue = (queue_h > 0).then(|| {
        let area = chunks[i];
        i += 1;
        area
    });
    let todos = (todos_h > 0).then(|| {
        let area = chunks[i];
        i += 1;
        area
    });
    let input = chunks[i];
    i += 1;
    let overlay = (overlay_h > 0).then(|| {
        let area = chunks[i];
        i += 1;
        area
    });
    let status = chunks[i];

    Regions {
        transcript,
        queue,
        todos,
        input,
        overlay,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    fn assert_tiles(regions: &Regions, area: Rect) {
        let mut bands = vec![regions.transcript];
        bands.extend(regions.queue);
        bands.extend(regions.todos);
        bands.push(regions.input);
        bands.extend(regions.overlay);
        bands.push(regions.status);
        let mut y = area.y;
        for band in bands {
            assert_eq!(band.y, y, "{band:?} must tile from the top");
            assert_eq!(band.width, area.width, "{band:?} must span the width");
            y += band.height;
        }
        assert_eq!(y, area.y + area.height, "bands must cover the terminal");
    }

    #[test]
    fn idle_layout_is_transcript_input_status() {
        let regions = split(area(80, 24), 1, 0, 0, 0);
        assert_eq!(regions.transcript, Rect::new(0, 0, 80, 22));
        assert_eq!(regions.input, Rect::new(0, 22, 80, 1));
        assert_eq!(regions.status, Rect::new(0, 23, 80, 1));
        assert_tiles(&regions, area(80, 24));
    }

    #[test]
    fn queue_overlay_and_todos_take_named_rows() {
        let regions = split(area(80, 24), 2, 1, 3, 4);
        assert_eq!(regions.transcript.height, 13);
        assert_eq!(regions.queue, Some(Rect::new(0, 13, 80, 1)));
        assert_eq!(regions.todos, Some(Rect::new(0, 14, 80, 3)));
        assert_eq!(regions.input, Rect::new(0, 17, 80, 2));
        assert_eq!(regions.overlay, Some(Rect::new(0, 19, 80, 4)));
        assert_eq!(regions.status, Rect::new(0, 23, 80, 1));
        assert_tiles(&regions, area(80, 24));
    }

    #[test]
    fn short_terminal_preserves_transcript_and_status() {
        let regions = split(area(20, 2), 4, 1, 3, 2);
        assert_eq!(regions.transcript.height, TRANSCRIPT_MIN);
        assert_eq!(regions.input.height, 0);
        assert_eq!(regions.status.height, STATUS_H);
        assert_tiles(&regions, area(20, 2));
    }

    #[test]
    fn one_row_terminal_keeps_only_the_status() {
        let regions = split(area(20, 1), 1, 1, 1, 1);
        assert_eq!(regions.transcript.height, 0);
        assert_eq!(regions.status.height, STATUS_H);
        assert_tiles(&regions, area(20, 1));
    }
}
