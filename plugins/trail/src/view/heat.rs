//! Heat lens: one cell per time bucket, shaded along the theme's ramp (the same stretch agent
//! colors use) by the share of the bucket the agent spent working; `heat_blocked` where it was
//! blocked at all. Cells are half-height `▀` in ↔ so neighboring rows stay apart, and fill the
//! column in ↕. The key lives in the header.

use super::{Orient, Pt};
use crate::model::{AgentStatus, Lane, status_time};
use crate::theme::theme;

pub(super) fn pts(lane: &Lane, start: i64, bucket: i64, n: usize, now_ms: i64, orient: Orient) -> Vec<Pt> {
    let glyph = match orient {
        Orient::Horizontal => '▀',
        Orient::Vertical => '█',
    };
    (0..n)
        .map(|i| {
            let t0 = start + i as i64 * bucket;
            let now_cell = lane.is_open() && (t0..t0 + bucket).contains(&now_ms);
            let st = (t0 <= now_ms).then(|| status_time(lane, t0, (t0 + bucket).min(now_ms + 1)));
            let color = match st {
                Some(st) if st.blocked_ms > 0 => Some(theme().heat_blocked),
                Some(st) if st.alive_ms > 0 => Some(theme().heat_color(st.working_ms as f64 / st.alive_ms as f64)),
                // A lane first seen just after the frame's clock still shows what it is doing now.
                _ if now_cell => Some(match lane.status {
                    AgentStatus::Working => theme().heat_color(1.0),
                    AgentStatus::Blocked => theme().heat_blocked,
                    _ => theme().heat_color(0.0),
                }),
                _ => None,
            };
            Pt { fill: orient == Orient::Vertical, ..Pt::new(color.map(|c| (glyph, c)), 0, None) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::tests::{render, row, sample_app};
    use super::*;
    use crate::model::Mark;

    #[test]
    fn cells_shade_by_working_share() {
        // Idle from 0, working 2000..5000, blocked from 5000; now 8500; 1 s buckets, now in the last
        // of 29 cells from column 33, so the lane opens at column 53.
        let mut app = sample_app(4, Orient::Horizontal);
        let buf = render(&mut app, 80, 3);
        assert_eq!(row(&buf, 2, 53, 62), "▀▀▀▀▀▀▀▀▀", "one cell per second up to now");
        let fg = |i: u16| buf[(53 + i, 2)].fg;
        assert_eq!(fg(0), theme().heat_color(0.0), "idle");
        assert_eq!(fg(3), theme().heat_color(1.0), "working the whole second");
        assert_eq!(fg(6), theme().heat_blocked, "blocked");
    }

    #[test]
    fn heat_paints_now_for_a_first_seen_working_lane() {
        let mut app = sample_app(4, Orient::Horizontal);
        let i = app.state.open("w1:p2", "w1", 8550, Mark::Seen, AgentStatus::Working, true);
        app.state.lanes[i].agent = Some("omp".into());
        app.state.assign_slots();
        let buf = render(&mut app, 80, 4);
        assert_eq!(buf[(61, 3)].symbol(), "▀");
        assert_eq!(buf[(61, 3)].fg, theme().heat_color(1.0));
    }

    #[test]
    fn vertical_cells_fill_the_column() {
        let mut app = sample_app(4, Orient::Vertical);
        let buf = render(&mut app, 15, 16);
        // One lane: a 6-wide column from x 9, filled after its first cell; the last row is now.
        assert_eq!(row(&buf, 15, 10, 15), "█████");
    }
}
