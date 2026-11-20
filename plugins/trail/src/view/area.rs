//! USAGE: context in use per agent over time, as an area chart. Each agent is a series in its
//! palette color: a braille line (2×4 dots per cell) over a tinted fill. Stacked, each series sits
//! on the ones below and the top line is the total; overlaid (`v`), every series starts at zero
//! with the largest at the back. Round value ticks, a time axis with compaction marks, and labels
//! at each line's end frame it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::time::start_ms;
use super::{App, AreaMode, Orient, put_str, time_ticks, truncate};
use crate::config::history_ms;
use crate::model::Lane;
use crate::session::{Store, fmt_tokens};
use crate::theme::{mix, theme};

/// ↕ time-axis gutter: `HH:MM:SS` plus a space.
const AXIS_W: u16 = 9;
/// ↔ value-axis gutter (`530k┤`) and end-label margin.
const VAX_W: u16 = 6;
const LABEL_W: u16 = 16;
/// ↕ rows under the chart for end labels, one per series.
const END_ROWS: u16 = 4;

/// Fill where the series with palette `slots` cover a cell, back to front: each lays its color at
/// the theme's `fill_alpha` over what is behind it, so overlapping areas show through each other.
fn fill(slots: impl IntoIterator<Item = usize>) -> Option<Color> {
    let t = theme();
    slots.into_iter().fold(None, |behind, s| Some(mix(behind.unwrap_or(t.background), t.agent_color(s), t.fill_alpha)))
}

fn fill_color(slot: usize) -> Color {
    fill([slot]).unwrap_or(Color::Reset)
}

/// Smallest 1, 1.5, 2, 2.5, 3, 4, 5, 6 or 8 × 10ⁿ at or above `v`.
fn nice_ceil(v: u64) -> u64 {
    if v == 0 {
        return 0;
    }
    let mag = 10f64.powi((v as f64).log10().floor() as i32);
    [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0].iter().map(|m| (m * mag).round() as u64).find(|&n| n >= v).unwrap_or(v)
}

/// First two significant digits of `v`.
fn leading(v: u64) -> u64 {
    let mut v = v;
    while v >= 100 {
        v /= 10;
    }
    if v < 10 { v * 10 } else { v }
}

/// Value ticks from 0 to `max` over `rows` cells: the most even steps (up to `rows / 3`, 1..4)
/// that divide `max` into round numbers; else just the ends.
fn ticks(max: u64, rows: u16) -> Vec<u64> {
    let target = (rows / 3).clamp(1, 4) as u64;
    let n = (1..=target)
        .rev()
        .find(|&n| max.is_multiple_of(n) && [10, 15, 20, 25, 30, 40, 50, 60, 75, 80].contains(&leading(max / n)));
    match n {
        Some(n) => (0..=n).map(|i| max / n * i).collect(),
        None => vec![0, max],
    }
}

/// Braille cells: per-cell dot mask, line color and fill color.
struct Canvas {
    cols: usize,
    rows: usize,
    mask: Vec<u8>,
    fg: Vec<Option<Color>>,
    bg: Vec<Option<Color>>,
}

impl Canvas {
    fn new(cols: usize, rows: usize) -> Self {
        Canvas { cols, rows, mask: vec![0; cols * rows], fg: vec![None; cols * rows], bg: vec![None; cols * rows] }
    }

    /// Lights screen dot (sx, sy): 2 dots across, 4 down per cell.
    fn dot(&mut self, sx: usize, sy: usize, color: Color) {
        let (c, r) = (sx / 2, sy / 4);
        if c >= self.cols || r >= self.rows {
            return;
        }
        const BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        let i = r * self.cols + c;
        self.mask[i] |= BITS[sx % 2][sy % 4];
        self.fg[i] = Some(color);
    }
}

/// One agent's values per time dot (None outside its life or before any usage).
struct Series {
    /// Row index, for labels and selection.
    k: usize,
    /// Agent palette slot, for colors.
    slot: usize,
    vals: Vec<Option<u64>>,
}

/// Chart geometry in one orientation: time and value axes in dots, and where the chart sits.
struct Plot {
    orient: Orient,
    area: Rect,
}

impl Plot {
    fn t_dots(&self) -> usize {
        match self.orient {
            Orient::Horizontal => self.area.width as usize * 2,
            Orient::Vertical => self.area.height as usize * 4,
        }
    }

    fn v_dots(&self) -> usize {
        match self.orient {
            Orient::Horizontal => self.area.height as usize * 4,
            Orient::Vertical => self.area.width as usize * 2,
        }
    }

    /// Screen dot for time dot `t`, value dot `v` (0 = baseline).
    fn screen(&self, t: usize, v: usize) -> (usize, usize) {
        match self.orient {
            Orient::Horizontal => (t, self.v_dots() - 1 - v.min(self.v_dots() - 1)),
            Orient::Vertical => (v, t),
        }
    }

    /// Chart-space dot at the center of cell (col, row).
    fn cell_center(&self, col: usize, row: usize) -> (usize, usize) {
        let (sx, sy) = (col * 2 + 1, row * 4 + 2);
        match self.orient {
            Orient::Horizontal => (sx, self.v_dots() - 1 - sy),
            Orient::Vertical => (sy, sx),
        }
    }

    /// Terminal position of the cell holding screen dot (sx, sy).
    fn cell_pos(&self, sx: usize, sy: usize) -> (u16, u16) {
        (self.area.x + (sx / 2) as u16, self.area.y + (sy / 4) as u16)
    }
}

/// First ms the chart covers, and ms per time dot. Fitting while following stretches the history
/// (capped at the history window) across the whole time axis; otherwise LANES' buckets apply.
fn time_frame(app: &App, lanes: &[usize], t_dots: usize, dots_per_cell: usize) -> (i64, i64) {
    let now = app.now_ms;
    if app.bucket_auto && app.follow {
        let earliest = lanes
            .iter()
            .map(|&i| {
                let l = &app.state.lanes[i];
                app.sessions.first_at(l).map_or(l.opened_ms, |f| f.min(l.opened_ms))
            })
            .min()
            .unwrap_or(now)
            .max(now - history_ms());
        let dot_ms = ((now - earliest) / t_dots.max(1) as i64 + 1).max(1000);
        return (earliest, dot_ms);
    }
    let bucket = app.bucket_ms();
    let start = start_ms(app, bucket, t_dots / dots_per_cell);
    (start, (bucket / dots_per_cell as i64).max(1))
}

/// Draws a line through one dot per time dot, joined across steps so it stays continuous; `None`
/// breaks it.
fn trace(canvas: &mut Canvas, plot: &Plot, pts: &[Option<(usize, Color)>]) {
    let mut prev: Option<usize> = None;
    for (t, pt) in pts.iter().enumerate() {
        let Some((d, color)) = *pt else {
            prev = None;
            continue;
        };
        let (lo, hi) = prev.map_or((d, d), |p| (p.min(d), p.max(d)));
        for v in lo..=hi {
            let (sx, sy) = plot.screen(t, v);
            canvas.dot(sx, sy, color);
        }
        prev = Some(d);
    }
}

/// Context of `lane` at `at`; None outside its life.
fn value_at(lane: &Lane, store: &Store, at: i64) -> Option<u64> {
    if at < lane.opened_ms || lane.closed_ms.is_some_and(|c| at >= c) {
        return None;
    }
    store.ctx_at(lane, at)
}

pub fn render(buf: &mut Buffer, body: Rect, app: &mut App, lanes: &[usize]) {
    let horizontal = app.orient == Orient::Horizontal;
    let dim = Style::new().fg(theme().dim);
    // ↔: chart, time-axis row; value axis left, end labels right.
    // ↕: value-axis row, chart, one end-label row per series (up to four); time axis left.
    let label_w = if horizontal && body.width >= 40 { LABEL_W } else { 0 };
    let end_rows = if horizontal { 0 } else { (lanes.len() as u16).min(END_ROWS) };
    let plot = if horizontal {
        Rect::new(body.x + VAX_W, body.y, body.width.saturating_sub(VAX_W + label_w), body.height.saturating_sub(1))
    } else {
        Rect::new(body.x + AXIS_W, body.y + 1, body.width.saturating_sub(AXIS_W), body.height.saturating_sub(1 + end_rows))
    };
    let plot = Plot { orient: app.orient, area: plot };
    let (t_dots, v_dots) = (plot.t_dots(), plot.v_dots());
    let dots_per_cell = if horizontal { 2 } else { 4 };
    app.time_cells = (t_dots / dots_per_cell).max(1);
    if t_dots == 0 || v_dots == 0 {
        return;
    }
    let (start, dot_ms) = time_frame(app, lanes, t_dots, dots_per_cell);
    let now = app.now_ms;

    let mut series: Vec<Series> = lanes
        .iter()
        .enumerate()
        .map(|(k, &li)| {
            let lane = &app.state.lanes[li];
            let vals = (0..t_dots)
                .map(|j| {
                    let t0 = start + j as i64 * dot_ms;
                    (t0 <= now).then(|| value_at(lane, &app.sessions, (t0 + dot_ms - 1).min(now))).flatten()
                })
                .collect();
            Series { k, slot: lane.slot.unwrap_or(0), vals }
        })
        .collect();
    let mode = app.area_mode;
    if mode == AreaMode::Overlaid {
        // Back to front: the largest series behind, so smaller ones stay visible.
        series.sort_by_key(|s| std::cmp::Reverse(s.vals.iter().flatten().max().copied().unwrap_or(0)));
    }
    // Each series' band per time dot, in tokens: stacked on the ones before it, or from zero.
    let bands: Vec<Vec<Option<(f64, f64)>>> = {
        let mut cum = vec![0.0; t_dots];
        series
            .iter()
            .map(|s| {
                s.vals
                    .iter()
                    .enumerate()
                    .map(|(t, v)| {
                        v.map(|v| match mode {
                            AreaMode::Overlaid => (0.0, v as f64),
                            AreaMode::Stacked => {
                                let lo = cum[t];
                                cum[t] += v as f64;
                                (lo, cum[t])
                            }
                        })
                    })
                    .collect()
            })
            .collect()
    };
    let max = nice_ceil(bands.iter().flatten().flatten().map(|b| b.1 as u64).max().unwrap_or(0));
    if max == 0 {
        put_str(buf, body, plot.area.x, plot.area.y, "no usage yet — no agent session reports its context", dim);
        return;
    }
    let to_dot = |x: f64| ((x / max as f64) * (v_dots - 1) as f64).round().clamp(0.0, (v_dots - 1) as f64) as usize;

    let mut canvas = Canvas::new(plot.area.width as usize, plot.area.height as usize);
    // Fill from the series whose bands hold each cell's center. Stacked bands don't overlap, so
    // the first (lowest) owns the cell; overlaid areas all show, blended back to front.
    for row in 0..canvas.rows {
        for col in 0..canvas.cols {
            let (t, v) = plot.cell_center(col, row);
            let mut covering =
                bands.iter().enumerate().filter(|(_, b)| b[t].is_some_and(|(lo, hi)| to_dot(lo) <= v && v <= to_dot(hi)));
            canvas.bg[row * canvas.cols + col] = match mode {
                AreaMode::Overlaid => fill(covering.map(|(i, _)| series[i].slot)),
                AreaMode::Stacked => covering.next().map(|(i, _)| fill_color(series[i].slot)),
            };
        }
    }
    for (i, s) in series.iter().enumerate() {
        let color = theme().agent_color(s.slot);
        let edge: Vec<Option<(usize, Color)>> = bands[i].iter().map(|b| b.map(|(_, hi)| (to_dot(hi), color))).collect();
        trace(&mut canvas, &plot, &edge);
    }

    let value_cells = if horizontal { plot.area.height } else { plot.area.width / 2 };
    let ticks = ticks(max, value_cells);
    // Gridlines at the ticks, behind lines; then the canvas.
    for (row, col) in (0..canvas.rows).flat_map(|r| (0..canvas.cols).map(move |c| (r, c))) {
        let i = row * canvas.cols + col;
        let (x, y) = (plot.area.x + col as u16, plot.area.y + row as u16);
        let bg = canvas.bg[i].unwrap_or(Color::Reset);
        let cell = &mut buf[(x, y)];
        if canvas.mask[i] != 0 {
            let ch = char::from_u32(0x2800 + u32::from(canvas.mask[i])).unwrap_or(' ');
            cell.set_char(ch).set_style(Style::new().fg(canvas.fg[i].unwrap_or(Color::Reset)).bg(bg));
            continue;
        }
        let (t, _) = plot.cell_center(col, row);
        let on_tick = ticks.iter().any(|&tk| {
            let d = to_dot(tk as f64);
            match plot.orient {
                Orient::Horizontal => plot.screen(t, d).1 / 4 == row,
                Orient::Vertical => plot.screen(t, d).0 / 2 == col,
            }
        });
        let grid = match plot.orient {
            Orient::Horizontal => '┈',
            Orient::Vertical => '┆',
        };
        cell.set_char(if on_tick { grid } else { ' ' }).set_style(Style::new().fg(theme().grid).bg(bg));
    }

    // Value axis: right-aligned labels with a tick into the chart (↔), or centered above it (↕),
    // where a label that would touch the one before it is left out.
    let mut free_x = body.x;
    for &tk in &ticks {
        let text = fmt_tokens(tk);
        let (sx, sy) = plot.screen(0, to_dot(tk as f64));
        match plot.orient {
            Orient::Horizontal => {
                let y = plot.cell_pos(sx, sy).1;
                put_str(buf, body, body.x, y, &format!("{text:>w$}┤", w = (VAX_W - 1) as usize), dim);
            }
            Orient::Vertical => {
                let len = text.chars().count() as u16;
                let x = plot.cell_pos(sx, sy).0.saturating_sub(len / 2).max(plot.area.x);
                let x = x.min(body.right().saturating_sub(len));
                if x < free_x {
                    continue;
                }
                put_str(buf, body, x, body.y, &text, dim);
                free_x = x + len + 1;
            }
        }
    }

    // Time axis, with a `⇣` wherever an agent's context was compacted.
    let cells = t_dots / dots_per_cell;
    for (c, label) in time_ticks(start, (dot_ms * dots_per_cell as i64) as f64, cells, horizontal) {
        match plot.orient {
            Orient::Horizontal => put_str(buf, body, plot.area.x + c as u16, body.bottom() - 1, &format!("┊{label}"), dim),
            Orient::Vertical => put_str(buf, body, body.x, plot.area.y + c as u16, &label, dim),
        };
    }
    let cell_ms = dot_ms * dots_per_cell as i64;
    for &li in lanes {
        let lane = &app.state.lanes[li];
        let color = Style::new().fg(theme().agent_color(lane.slot.unwrap_or(0)));
        for (at, _, _) in app.sessions.compactions(lane) {
            if at < start || at >= start + cells as i64 * cell_ms {
                continue;
            }
            let c = ((at - start) / cell_ms) as u16;
            match plot.orient {
                Orient::Horizontal => put_str(buf, body, plot.area.x + c, body.bottom() - 1, "⇣", color),
                Orient::Vertical => put_str(buf, body, body.x + AXIS_W - 1, plot.area.y + c, "⇣", color),
            };
        }
    }

    // End labels: beside each line's last point (↔), or one row each under the chart (↕).
    let sel = app.selected_lane();
    let label_style = |k: usize, slot: usize| {
        let s = Style::new().fg(theme().agent_color(slot));
        if sel == Some(lanes[k]) { s.add_modifier(Modifier::UNDERLINED | Modifier::BOLD) } else { s }
    };
    let last_t = (0..t_dots).rev().find(|&t| bands.iter().any(|b| b[t].is_some()));
    let mut ends: Vec<(u16, usize, usize, u64)> = last_t
        .map(|last_t| {
            series
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    let (_, hi) = bands[i][last_t]?;
                    let (sx, sy) = plot.screen(last_t, to_dot(hi));
                    Some((plot.cell_pos(sx, sy).1, s.k, s.slot, s.vals[last_t].unwrap_or(0)))
                })
                .collect()
        })
        .unwrap_or_default();
    match plot.orient {
        Orient::Horizontal if label_w > 0 => {
            ends.sort_by_key(|e| e.0);
            let mut used: Vec<u16> = Vec::new();
            let x = plot.area.right() + 1;
            for (y, k, slot, val) in ends {
                let mut y = y;
                while used.contains(&y) && y + 1 < plot.area.bottom() {
                    y += 1;
                }
                used.push(y);
                let ctx = fmt_tokens(val);
                let name = truncate(app.state.lanes[lanes[k]].label(), (LABEL_W as usize - 2).saturating_sub(ctx.len()));
                put_str(buf, body, x, y, &format!("{name} {ctx}"), label_style(k, slot));
            }
            let quiet: Vec<&str> = series
                .iter()
                .filter(|s| s.vals.iter().all(Option::is_none))
                .map(|s| app.state.lanes[lanes[s.k]].label())
                .collect();
            if !quiet.is_empty() {
                let text = truncate(&format!("no usage: {}", quiet.join(", ")), LABEL_W as usize - 1);
                put_str(buf, body, x, plot.area.bottom() - 1, &text, dim);
            }
        }
        Orient::Vertical => {
            ends.sort_by_key(|e| e.1);
            for (row, (_, k, slot, val)) in ends.into_iter().take(end_rows as usize).enumerate() {
                let text = format!("■ {} {}", app.state.lanes[lanes[k]].label(), fmt_tokens(val));
                let text = truncate(&text, body.width as usize);
                put_str(buf, body, body.x, plot.area.bottom() + row as u16, &text, label_style(k, slot));
            }
        }
        Orient::Horizontal => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{key, render, row, sample_app};
    use super::*;
    use crate::model::{AgentStatus, Mark};

    /// Lane 0 at 100 tokens and lane 1 at 300, both from 0 ms; `now_ms = 8500`.
    fn two_series(orient: Orient) -> App {
        let mut app = sample_app(3, orient);
        let i = app.state.open("w1:p2", "w1", 0, Mark::Created, AgentStatus::Working, true);
        app.state.lanes[i].agent = Some("claude".into());
        app.state.assign_slots();
        for (li, ctx) in [(0, 100), (i, 300)] {
            let id = app.state.lanes[li].id;
            app.sessions.insert_samples(id, &[(0, ctx, None)]);
        }
        app
    }

    fn braille(s: &str) -> bool {
        s.chars().next().is_some_and(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
    }

    #[test]
    fn nice_ceil_rounds_up_to_readable_maxima() {
        assert_eq!(nice_ceil(400), 400);
        assert_eq!(nice_ceil(401), 500);
        assert_eq!(nice_ceil(527_000), 600_000);
        assert_eq!(nice_ceil(130_000), 150_000);
        assert_eq!(nice_ceil(0), 0);
    }

    #[test]
    fn ticks_are_round() {
        assert_eq!(ticks(150_000, 8), [0, 75_000, 150_000]);
        assert_eq!(ticks(400_000, 12), [0, 100_000, 200_000, 300_000, 400_000]);
        assert_eq!(ticks(300_000, 3), [0, 300_000]);
    }

    /// Body is rows 1..=9 of a 10-row frame: chart rows 1..=8 (32 value dots), time axis row 9.
    #[test]
    fn stacked_series_sit_on_each_other() {
        let mut app = two_series(Orient::Horizontal);
        let buf = render(&mut app, 60, 10);
        let x = VAX_W + 2;
        // max 400 over 31 dots: lane 0's top at dot 8 (row 6), the total at dot 31 (row 1).
        assert!(braille(buf[(x, 1)].symbol()), "total line on the top row");
        assert_eq!(buf[(x, 1)].fg, theme().agent_color(1));
        assert!(braille(buf[(x, 6)].symbol()), "lane 0's line at a quarter height");
        assert_eq!(buf[(x, 6)].fg, theme().agent_color(0));
        assert_eq!(buf[(x, 8)].bg, fill_color(0), "lane 0 fills the base");
        assert_eq!(buf[(x, 3)].bg, fill_color(1), "lane 1 fills above it");
        assert_eq!(buf[(0, 1)].symbol().trim(), "", "value labels are right-aligned");
        assert!((0..VAX_W).map(|c| buf[(c, 1)].symbol()).collect::<String>().contains("400"));
        let label = row(&buf, 1, 60 - LABEL_W + 1, 60);
        assert_eq!(label, "claude 300", "each line ends in its own context");
    }

    #[test]
    fn overlaid_series_start_at_zero_with_largest_behind() {
        let mut app = two_series(Orient::Horizontal);
        key(&mut app, 'v');
        assert_eq!(app.area_mode, AreaMode::Overlaid);
        let buf = render(&mut app, 60, 10);
        let x = VAX_W + 2;
        // max 300: lane 1's line on top, lane 0's at a third. Below lane 0's line both areas
        // overlap, lane 0's tint over lane 1's; above it only lane 1's.
        assert_eq!(buf[(x, 1)].fg, theme().agent_color(1));
        let both = fill([1, 0]).unwrap();
        assert_eq!(buf[(x, 8)].bg, both, "overlap blends, smaller series in front");
        assert_ne!(both, fill_color(0));
        assert_ne!(both, fill_color(1));
        assert_eq!(buf[(x, 3)].bg, fill_color(1));
    }

    #[test]
    fn series_without_usage_are_named() {
        let mut app = two_series(Orient::Horizontal);
        let i = app.state.open("w1:p3", "w1", 0, Mark::Created, AgentStatus::Idle, true);
        app.state.lanes[i].agent = Some("codex".into());
        app.state.assign_slots();
        let buf = render(&mut app, 60, 10);
        assert_eq!(row(&buf, 8, 60 - LABEL_W + 1, 60), "no usage: codex");
    }

    #[test]
    fn fit_spreads_history_across_the_axis() {
        let mut app = two_series(Orient::Horizontal);
        let buf = render(&mut app, 60, 10);
        // 8.5 s over 76 dots: the 1 s floor applies, so lines end around dot 8 (cell 4).
        let last = (VAX_W..60).rev().find(|&x| braille(buf[(x, 1)].symbol())).unwrap();
        assert_eq!(last, VAX_W + 4);
        app.now_ms = 200_000;
        let buf = render(&mut app, 60, 10);
        let last = (VAX_W..60 - LABEL_W).rev().find(|&x| braille(buf[(x, 1)].symbol())).unwrap();
        assert!(last >= 60 - LABEL_W - 2, "a long history reaches the right edge, got {last}");
    }

    #[test]
    fn vertical_values_grow_rightward() {
        let mut app = two_series(Orient::Vertical);
        let buf = render(&mut app, AXIS_W + 8, 11);
        // Value-axis row 1, chart rows 2..=8, an end label per series on rows 9 and 10.
        assert_eq!(buf[(AXIS_W, 2)].bg, fill_color(0));
        let right = (AXIS_W..AXIS_W + 8).rev().find(|&x| braille(buf[(x, 2)].symbol())).unwrap();
        assert_eq!(buf[(right, 2)].fg, theme().agent_color(1));
        assert_eq!(right, AXIS_W + 7);
        assert!(row(&buf, 9, 0, AXIS_W + 8).starts_with("■ omp 100"), "{}", row(&buf, 9, 0, AXIS_W + 8));
        assert!(row(&buf, 10, 0, AXIS_W + 8).starts_with("■ claude"), "{}", row(&buf, 10, 0, AXIS_W + 8));
    }
}
