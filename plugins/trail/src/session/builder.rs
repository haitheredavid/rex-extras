//! `Session::apply`: the one place that turns decoder facts into turns, requests, spans and events.

use std::collections::HashMap;
use std::path::PathBuf;

use super::{EventKind, Fact, Outcome, Request, Session, Source, Span, SpanStatus, Trigger, Turn};

impl Session {
    pub fn new(source: Source, path: PathBuf) -> Session {
        Session {
            source,
            path,
            id: None,
            label: None,
            cwd: None,
            started_ms: None,
            ended: None,
            parent: None,
            spawn_span: None,
            children: Vec::new(),
            turns: Vec::new(),
            requests: Vec::new(),
            spans: Vec::new(),
            events: Vec::new(),
            ctx: Vec::new(),
            revision: 0,
            open_spans: HashMap::new(),
            child_keys: HashMap::new(),
        }
    }

    fn turn_at(&self, started_ms: i64, trigger: Trigger) -> Turn {
        Turn {
            started_ms,
            ended_ms: None,
            trigger,
            prompt: None,
            prompt_chars: 0,
            outcome: Outcome::Open,
            first_request: self.requests.len() as u32,
            first_span: self.spans.len() as u32,
            first_event: self.events.len() as u32,
        }
    }

    fn open_turn(&mut self) -> Option<&mut Turn> {
        self.turns.last_mut().filter(|t| t.outcome == Outcome::Open)
    }

    /// Work with no open turn starts an implicit one, so it never joins a finished turn.
    fn ensure_turn(&mut self, at_ms: i64) {
        if self.open_turn().is_none() {
            let t = self.turn_at(at_ms, Trigger::Unknown);
            self.turns.push(t);
        }
    }

    fn end_turn(&mut self, at_ms: i64, outcome: Outcome) {
        if let Some(t) = self.open_turn() {
            t.ended_ms = Some(at_ms);
            t.outcome = outcome;
        }
    }

    fn insert_ctx(&mut self, sample: (i64, u64)) {
        let at = self.ctx.partition_point(|s| s.0 <= sample.0);
        self.ctx.insert(at, sample);
    }

    fn push_request(&mut self, r: Request) {
        self.ensure_turn(r.started_ms);
        if r.side.is_none() {
            self.insert_ctx((r.ended_ms, r.ctx_tokens));
        }
        self.requests.push(r);
    }

    pub fn apply(&mut self, f: Fact) {
        self.revision += 1;
        match f {
            Fact::Meta { id, cwd, at_ms } => {
                self.id = self.id.take().or(id);
                self.cwd = self.cwd.take().or(cwd);
                self.started_ms = self.started_ms.or(at_ms);
            }
            Fact::Label(l) => self.label = Some(l),
            Fact::Prompt { at_ms, trigger, chars, src } => {
                self.end_turn(at_ms, Outcome::Completed);
                let mut t = self.turn_at(at_ms, trigger);
                t.prompt = Some(src);
                t.prompt_chars = chars;
                self.turns.push(t);
            }
            Fact::Request(r) => self.push_request(r),
            Fact::RequestUpdate(r) => {
                let Some(last) = self.requests.last() else { return self.push_request(r) };
                if last.side.is_none()
                    && let Some(i) = self.ctx.iter().position(|&s| s == (last.ended_ms, last.ctx_tokens))
                {
                    self.ctx.remove(i);
                }
                if r.side.is_none() {
                    self.insert_ctx((r.ended_ms, r.ctx_tokens));
                }
                *self.requests.last_mut().expect("checked above") = r;
            }
            Fact::SpanStart { id, at_ms, kind, name, intent } => {
                self.ensure_turn(at_ms);
                if self.open_spans.contains_key(&id) {
                    return;
                }
                self.open_spans.insert(id.clone(), self.spans.len() as u32);
                self.spans.push(Span {
                    id,
                    kind,
                    name,
                    started_ms: at_ms,
                    ended_ms: None,
                    status: SpanStatus::Running,
                    request: self.requests.len().checked_sub(1).map(|i| i as u32),
                    intent,
                    child: None,
                    exit_code: None,
                });
            }
            Fact::SpanEnd { id, at_ms, status, exit_code } => {
                let Some(i) = self.open_spans.remove(&id) else { return };
                let span = &mut self.spans[i as usize];
                span.ended_ms = Some(at_ms);
                span.status = status;
                span.exit_code = exit_code;
            }
            // A key spawned twice keeps its latest span; the earlier child stays unlinked.
            Fact::Child { span_id, key } => {
                self.child_keys.insert(key, span_id);
            }
            Fact::TurnEnd { at_ms, outcome } => self.end_turn(at_ms, outcome),
            Fact::Point(e) => {
                let at = e.at_ms;
                if let EventKind::Exit(reason) = &e.kind {
                    self.ended = Some((at, reason.clone()));
                    for (_, i) in self.open_spans.drain() {
                        let span = &mut self.spans[i as usize];
                        span.ended_ms = Some(at);
                        span.status = SpanStatus::Interrupted;
                    }
                    self.end_turn(at, Outcome::Completed);
                }
                let i = self.events.partition_point(|x| x.at_ms <= at);
                self.events.insert(i, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn src(offset: u64) -> SourceRef {
        SourceRef { session: 0, offset, len: 1 }
    }

    fn req(at: i64, ctx: u64, side: Option<&str>) -> Request {
        Request {
            started_ms: at,
            ended_ms: at + 1,
            model: "m".into(),
            usage: Usage::default(),
            ctx_tokens: ctx,
            ctx_window: None,
            thinking_ms: None,
            stop: Stop::ToolUse,
            side: side.map(String::from),
        }
    }

    #[test]
    fn prompts_open_turns_and_work_after_a_finished_turn_starts_a_new_one() {
        let mut s = Session::new(Source::Omp, "/s.jsonl".into());
        s.apply(Fact::Prompt { at_ms: 0, trigger: Trigger::User, chars: 5, src: src(0) });
        s.apply(Fact::Request(req(1, 100, None)));
        s.apply(Fact::SpanStart {
            id: "a".into(),
            at_ms: 3,
            kind: SpanKind::Tool(ToolCat::Shell),
            name: "bash".into(),
            intent: None,
        });
        s.apply(Fact::SpanEnd { id: "a".into(), at_ms: 4, status: SpanStatus::Ok, exit_code: Some(0) });
        s.apply(Fact::TurnEnd { at_ms: 5, outcome: Outcome::Completed });
        s.apply(Fact::Request(req(10, 50, Some("title"))));
        assert_eq!(s.turns.len(), 2, "the side call after the turn ended opens an implicit turn");
        assert_eq!(s.turns[1].trigger, Trigger::Unknown);
        assert_eq!(s.spans[0].request, Some(0));
        assert_eq!(s.ctx, [(2, 100)], "side calls carry no context");
        assert_eq!(s.revision, 6);
    }

    #[test]
    fn request_update_replaces_the_last_request_and_its_sample() {
        let mut s = Session::new(Source::Claude, "/s.jsonl".into());
        s.apply(Fact::Request(req(0, 100, None)));
        s.apply(Fact::RequestUpdate(Request { ended_ms: 9, ..req(0, 120, None) }));
        assert_eq!(s.requests.len(), 1);
        assert_eq!(s.ctx, [(9, 120)]);
    }

    #[test]
    fn exit_interrupts_open_spans() {
        let mut s = Session::new(Source::Omp, "/s.jsonl".into());
        s.apply(Fact::SpanStart { id: "a".into(), at_ms: 1, kind: SpanKind::Subagent, name: "task".into(), intent: None });
        s.apply(Fact::Point(Event { at_ms: 7, kind: EventKind::Exit("quit".into()) }));
        assert_eq!(s.spans[0].status, SpanStatus::Interrupted);
        assert_eq!(s.spans[0].ended_ms, Some(7));
        assert_eq!(s.turns[0].outcome, Outcome::Completed);
        assert_eq!(s.ended, Some((7, "quit".into())));
    }
}
