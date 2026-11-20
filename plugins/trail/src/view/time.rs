//! Status lens on the time axis: cells are fixed time buckets, so line length is wall-clock
//! duration. Also the time-axis geometry the other timed lenses share.

use ratatui::buffer::Buffer;
use ratatui::style::Style;

use super::{App, Cell, Grid, Orient, Pt, Scale, head_cell, line_cell, node_cell, put_str, time_ticks};
use crate::model::{Lane, status_at};
use crate::session::Store;
use crate::theme::theme;

/// ↕ axis gutter: `HH:MM` ticks when they're a minute or more apart, else `HH:MM:SS`, plus a space.
pub(super) fn axis_w(bucket_ms: i64) -> u16 {
    if bucket_ms * 4 >= 60_000 { 6 } else { 9 }
}

/// Start of the first cell: the last cell holds the view's end (now while following).
pub(super) fn start_ms(app: &App, bucket: i64, n: usize) -> i64 {
    let end = if app.follow { app.now_ms } else { app.view_end_ms };
    (end.div_euclid(bucket) + 1) * bucket - n as i64 * bucket
}

fn cell(lane: &Lane, t0: i64, t1: i64, now_ms: i64, orient: Orient) -> Cell {
    let lo = lane.steps.partition_point(|s| s.at_ms < t0);
    let hi = lane.steps.partition_point(|s| s.at_ms < t1);
    if let Some(node) = node_cell(&lane.steps[lo..hi]) {
        return Some(node);
    }
    if lane.is_open() && (t0..t1).contains(&now_ms) {
        return head_cell(lane.status, orient);
    }
    if t0 > now_ms {
        return None;
    }
    status_at(lane, t1 - 1).and_then(|s| line_cell(s, orient))
}

/// Cells for `lane`, each at the context in use at the end of its bucket (or now) across a band
/// `band` rows tall.
#[allow(clippy::too_many_arguments)]
pub(super) fn pts(
    lane: &Lane,
    store: &Store,
    scale: Scale,
    band: u16,
    start: i64,
    bucket: i64,
    n: usize,
    now_ms: i64,
    orient: Orient,
) -> Vec<Pt> {
    (0..n)
        .map(|i| {
            let t0 = start + i as i64 * bucket;
            let t1 = t0 + bucket;
            let at = (t1 - 1).min(now_ms);
            Pt::new(cell(lane, t0, t1, now_ms, orient), scale.level(store.ctx_at(lane, at), band), status_at(lane, at))
        })
        .collect()
}

/// Axis labels sit on absolute bucket multiples so they scroll with time.
pub(super) fn draw_axis(buf: &mut Buffer, grid: &Grid, start: i64, bucket: i64) {
    let horizontal = grid.orient == Orient::Horizontal;
    let dim = Style::new().fg(theme().dim);
    let area = grid.axis_area();
    for (i, label) in time_ticks(start, bucket as f64, grid.n(), horizontal) {
        let (x, y) = grid.axis_pos(i);
        put_str(buf, area, x, y, &if horizontal { format!("┊{label}") } else { label }, dim);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{col, key, render, row, sample_app};
    use super::*;

    #[test]
    fn horizontal_lane_ends_at_now_on_the_far_edge() {
        let mut app = sample_app(1, Orient::Horizontal);
        // Header, axis row, lane row. 29 strip cells from column 33, 1 s buckets fit 8.5 s.
        let buf = render(&mut app, 80, 3);
        assert_eq!(row(&buf, 2, 33, 62).trim_start(), "◆─●━━▲╍╍▸");
        assert_eq!(buf[(61, 2)].symbol(), "▸", "now sits in the last cell");
        assert_eq!(buf[(61, 2)].fg, theme().blocked, "head in the current status color");
        assert_eq!(buf[(54, 2)].fg, theme().idle, "lines take their status color");
        assert_eq!(buf[(55, 2)].fg, theme().working, "status nodes keep status colors");
    }

    #[test]
    fn vertical_lane_grows_downward() {
        let mut app = sample_app(1, Orient::Vertical);
        // Header, column header row, 14 cells of 1 s: now (8.5 s) in the last.
        let buf = render(&mut app, axis_w(1000) + 6, 16);
        let cells = col(&buf, axis_w(1000) + 2, 7, 16);
        assert_eq!(cells, ["◆", "│", "●", "┃", "┃", "▲", "╏", "╏", "▾"]);
        assert_eq!(buf[(axis_w(1000) + 2, 6)].symbol(), " ", "nothing before the lane opened");
    }

    #[test]
    fn vertical_axis_narrows_for_minute_ticks() {
        assert_eq!(axis_w(1000), 9);
        assert_eq!(axis_w(15_000), 6);
    }

    /// Context 100 from the start, 400 from 4000 ms: the line bends up a level per cell of band.
    fn with_usage(orient: Orient) -> crate::view::App {
        let mut app = sample_app(1, orient);
        let id = app.state.lanes[0].id;
        app.sessions.insert_samples(id, &[(0, 100, Some(0.25)), (4000, 400, Some(0.5))]);
        app
    }

    #[test]
    fn horizontal_band_rises_with_context() {
        let mut app = with_usage(Orient::Horizontal);
        // Header, axis row, 4-row band from row 2; the lane starts at strip cell 20 (column 53).
        let buf = render(&mut app, 80, 7);
        assert_eq!(row(&buf, 2, 53, 62), "    ┏▲╍╍▸", "level 3: 400 of 400");
        assert_eq!(row(&buf, 3, 53, 62), "    ┃");
        assert_eq!(row(&buf, 4, 53, 62), "◆─●━┛", "level 1: 100 of 400");
        assert_eq!(row(&buf, 3, 3, 33), "ctx 400 · peak 400");
        assert_eq!(row(&buf, 4, 3, 33), "$0.75 · 2 reqs");
        assert_eq!(row(&buf, 5, 3, 33), "omp · w1:p1");
        let wide = render(&mut app, 120, 7);
        assert!(row(&wide, 0, 0, 120).contains("ctx≤400"), "{}", row(&wide, 0, 0, 120));

        key(&mut app, 'Z');
        let buf = render(&mut app, 80, 7);
        assert_eq!(row(&buf, 2, 53, 62), "◆─●━━▲╍╍▸", "compact density flattens the bands");
    }

    #[test]
    fn bands_collapse_when_they_dont_fit() {
        let mut app = with_usage(Orient::Horizontal);
        // Header, axis row and three lane rows: a 4-row band doesn't fit.
        let buf = render(&mut app, 80, 5);
        assert_eq!(row(&buf, 2, 53, 62), "◆─●━━▲╍╍▸");
        assert_eq!(app.band_max, None);
    }

    #[test]
    fn vertical_lanes_never_band() {
        let mut app = with_usage(Orient::Vertical);
        let buf = render(&mut app, axis_w(1000) + 6, 16);
        let cells = col(&buf, axis_w(1000) + 2, 7, 16);
        assert_eq!(cells, ["◆", "│", "●", "┃", "┃", "▲", "╏", "╏", "▾"], "the line keeps its column");
    }
}
