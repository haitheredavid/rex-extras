//! The one-line header: segments with priorities, dropped from the least important while the line
//! doesn't fit. Left segments read in order, separated by a dim `│`; right ones sit at the far end.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::theme::theme;

/// One header segment; lower `prio` is kept first.
pub(super) struct Seg {
    pub spans: Vec<Span<'static>>,
    pub prio: u8,
    pub right: bool,
}

impl Seg {
    pub fn left(prio: u8, spans: Vec<Span<'static>>) -> Seg {
        Seg { spans, prio, right: false }
    }

    pub fn right(prio: u8, spans: Vec<Span<'static>>) -> Seg {
        Seg { spans, prio, right: true }
    }

    fn width(&self) -> u16 {
        self.spans.iter().map(|s| s.content.chars().count() as u16).sum()
    }

    /// Cells it takes with its separator: `" │ "` before a left segment, two spaces before a right one.
    fn cost(&self) -> u16 {
        self.width() + if self.right { 2 } else { 3 }
    }

    /// Cuts the text to `max` cells, ending in `…`.
    fn truncate(&mut self, max: u16) {
        let mut left = max.saturating_sub(1) as usize;
        let mut out = Vec::new();
        for s in self.spans.drain(..) {
            let n = s.content.chars().count();
            if n <= left {
                left -= n;
                out.push(s);
                continue;
            }
            let text: String = s.content.chars().take(left).collect();
            out.push(Span::styled(text + "…", s.style));
            break;
        }
        self.spans = out;
    }
}

/// Draws the segments that fit; returns where each input segment landed (None when dropped).
pub(super) fn draw_header(buf: &mut Buffer, area: Rect, mut segs: Vec<Seg>) -> Vec<Option<Rect>> {
    let mut order: Vec<usize> = (0..segs.len()).collect();
    order.sort_by_key(|&i| segs[i].prio);
    // Separators are counted for every segment; the first left one draws none, so 3 cells spare.
    let budget = area.width + 3;
    let mut used = 0;
    let mut keep = vec![false; segs.len()];
    for (n, &i) in order.iter().enumerate() {
        if n == 0 && segs[i].cost() > budget {
            segs[i].truncate(budget.saturating_sub(3));
        }
        if used + segs[i].cost() <= budget {
            used += segs[i].cost();
            keep[i] = true;
        }
    }
    let dim = Style::new().fg(theme().dim);
    let mut left: Vec<Span<'static>> = Vec::new();
    let mut right: Vec<Span<'static>> = Vec::new();
    // (segment, side, offset within its side, width)
    let mut placed: Vec<(usize, bool, u16, u16)> = Vec::new();
    let (mut left_w, mut right_w) = (0u16, 0u16);
    for (i, seg) in segs.into_iter().enumerate().filter(|(i, _)| keep[*i]) {
        let (side, w) = if seg.right { (&mut right, &mut right_w) } else { (&mut left, &mut left_w) };
        if !side.is_empty() {
            let sep = if seg.right { "  " } else { " │ " };
            *w += sep.chars().count() as u16;
            side.push(Span::styled(sep, dim));
        }
        let width = seg.width();
        placed.push((i, seg.right, *w, width));
        *w += width;
        side.extend(seg.spans);
    }
    Line::from(left).render(area, buf);
    let x = area.right().saturating_sub(right_w).max(area.x);
    Line::from(right).render(Rect::new(x, area.y, area.right() - x, 1), buf);
    let mut rects = vec![None; keep.len()];
    for (i, right, at, width) in placed {
        rects[i] = Some(Rect::new(if right { x } else { area.x } + at, area.y, width, 1));
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buf: &Buffer, w: u16) -> String {
        (0..w).map(|x| buf[(x, 0)].symbol()).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn segments_drop_by_priority_and_keep_their_order() {
        let segs = || {
            vec![
                Seg::left(1, vec![Span::raw("views")]),
                Seg::left(3, vec![Span::raw("paused")]),
                Seg::left(2, vec![Span::raw("3/6")]),
                Seg::right(7, vec![Span::raw("? keys")]),
            ]
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 1));
        draw_header(&mut buf, Rect::new(0, 0, 40, 1), segs());
        let line = text(&buf, 40);
        assert!(line.starts_with("views │ paused │ 3/6"), "{line}");
        assert!(line.ends_with("? keys"), "{line}");

        let mut buf = Buffer::empty(Rect::new(0, 0, 14, 1));
        draw_header(&mut buf, Rect::new(0, 0, 14, 1), segs());
        assert_eq!(text(&buf, 14), "views │ 3/6", "lowest priorities go first");
    }

    #[test]
    fn an_overlong_first_segment_is_cut() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        draw_header(&mut buf, Rect::new(0, 0, 10, 1), vec![Seg::left(0, vec![Span::raw("a very long error message")])]);
        assert_eq!(text(&buf, 10), "a very lo…");
    }
}
