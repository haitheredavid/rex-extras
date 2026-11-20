//! LANES: one lane per agent through the selected lens, on the time axis (status, heat, ctx) or the
//! steps axis (status only), with group header rows between them. A blocked agent the rows can't
//! show gets a sticky line at the top.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::gutter::Cols;
use super::layout::SizeClass;
use super::time::{axis_w, draw_axis, start_ms};
use super::{
    App, Axis, BAND, Density, Grid, Hit, Item, Lens, Orient, Scale, blocked_lanes, ctx, fmt_dur, heat, lanes_of, put_str,
    row_height, sequence, time,
};
use crate::model::{Group, Row, RowKey};
use crate::theme::theme;

/// Gutter columns for a frame `width` wide in the app's layout.
pub(super) fn cols_for(app: &App, width: u16) -> Cols {
    Cols::fit(width, app.class, !app.headers && app.query.group != Group::None)
}

/// The grid for `rows` in `body`: lead, row heights (bands, or a sidebar's two-row lanes).
fn grid(app: &App, body: Rect, rows: &[Row]) -> Grid {
    let horizontal = app.orient == Orient::Horizontal;
    let lead = match (!app.steps(), horizontal) {
        (false, _) => 0,
        (true, true) => 1,
        (true, false) => axis_w(app.bucket_ms()),
    };
    let space = body.height.saturating_sub(lead) as usize;
    let lanes = rows.iter().filter(|r| matches!(r, Row::Lane(_))).count();
    let groups = rows.len() - lanes;
    let two_row = horizontal
        && app.class == SizeClass::Sidebar
        && match app.density {
            Density::Roomy => true,
            Density::Compact => false,
            Density::Auto => lanes * 2 + groups <= space,
        };
    let all_fit = lanes * usize::from(BAND + 1) + groups <= space + 1;
    let items = rows
        .iter()
        .map(|r| match *r {
            Row::Lane(li) => {
                Item { lane: Some(li), height: if two_row { 2 } else { row_height(app, &app.state.lanes[li], all_fit) } }
            }
            _ => Item { lane: None, height: 1 },
        })
        .collect();
    Grid::new(app.orient, body, lead, items, cols_for(app, body.width), two_row)
}

pub fn render(buf: &mut Buffer, body: Rect, app: &mut App, rows: &[Row]) {
    let horizontal = app.orient == Orient::Horizontal;
    // Time-down columns ignore groups.
    let rows: Vec<Row> =
        if horizontal { rows.to_vec() } else { rows.iter().filter(|r| matches!(r, Row::Lane(_))).cloned().collect() };
    let lanes = lanes_of(&rows);
    if !app.steps() {
        let n = grid(app, body, &rows).n();
        app.fit_bucket(&lanes, n);
    }

    // A blocked agent scrolled out of view gets the top row.
    let mut body = body;
    let trial = grid(app, body, &rows);
    let placed = trial.placed(trial.scroll_for(app.lane_scroll, app.sel_row(&rows).unwrap_or(0)));
    let hidden = blocked_lanes(app).into_iter().find(|&li| !placed.iter().any(|&(k, _)| rows[k] == Row::Lane(li)));
    if let Some(li) = hidden.filter(|_| horizontal && body.height > 2) {
        let lane = &app.state.lanes[li];
        let text = format!("▲ {} · blocked {} · b", lane.label(), fmt_dur(app.now_ms - lane.last_ms()));
        put_str(buf, body, body.x, body.y, &text, Style::new().fg(theme().blocked).add_modifier(Modifier::BOLD));
        app.hits.push((Rect { height: 1, ..body }, Hit::Row(RowKey::Lane(lane.id))));
        body = Rect { y: body.y + 1, height: body.height - 1, ..body };
    }

    let grid = grid(app, body, &rows);
    let scale = Scale::of(app, &lanes);
    if grid.items.iter().any(|i| i.height > 1 && !grid.two_row) {
        app.band_max = Some(scale.max);
    }
    let bucket = app.bucket_ms();
    let n = grid.n();
    app.time_cells = n.max(1);
    let start = start_ms(app, bucket, n);
    if !app.steps() {
        draw_axis(buf, &grid, start, bucket);
    }

    let (now, orient, back) = (app.now_ms, app.orient, app.seq_back);
    match (app.lens, app.axis) {
        (Lens::Status, Axis::Time) => {
            grid.draw_lanes(buf, app, &rows, |lane, s, n, band| time::pts(lane, s, scale, band, start, bucket, n, now, orient))
        }
        (Lens::Status, Axis::Steps) => {
            grid.draw_lanes(buf, app, &rows, |lane, s, n, band| sequence::pts(lane, s, scale, band, now, n, back, orient))
        }
        (Lens::Heat, _) => grid.draw_lanes(buf, app, &rows, |lane, _, n, _| heat::pts(lane, start, bucket, n, now, orient)),
        (Lens::Ctx, _) => {
            let max = ctx::max(app, &lanes, start, start + n as i64 * bucket);
            grid.draw_lanes(buf, app, &rows, |lane, s, n, _| ctx::pts(lane, s, max, start, bucket, n, now));
            ctx::totals(buf, &grid, app, &lanes);
        }
    }
}
