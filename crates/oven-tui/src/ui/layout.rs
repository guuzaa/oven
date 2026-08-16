use ratatui::layout::{Constraint, Direction, Layout, Rect};

const TRANSCRIPT_MIN: u16 = 3;
const STATUS_H: u16 = 1;

pub struct Regions {
    pub transcript: Rect,
    pub queue: Option<Rect>,
    pub todos: Option<Rect>,
    pub input: Rect,
    pub overlay: Option<Rect>,
    pub status: Rect,
    pub reply: Option<Rect>,
}

pub fn split(
    area: Rect,
    mut input_h: u16,
    mut queue_h: u16,
    mut todos_h: u16,
    mut overlay_h: u16,
    mut reply_h: u16,
) -> Regions {
    let avail = area.height;
    reply_h = reply_h.min(avail.saturating_sub(TRANSCRIPT_MIN + STATUS_H));
    let chrome = TRANSCRIPT_MIN + STATUS_H + reply_h;
    input_h = input_h.min(avail.saturating_sub(chrome));
    queue_h = queue_h.min(avail.saturating_sub(chrome + input_h));
    todos_h = todos_h.min(avail.saturating_sub(chrome + input_h + queue_h));
    overlay_h = overlay_h.min(avail.saturating_sub(chrome + input_h + queue_h + todos_h));

    let mut constraints = vec![Constraint::Min(TRANSCRIPT_MIN)];
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
    if reply_h > 0 {
        constraints.push(Constraint::Length(reply_h));
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut i = 0;
    let transcript = chunks[i];
    i += 1;
    let queue = if queue_h > 0 {
        let r = chunks[i];
        i += 1;
        Some(r)
    } else {
        None
    };
    let todos = if todos_h > 0 {
        let r = chunks[i];
        i += 1;
        Some(r)
    } else {
        None
    };
    let input = chunks[i];
    i += 1;
    let overlay = if overlay_h > 0 {
        let r = chunks[i];
        i += 1;
        Some(r)
    } else {
        None
    };
    let status = chunks[i];
    let reply = if reply_h > 0 {
        Some(chunks[i + 1])
    } else {
        None
    };

    Regions {
        transcript,
        queue,
        todos,
        input,
        overlay,
        status,
        reply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn idle_layout_is_transcript_input_status() {
        let r = split(area(80, 24), 1, 0, 0, 0, 0);
        assert_eq!(r.transcript, Rect::new(0, 0, 80, 22));
        assert!(r.queue.is_none());
        assert!(r.todos.is_none());
        assert_eq!(r.input, Rect::new(0, 22, 80, 1));
        assert!(r.overlay.is_none());
        assert_eq!(r.status, Rect::new(0, 23, 80, 1));
        assert!(r.reply.is_none());
    }

    #[test]
    fn queue_overlay_and_reply_take_named_rows() {
        let r = split(area(80, 24), 2, 1, 0, 4, 2);
        assert_eq!(r.transcript.height, 14);
        assert_eq!(r.queue, Some(Rect::new(0, 14, 80, 1)));
        assert!(r.todos.is_none());
        assert_eq!(r.input, Rect::new(0, 15, 80, 2));
        assert_eq!(r.overlay, Some(Rect::new(0, 17, 80, 4)));
        assert_eq!(r.status, Rect::new(0, 21, 80, 1));
        assert_eq!(r.reply, Some(Rect::new(0, 22, 80, 2)));
    }

    #[test]
    fn todos_sit_between_queue_and_input() {
        let r = split(area(80, 24), 1, 1, 3, 0, 0);
        assert_eq!(r.transcript.height, 18);
        assert_eq!(r.queue, Some(Rect::new(0, 18, 80, 1)));
        assert_eq!(r.todos, Some(Rect::new(0, 19, 80, 3)));
        assert_eq!(r.input, Rect::new(0, 22, 80, 1));
        assert_eq!(r.status, Rect::new(0, 23, 80, 1));
    }

    #[test]
    fn empty_todos_are_absent() {
        let r = split(area(80, 24), 1, 0, 0, 0, 0);
        assert!(r.todos.is_none());
        let r = split(area(80, 24), 1, 1, 0, 0, 0);
        assert!(r.todos.is_none());
        assert!(r.queue.is_some());
    }

    #[test]
    fn narrow_height_preserves_transcript_minimum() {
        let r = split(area(40, 8), 8, 3, 6, 6, 5);
        assert!(r.transcript.height >= TRANSCRIPT_MIN);
        assert_eq!(r.status.height, STATUS_H);
        let used = r.transcript.height
            + r.queue.map(|q| q.height).unwrap_or(0)
            + r.todos.map(|t| t.height).unwrap_or(0)
            + r.input.height
            + r.overlay.map(|o| o.height).unwrap_or(0)
            + r.status.height
            + r.reply.map(|p| p.height).unwrap_or(0);
        assert_eq!(used, 8);
    }
}
