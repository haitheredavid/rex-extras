//! Ctx lens: context in use per time bucket as a sparkline in the agent's color, against the
//! largest context on screen; the gutter's right columns give the numbers, and a totals row sums
//! them when there is room.

use ratatui::buffer::Buffer;
use ratatui::style::Style;

use super::gutter::fmt_cost;
use super::{App, Grid, Orient, Pt, put_str};
use crate::config::history_ms;
use crate::model::Lane;
use crate::session::{Store, fmt_tokens};
use crate::theme::theme;

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Largest context across `rows` in `[from_ms, to_ms]`.
pub(super) fn max(app: &App, rows: &[usize], from_ms: i64, to_ms: i64) -> u64 {
    rows.iter()
        .filter_map(|&li| {
            let l = &app.state.lanes[li];
            app.sessions.max_between(l, from_ms.max(l.opened_ms), to_ms.min(l.closed_ms.unwrap_or(app.now_ms)))
        })
        .max()
        .unwrap_or(0)
}

/// One sparkline cell per bucket, at the context in use at its end; blank outside the lane's life
/// or before its first request.
pub(super) fn pts(lane: &Lane, store: &Store, max: u64, start: i64, bucket: i64, n: usize, now_ms: i64) -> Vec<Pt> {
    let color = theme().agent_color(lane.slot.unwrap_or(0));
    (0..n)
        .map(|i| {
            let t0 = start + i as i64 * bucket;
            let at = (t0 + bucket - 1).min(now_ms);
            let alive = at >= lane.opened_ms && lane.closed_ms.is_none_or(|c| at < c);
            let cell = (max > 0 && t0 <= now_ms && alive)
                .then(|| store.ctx_at(lane, at))
                .flatten()
                .map(|c| (SPARK[((c as f64 / max as f64) * 7.0).round().clamp(0.0, 7.0) as usize], color));
            Pt::new(cell, 0, None)
        })
        .collect()
}

/// `Σ` row under the lanes, when every row is shown and one is spare: total context now and cost.
pub(super) fn totals(buf: &mut Buffer, grid: &Grid, app: &App, rows: &[usize]) {
    let (above, _, below) = app.shown;
    if grid.orient != Orient::Horizontal || above > 0 || below > 0 {
        return;
    }
    let Some(y) = grid.placed(app.lane_scroll).last().map(|&(k, start)| start + grid.size(k).0) else { return };
    if y >= grid.body.bottom() {
        return;
    }
    let dim = Style::new().fg(theme().dim);
    let now = app.now_ms;
    put_str(buf, grid.body, grid.body.x + 3, y, "Σ", dim);
    let ctx: u64 = rows.iter().filter_map(|&li| app.sessions.ctx_at(&app.state.lanes[li], now)).sum();
    let cost = rows
        .iter()
        .filter_map(|&li| app.sessions.spend_between(&app.state.lanes[li], now - history_ms(), now).cost)
        .reduce(|a, b| a + b);
    let mut rx = grid.body.right().saturating_sub(grid.cols.right());
    if grid.cols.ctx {
        rx = put_str(buf, grid.body, rx, y, &format!(" {:>5}", fmt_tokens(ctx)), dim);
    }
    if grid.cols.cost {
        put_str(buf, grid.body, rx, y, &format!(" {:>6}", fmt_cost(cost)), dim);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{render, row, sample_app};
    use super::*;

    /// The old DASH row: context 120k then 150k, $1.25 over two requests, blocked for 3.5 s.
    fn with_usage() -> App {
        let mut app = sample_app(5, Orient::Horizontal);
        let id = app.state.lanes[0].id;
        app.sessions.insert_samples(id, &[(1000, 120_000, Some(0.5)), (3000, 150_000, Some(0.75))]);
        app
    }

    #[test]
    fn ctx_lens_rows_carry_context_cost_and_state() {
        let mut app = with_usage();
        let buf = render(&mut app, 120, 5);
        let line = row(&buf, 2, 0, 120);
        assert!(line.contains("150k") && line.contains("$1.25") && line.contains("blocked"), "{line}");
        let totals = row(&buf, 3, 0, 120);
        assert!(totals.contains('Σ') && totals.contains("150k") && totals.contains("$1.25"), "{totals}");
    }

    #[test]
    fn ctx_lens_draws_a_sparkline() {
        let mut app = with_usage();
        let buf = render(&mut app, 120, 4);
        // 1 s buckets: before 1 s nothing, then 120k (level 6 of 150k), 150k from 3 s on.
        let strip = row(&buf, 2, 39, 120 - 18);
        assert!(strip.ends_with(" ▇▇██████"), "{strip:?}");
        let x = 39 + strip.chars().count() as u16 - 1;
        assert_eq!(buf[(x, 2)].fg, theme().agent_color(0));
    }
}
