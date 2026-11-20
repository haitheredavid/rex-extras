//! The lane gutter shared by ↔ lanes (TIME, SEQUENCE, HEAT): agent color, status shape, name and
//! state on the left of the strip, and context, cost and working share on its right. Columns
//! drop by priority as the width shrinks, so every row keeps its state.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::layout::SizeClass;
use super::{App, fmt_dur, hm, put_str, status_glyph, truncate};
use crate::config::history_ms;
use crate::model::{AgentStatus, State, status_time};
use crate::session::fmt_tokens;
use crate::theme::theme;

/// Narrowest strip worth keeping right-hand columns for.
const MIN_STRIP: u16 = 24;
const CTX_W: u16 = 6;
const COST_W: u16 = 7;
const WORK_W: u16 = 5;
/// The band's group column.
pub(super) const GROUP_W: u16 = 10;

/// Gutter column widths for one frame width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cols {
    /// Group column before the gutter (bands only), 0 without.
    pub group_w: u16,
    pub name_w: u16,
    /// 0 drops the state column.
    pub state_w: u16,
    pub ctx: bool,
    pub cost: bool,
    pub work: bool,
}

impl Cols {
    /// A sidebar keeps state and context and gives the strip what's left (maybe nothing); bands
    /// and panels keep a usable strip and add right-hand columns while they fit.
    pub fn fit(width: u16, class: SizeClass, group_col: bool) -> Cols {
        let group_w = if group_col { GROUP_W } else { 0 };
        if class == SizeClass::Sidebar {
            let state_w = if width >= 40 { 14 } else { 0 };
            let name_w = width.saturating_sub(3 + 15 + 6 + 9).clamp(10, 24);
            return Cols { group_w, name_w, state_w, ctx: width >= 50, cost: false, work: false };
        }
        let width = width.saturating_sub(group_w);
        let mut c = Cols {
            group_w,
            name_w: (width / 6).clamp(14, 24),
            state_w: if width >= 60 { 14 } else { 0 },
            ctx: false,
            cost: false,
            work: false,
        };
        let left = c.left() - group_w;
        let mut right = 0;
        for (w, keep) in [(CTX_W, &mut c.ctx), (COST_W, &mut c.cost), (WORK_W, &mut c.work)] {
            if left + MIN_STRIP + right + w > width {
                break;
            }
            right += w;
            *keep = true;
        }
        c
    }

    /// Cells left of the strip: group column, color bar, status, gap, name, gap, state.
    pub fn left(&self) -> u16 {
        self.group_w + 3 + self.name_w + 1 + self.state_w + u16::from(self.state_w > 0)
    }

    /// Cells right of the strip.
    pub fn right(&self) -> u16 {
        u16::from(self.ctx) * CTX_W + u16::from(self.cost) * COST_W + u16::from(self.work) * WORK_W
    }

    /// Strip cells in a frame `width` wide.
    pub fn strip(&self, width: u16) -> u16 {
        width.saturating_sub(self.left() + self.right())
    }
}

/// Labels for `rows`: a label shown more than once gets its workspace, then its pane id.
pub(super) fn display_labels(state: &State, rows: &[usize]) -> Vec<String> {
    let count = |labels: &[String]| {
        let mut n: HashMap<&str, usize> = HashMap::new();
        for l in labels {
            *n.entry(l).or_default() += 1;
        }
        labels.iter().map(|l| n[l.as_str()] > 1).collect::<Vec<bool>>()
    };
    let mut labels: Vec<String> = rows.iter().map(|&i| state.lanes[i].label().to_string()).collect();
    let suffixes: [fn(&State, usize) -> String; 2] = [|s, i| s.workspace_label(i).to_string(), |s, i| s.lanes[i].pane_id.clone()];
    for suffix in suffixes {
        let dup = count(&labels);
        for (k, &i) in rows.iter().enumerate() {
            if dup[k] {
                labels[k] = format!("{} · {}", labels[k], suffix(state, i));
            }
        }
    }
    labels
}

/// `$0.00`, or `–` when the lane has no usage to cost.
pub(super) fn fmt_cost(cost: Option<f64>) -> String {
    cost.map_or("–".into(), |c| format!("${c:.2}"))
}

/// Draws lane `li`'s gutter at row `y` (and, for a band `rows` tall, its detail below), with
/// right-hand columns from `right_x`.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_row(
    buf: &mut Buffer,
    body: Rect,
    cols: &Cols,
    app: &App,
    li: usize,
    label: &str,
    y: u16,
    rows: u16,
    selected: bool,
) {
    let lane = &app.state.lanes[li];
    let (x, now) = (body.x + cols.group_w, app.now_ms);
    let dim = Style::new().fg(theme().dim);
    let end = lane.closed_ms.unwrap_or(now);
    let sel = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
    if selected {
        for cx in x..(body.x + cols.left()).min(body.right()) {
            buf[(cx, y)].set_style(sel);
        }
    }
    let agent = theme().agent_color(lane.slot.unwrap_or(0));
    put_str(buf, body, x, y, "▌", if lane.is_open() { Style::new().fg(agent) } else { dim }.patch(sel));
    let (glyph, gs) =
        if lane.is_open() { (status_glyph(lane.status), Style::new().fg(theme().status(lane.status))) } else { ('✕', dim) };
    put_str(buf, body, x + 1, y, &glyph.to_string(), gs.patch(sel));
    let name_style = if lane.is_open() { Style::new() } else { dim };
    let name_w = cols.name_w as usize;
    put_str(buf, body, x + 3, y, &format!("{:<name_w$}", truncate(label, name_w)), name_style.patch(sel));
    if cols.state_w > 0 {
        let state = match lane.closed_ms {
            None => format!("{} {}", lane.status.as_str(), fmt_dur(now - lane.last_ms())),
            Some(c) => format!("closed {}", hm(c)),
        };
        let mut style = Style::new().fg(theme().status(lane.status));
        if lane.is_open() && lane.status == AgentStatus::Blocked {
            style = style.add_modifier(Modifier::BOLD);
        }
        let state_w = cols.state_w as usize;
        put_str(buf, body, x + 4 + cols.name_w, y, &format!("{:<state_w$}", truncate(&state, state_w)), style.patch(sel));
    }

    let from = now - history_ms();
    let spend = app.sessions.spend_between(lane, from, now);
    let cost = spend.cost;
    let mut rx = body.right().saturating_sub(cols.right());
    if cols.ctx {
        let ctx = app.sessions.ctx_at(lane, end).map_or("–".into(), fmt_tokens);
        rx = put_str(buf, body, rx, y, &format!(" {ctx:>5}"), Style::new());
    }
    if cols.cost {
        rx = put_str(buf, body, rx, y, &format!(" {:>6}", fmt_cost(cost)), Style::new());
    }
    if cols.work {
        let st = status_time(lane, from, now);
        let share = if st.alive_ms > 0 { st.working_ms * 100 / st.alive_ms } else { 0 };
        put_str(buf, body, rx, y, &format!(" {:>3}%", share), Style::new().fg(theme().working));
    }

    if rows > 1 {
        let ctx = app.sessions.ctx_at(lane, end).map_or("–".into(), fmt_tokens);
        let peak = app.sessions.max_between(lane, lane.opened_ms, end).map_or("–".into(), fmt_tokens);
        let tab = app.state.tab_label(li).map(|t| format!(" · tab {t}")).unwrap_or_default();
        let detail = [
            format!("ctx {ctx} · peak {peak}"),
            if spend.requests > 0 { format!("{} · {} reqs", fmt_cost(cost), spend.requests) } else { "no usage".into() },
            format!("{}{tab} · {}", lane.agent.as_deref().unwrap_or("-"), lane.pane_id),
        ];
        let w = (cols.left() - cols.group_w).saturating_sub(4) as usize;
        for (k, text) in detail.iter().enumerate().take(rows as usize - 1) {
            put_str(buf, body, x + 3, y + 1 + k as u16, &truncate(text, w), dim);
        }
    }
}

/// `▲1 ●2 ○1`: a group's open agents by status, in status colors.
pub(super) fn rollup(counts: &[usize; 5]) -> Vec<Span<'static>> {
    let statuses = [AgentStatus::Blocked, AgentStatus::Working, AgentStatus::Done, AgentStatus::Idle, AgentStatus::Unknown];
    let mut out = Vec::new();
    for (s, &n) in statuses.iter().zip(counts).filter(|(_, n)| **n > 0) {
        if !out.is_empty() {
            out.push(Span::raw(" "));
        }
        out.push(Span::styled(format!("{}{n}", status_glyph(*s)), Style::new().fg(theme().status(*s))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Mark;

    #[test]
    fn cols_drop_by_priority() {
        let c = Cols::fit(60, SizeClass::Panel, false);
        assert_eq!((c.left(), c.state_w, c.right()), (33, 14, 0));
        let c = Cols::fit(120, SizeClass::Band, false);
        assert_eq!(c.name_w, 20);
        assert!(c.ctx && c.cost && c.work);
        assert_eq!(Cols::fit(40, SizeClass::Panel, false).state_w, 0);
        let c = Cols::fit(120, SizeClass::Band, true);
        assert_eq!((c.group_w, c.name_w), (GROUP_W, 18), "the group column comes out of the gutter's share");
        let c = Cols::fit(50, SizeClass::Sidebar, false);
        assert_eq!((c.name_w, c.state_w, c.ctx, c.cost), (17, 14, true, false));
    }

    #[test]
    fn duplicate_labels_get_their_workspace_then_pane() {
        let mut s = State::default();
        for (p, w) in [("a:p1", "a"), ("b:p1", "b"), ("b:p2", "b")] {
            let i = s.open(p, w, 0, Mark::Seen, AgentStatus::Idle, true);
            s.lanes[i].agent = Some("omp".into());
        }
        assert_eq!(display_labels(&s, &[0, 1, 2]), ["omp · a", "omp · b · b:p1", "omp · b · b:p2"]);
    }
}
