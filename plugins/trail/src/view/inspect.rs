//! Lane inspector: the selected agent in full, beside the lanes (↔) or under them (↕). On when
//! there's room unless `i` says otherwise; USAGE shows it only when asked.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::gutter::fmt_cost;
use super::layout::SizeClass;
use super::{App, Mode, Orient, fmt_dur, hm, hms, node_glyph, put_str, status_glyph, truncate};
use crate::config::history_ms;
use crate::model::{Mark, status_time};
use crate::session::fmt_tokens;
use crate::theme::theme;

const SIDE_W: u16 = 44;
const BOTTOM_H: u16 = 12;
const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Splits `body` into the lanes' area and the inspector's when the inspector shows; true when it
/// sits under the lanes. A sidebar shows it under `rows` rows when 12 more are free.
pub(super) fn place(app: &App, body: Rect, rows: usize) -> Option<(Rect, Rect, bool)> {
    let sidebar = app.class == SizeClass::Sidebar && app.mode == Mode::Lanes && app.orient == Orient::Horizontal;
    let bottom = app.mode == Mode::Lanes && (app.orient == Orient::Vertical || sidebar);
    let auto = match app.mode {
        Mode::Lanes if sidebar => usize::from(body.height) >= 1 + rows * 2 + usize::from(BOTTOM_H),
        Mode::Lanes if bottom => body.height >= 30,
        Mode::Lanes => body.width >= 140,
        Mode::Usage => false,
    };
    if !app.inspector.unwrap_or(auto) {
        return None;
    }
    if bottom {
        let h = BOTTOM_H.min(body.height.saturating_sub(4));
        (h >= 4).then(|| (Rect { height: body.height - h, ..body }, Rect { y: body.bottom() - h, height: h, ..body }, true))
    } else {
        let w = SIDE_W.min(body.width.saturating_sub(30));
        (w >= 20).then(|| (Rect { width: body.width - w, ..body }, Rect { x: body.right() - w, width: w, ..body }, false))
    }
}

fn mark_name(m: Mark) -> &'static str {
    match m {
        Mark::Created => "created",
        Mark::Seen => "seen",
        Mark::Status(s) => s.as_str(),
        Mark::Closed => "closed",
        Mark::Exited => "exited",
    }
}

pub(super) fn draw(buf: &mut Buffer, area: Rect, bottom: bool, app: &App, li: usize) {
    let dim = Style::new().fg(theme().dim);
    // A border toward the lanes: `─` along the top, or `│` down the left.
    let content = if bottom {
        put_str(buf, area, area.x, area.y, &"─".repeat(area.width as usize), dim);
        Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area }
    } else {
        for y in area.y..area.bottom() {
            put_str(buf, area, area.x, y, "│", dim);
        }
        Rect { x: area.x + 2, width: area.width.saturating_sub(2), ..area }
    };
    let w = content.width as usize;
    let lane = &app.state.lanes[li];
    let now = app.now_ms;
    let end = lane.closed_ms.unwrap_or(now);
    let store = &app.sessions;

    let mut lines: Vec<(String, Style)> = Vec::new();
    lines.push((format!("{} {}", status_glyph(lane.status), lane.label()), Style::new().add_modifier(Modifier::BOLD)));
    let tab = app.state.tab_label(li).map(|t| format!(" · tab {t}")).unwrap_or_default();
    lines.push((
        format!("{}{tab} · {} · {}", app.state.workspace_label(li), lane.agent.as_deref().unwrap_or("-"), lane.pane_id),
        dim,
    ));
    let state = match lane.closed_ms {
        None => format!("{} {}", lane.status.as_str(), fmt_dur(now - lane.last_ms())),
        Some(c) => format!("closed {}", hms(c)),
    };
    lines.push((state, Style::new().fg(theme().status(lane.status))));
    lines.push((format!("opened {} · alive {}", hm(lane.opened_ms), fmt_dur(end - lane.opened_ms)), Style::new()));

    let peak = store.max_between(lane, lane.opened_ms, end);
    let window = store.ctx_window(lane).map(|w| format!(" / {}", fmt_tokens(w))).unwrap_or_default();
    let spark: String = (1..=10)
        .map(|i| {
            let at = lane.opened_ms + (end - lane.opened_ms) * i / 10;
            match (store.ctx_at(lane, at), peak) {
                (Some(c), Some(p)) if p > 0 => SPARK[((c as f64 / p as f64) * 7.0).round() as usize],
                _ => ' ',
            }
        })
        .collect();
    let fmt = |v: Option<u64>| v.map_or("–".into(), fmt_tokens);
    lines.push((format!("ctx {} · peak {}{window} {spark}", fmt(store.ctx_at(lane, end)), fmt(peak)), Style::new()));
    let spend = store.spend_between(lane, now - history_ms(), now);
    let st = status_time(lane, now - history_ms(), now);
    let working = if st.alive_ms > 0 { st.working_ms * 100 / st.alive_ms } else { 0 };
    lines.push((
        format!("{} · {} reqs · working {working}% · {} steps", fmt_cost(spend.cost), spend.requests, lane.steps.len()),
        Style::new(),
    ));
    if let Some(&root) = store.roots(lane).last() {
        let s = &store.sessions[root];
        let name = s.label.as_deref().or(s.id.as_deref()).unwrap_or("-");
        lines.push((format!("session {name} · {} turns · {} subagents", s.turns.len(), s.children.len()), dim));
    }
    lines.push((format!("─ recent {}", "─".repeat(w.saturating_sub(9))), dim));

    let rows = content.height as usize;
    let room = rows.saturating_sub(lines.len() + 1);
    for step in lane.steps.iter().rev().take(room) {
        let (glyph, color) = node_glyph(step.mark);
        let ctx = store.ctx_at(lane, step.at_ms).map(|c| format!("  {}", fmt_tokens(c))).unwrap_or_default();
        lines.push((format!("{}  {glyph} {}{ctx}", hms(step.at_ms), mark_name(step.mark)), Style::new().fg(color)));
    }
    for (k, (text, style)) in lines.iter().take(rows.saturating_sub(1)).enumerate() {
        put_str(buf, content, content.x, content.y + k as u16, &truncate(text, w), *style);
    }
    if rows > 0 {
        put_str(buf, content, content.x, content.bottom() - 1, &truncate("I timeline · ⏎ jump · i hide", w), dim);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{key, render, row, sample_app};
    use super::*;

    #[test]
    fn wide_frames_show_the_inspector_and_i_hides_it() {
        let mut app = sample_app(1, Orient::Horizontal);
        let buf = render(&mut app, 160, 14);
        let side: Vec<String> = (1..14).map(|y| row(&buf, y, 160 - SIDE_W + 2, 160)).collect();
        assert!(side[0].contains("omp"), "{side:?}");
        assert!(side.iter().any(|l| l.starts_with("blocked 3s")), "{side:?}");
        assert!(side.iter().any(|l| l.contains("▲ blocked")), "recent steps, newest first: {side:?}");
        assert_eq!(side[12], "I timeline · ⏎ jump · i hide");
        key(&mut app, 'i');
        let buf = render(&mut app, 160, 14);
        assert!(!row(&buf, 13, 120, 160).contains("I timeline"));
        key(&mut app, 'i');
        let buf = render(&mut app, 100, 14);
        assert!(row(&buf, 13, 56, 100).contains("I timeline"), "i forces it on when narrow too");
    }
}
