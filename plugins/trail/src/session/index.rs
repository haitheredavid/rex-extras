//! Step index: a session's turns, requests, spans and events, plus the lane's blocked waits (which
//! transcripts don't record), as one time-ordered list of rows the timeline steps through. It is
//! kept in sync as facts arrive: new rows are appended, and only something that sorts before the
//! last row forces a rebuild. A lane with no session indexes its status runs instead.

use super::Session;
use crate::model::{AgentStatus, Lane, Mark};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StepRef {
    Turn(u32),
    Request(u32),
    Span(u32),
    Event(u32),
    /// Index into `lane.steps`.
    Status(u32),
}

impl StepRef {
    /// Tie-break for rows at the same time: a turn before its first request, a request before
    /// the tools it called.
    fn order(self) -> u8 {
        match self {
            StepRef::Turn(_) => 0,
            StepRef::Request(_) => 1,
            StepRef::Span(_) => 2,
            StepRef::Event(_) => 3,
            StepRef::Status(_) => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StepRow {
    pub r: StepRef,
    pub at_ms: i64,
    pub depth: u8,
    /// The turn the row belongs to (its own, for a turn header).
    pub turn: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct StepIndex {
    pub rows: Vec<StepRow>,
    /// Row indices of turn headers.
    pub turn_rows: Vec<u32>,
    /// Turns, requests, spans, events and lane steps already indexed.
    seen: [usize; 5],
    built_rev: u64,
    /// The last request row and the current turn, for placing appended rows.
    last_request: Option<u32>,
    turn: Option<u32>,
}

/// When a row ends, read live so spans and turns that close in place need no re-index.
pub fn end_of(r: StepRef, session: Option<&Session>, lane: &Lane) -> Option<i64> {
    match (r, session) {
        (StepRef::Turn(i), Some(s)) => s.turns[i as usize].ended_ms,
        (StepRef::Request(i), Some(s)) => Some(s.requests[i as usize].ended_ms),
        (StepRef::Span(i), Some(s)) => s.spans[i as usize].ended_ms,
        (StepRef::Status(i), _) => lane.steps.get(i as usize + 1).map(|s| s.at_ms).or(lane.closed_ms),
        _ => None,
    }
}

impl StepIndex {
    pub fn build(session: Option<&Session>, lane: &Lane) -> StepIndex {
        let mut ix = StepIndex::default();
        ix.sync(session, lane);
        ix
    }

    /// Takes in what was applied since the last build or sync.
    pub fn sync(&mut self, session: Option<&Session>, lane: &Lane) {
        if session.is_some_and(|s| s.revision == self.built_rev) && self.seen[4] == lane.steps.len() {
            return;
        }
        let mut items = self.pending(session, lane);
        if items.is_empty() {
            return;
        }
        if self.rows.last().is_some_and(|last| items[0].0 < last.at_ms) {
            *self = StepIndex::default();
            items = self.pending(session, lane);
        }
        for (at_ms, r) in items {
            self.push(session, at_ms, r);
        }
        self.built_rev = session.map_or(0, |s| s.revision);
    }

    pub fn position(&self, r: StepRef) -> Option<usize> {
        self.rows.iter().position(|row| row.r == r)
    }

    /// Items past `seen`, sorted, and `seen` moved past them.
    fn pending(&mut self, session: Option<&Session>, lane: &Lane) -> Vec<(i64, StepRef)> {
        let mut items = Vec::new();
        let new_steps = self.seen[4]..lane.steps.len();
        self.seen[4] = lane.steps.len();
        match session {
            Some(s) => {
                let start = s.turns.first().map(|t| t.started_ms).or(s.started_ms).unwrap_or(i64::MIN);
                items.extend((self.seen[0]..s.turns.len()).map(|i| (s.turns[i].started_ms, StepRef::Turn(i as u32))));
                items.extend((self.seen[1]..s.requests.len()).map(|i| (s.requests[i].started_ms, StepRef::Request(i as u32))));
                items.extend((self.seen[2]..s.spans.len()).map(|i| (s.spans[i].started_ms, StepRef::Span(i as u32))));
                items.extend((self.seen[3]..s.events.len()).map(|i| (s.events[i].at_ms, StepRef::Event(i as u32))));
                // The blocked status covers approval waits no transcript records.
                items.extend(
                    new_steps
                        .filter(|&i| lane.steps[i].mark == Mark::Status(AgentStatus::Blocked) && lane.steps[i].at_ms >= start)
                        .map(|i| (lane.steps[i].at_ms, StepRef::Status(i as u32))),
                );
                self.seen[..4].copy_from_slice(&[s.turns.len(), s.requests.len(), s.spans.len(), s.events.len()]);
            }
            None => items.extend(new_steps.map(|i| (lane.steps[i].at_ms, StepRef::Status(i as u32)))),
        }
        items.sort_by_key(|&(at, r)| (at, r.order(), r));
        items
    }

    fn push(&mut self, session: Option<&Session>, at_ms: i64, r: StepRef) {
        let depth = match (r, session) {
            (StepRef::Turn(i), _) => {
                self.turn = Some(i);
                self.turn_rows.push(self.rows.len() as u32);
                0
            }
            (StepRef::Request(i), _) => {
                self.last_request = Some(i);
                1
            }
            (StepRef::Span(i), Some(s))
                if s.spans[i as usize].request.is_some() && s.spans[i as usize].request == self.last_request =>
            {
                2
            }
            (StepRef::Status(_), None) => 0,
            _ => 1,
        };
        self.rows.push(StepRow { r, at_ms, depth, turn: self.turn });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::*;
    use super::*;
    use crate::model::State;

    pub fn src() -> SourceRef {
        SourceRef { session: 0, offset: 0, len: 0 }
    }

    pub fn request(at: i64) -> Request {
        Request {
            started_ms: at,
            ended_ms: at + 500,
            model: "m".into(),
            usage: Usage::default(),
            ctx_tokens: 1000,
            ctx_window: None,
            thinking_ms: None,
            stop: Stop::ToolUse,
            side: None,
        }
    }

    pub fn span(id: &str, at: i64) -> Fact {
        Fact::SpanStart { id: id.into(), at_ms: at, kind: SpanKind::Tool(ToolCat::Shell), name: "bash".into(), intent: None }
    }

    fn lane() -> (State, usize) {
        let mut s = State::default();
        let i = s.open("w1:p1", "w1", 0, Mark::Seen, AgentStatus::Working, true);
        (s, i)
    }

    #[test]
    fn index_orders_turns_requests_spans() {
        let mut s = Session::new(Source::Omp, "/s.jsonl".into());
        s.apply(Fact::Prompt { at_ms: 100, trigger: Trigger::User, chars: 4, src: src() });
        s.apply(Fact::Request(request(200)));
        s.apply(span("a", 700));
        let (state, li) = lane();
        let ix = StepIndex::build(Some(&s), &state.lanes[li]);
        let shape: Vec<_> = ix.rows.iter().map(|r| (r.r, r.depth)).collect();
        assert_eq!(shape, [(StepRef::Turn(0), 0), (StepRef::Request(0), 1), (StepRef::Span(0), 2)]);
        assert_eq!(ix.turn_rows, [0]);
    }

    #[test]
    fn sync_appends_and_rebuilds_only_for_older_rows() {
        let mut s = Session::new(Source::Omp, "/s.jsonl".into());
        s.apply(Fact::Request(request(200)));
        let (mut state, li) = lane();
        let mut ix = StepIndex::build(Some(&s), &state.lanes[li]);
        assert_eq!(ix.rows.len(), 2, "an implicit turn and its request");
        s.apply(span("a", 900));
        state.set_status(li, AgentStatus::Blocked, 950);
        ix.sync(Some(&s), &state.lanes[li]);
        assert_eq!(ix.rows.iter().map(|r| r.r).collect::<Vec<_>>()[2..], [StepRef::Span(0), StepRef::Status(1)]);
        s.apply(Fact::Point(Event { at_ms: 50, kind: EventKind::Branch }));
        ix.sync(Some(&s), &state.lanes[li]);
        assert_eq!(ix.rows[0].r, StepRef::Event(0), "an older row rebuilds in order");
        assert_eq!(ix.rows.len(), 5);
    }

    #[test]
    fn status_only_lane_lists_status_runs() {
        let (mut state, li) = lane();
        state.set_status(li, AgentStatus::Blocked, 10);
        state.set_status(li, AgentStatus::Idle, 20);
        let ix = StepIndex::build(None, &state.lanes[li]);
        let refs: Vec<_> = ix.rows.iter().map(|r| (r.r, r.depth)).collect();
        assert_eq!(refs, [(StepRef::Status(0), 0), (StepRef::Status(1), 0), (StepRef::Status(2), 0)]);
        assert_eq!(end_of(StepRef::Status(1), None, &state.lanes[li]), Some(20));
    }
}
