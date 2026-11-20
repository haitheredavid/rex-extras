//! Status lens on the steps axis: position is the step index, not time, so every lane starts at
//! the near edge and each state change gets the same room.

use super::{Orient, Pt, Scale, head_cell, line_cell, node_glyph};
use crate::model::Lane;
use crate::session::Store;
use crate::theme::theme;

/// Cells per step: the node, then connectors in the status it set.
const STEP_W: usize = 4;
const STEP_H: usize = 2;

/// Cells one step takes along the lane.
pub(super) fn step_cells(orient: Orient) -> usize {
    match orient {
        Orient::Horizontal => STEP_W,
        Orient::Vertical => STEP_H,
    }
}

/// Step k's node and connectors sit at the context in use when it happened; the head at now's.
#[allow(clippy::too_many_arguments)]
pub(super) fn pts(
    lane: &Lane,
    store: &Store,
    scale: Scale,
    band: u16,
    now_ms: i64,
    n: usize,
    seq_back: usize,
    orient: Orient,
) -> Vec<Pt> {
    let step = step_cells(orient);
    let len = lane.steps.len();
    if len == 0 {
        return Vec::new();
    }
    let back = seq_back.min(len - 1);
    let end = len - back;
    let head = lane.is_open() && back == 0;
    // A closed lane ends at its Closed/Exited node; every other step trails connectors.
    let width = |k: usize| if k == len - 1 && !lane.is_open() { 1 } else { step };
    let need = |s: usize| (s..end).map(width).sum::<usize>() + usize::from(head) + usize::from(s > 0);
    let first = (0..end).find(|&s| need(s) <= n).unwrap_or(end - 1);

    let level_at = |t_ms: i64| scale.level(store.ctx_at(lane, t_ms), band);
    let mut out: Vec<Pt> = Vec::with_capacity(n);
    if first > 0 {
        let s = &lane.steps[first];
        out.push(Pt::new(Some(('…', theme().dim)), level_at(s.at_ms), Some(s.status)));
    }
    for (k, s) in lane.steps.iter().enumerate().take(end).skip(first) {
        let (level, status) = (level_at(s.at_ms), Some(s.status));
        out.push(Pt::new(Some(node_glyph(s.mark)), level, status));
        if width(k) > 1 {
            out.extend(std::iter::repeat_n(Pt::new(line_cell(s.status, orient), level, status), step - 1));
        }
    }
    if head {
        out.push(Pt::new(head_cell(lane.status, orient), level_at(now_ms), Some(lane.status)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{render, row, sample_app};
    use super::*;
    use crate::model::{AgentStatus, Mark};

    #[test]
    fn horizontal_steps_are_evenly_spaced() {
        let mut app = sample_app(2, Orient::Horizontal);
        // Header, lane row: no axis row. The strip starts at column 33.
        let buf = render(&mut app, 80, 4);
        assert_eq!(row(&buf, 1, 33, 62), "◆───●━━━▲╍╍╍▸");
    }

    #[test]
    fn overflow_hides_oldest_steps_behind_ellipsis() {
        let mut app = sample_app(2, Orient::Horizontal);
        for k in 0..10 {
            let s = if k % 2 == 0 { AgentStatus::Idle } else { AgentStatus::Working };
            app.state.set_status(0, s, 6000 + k);
        }
        let buf = render(&mut app, 80, 4);
        let lane = row(&buf, 1, 33, 62);
        assert!(lane.starts_with('…'), "{lane}");
        assert!(lane.ends_with('▸'), "{lane}");
    }

    #[test]
    fn seq_back_and_close_end_the_lane_at_a_node() {
        let mut app = sample_app(2, Orient::Horizontal);
        app.seq_back = 1;
        let buf = render(&mut app, 80, 4);
        assert_eq!(row(&buf, 1, 33, 62), "◆───●━━━", "last step hidden, no head");

        app.seq_back = 0;
        app.state.close("w1:p1", Mark::Closed, 9000);
        app.query.show_closed = true;
        let buf = render(&mut app, 80, 4);
        assert_eq!(row(&buf, 1, 33, 62), "◆───●━━━▲╍╍╍✕");
    }
}
