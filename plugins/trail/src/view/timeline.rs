//! TIMELINE: one agent session step by step. A scrubber across the whole session (busy phases,
//! turn starts, compactions, errors, subagent spawns), the step list (turns → model requests,
//! tools, subagents, waits, events), and an inspector for the step under the cursor. Live while
//! following; paused once the cursor moves. Subagent steps open their own session on a stack.
//! A lane with no session lists its status runs.

use std::collections::BTreeSet;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::chrome::{Seg, draw_header};
use super::gutter::fmt_cost;
use super::prompt::{Prompt, PromptKind};
use super::{App, Hit, Orient, fmt_dur, fmt_short, hm, hms, put_str, status_glyph, truncate};
use crate::model::{AgentStatus, Lane};
use crate::session::index::{StepIndex, StepRef, StepRow, end_of};
use crate::session::text::{TextLevel, intent_text, prompt_text};
use crate::session::{EventKind, Outcome, Session, SessionIdx, SpanKind, SpanStatus, Store, ToolCat, Trigger, fmt_tokens};
use crate::theme::theme;

/// Idle stretches longer than this fold in the gap encoding and get a separator in the list.
const GAP_MS: i64 = 120_000;
/// Width of the list's inspector beside it, on wide frames.
const SIDE_W: u16 = 60;

pub enum Surface {
    Main,
    Timeline(Box<TimelineState>),
}

/// How the scrubber spreads the session across its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Wall-clock time, with long idle stretches folded to a fixed token.
    Gap,
    Wall,
    /// One slot per step.
    Ordinal,
}

impl Encoding {
    fn name(self) -> &'static str {
        match self {
            Encoding::Gap => "gap",
            Encoding::Wall => "wall",
            Encoding::Ordinal => "ordinal",
        }
    }
}

/// What `n`/`N` jump between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumpKind {
    Error,
    Blocked,
    Compaction,
    Prompt,
    Subagent,
    Tool(String),
}

impl JumpKind {
    fn name(&self) -> String {
        match self {
            JumpKind::Error => "errors".into(),
            JumpKind::Blocked => "blocked".into(),
            JumpKind::Compaction => "compaction".into(),
            JumpKind::Prompt => "prompts".into(),
            JumpKind::Subagent => "subagents".into(),
            JumpKind::Tool(t) => t.clone(),
        }
    }
}

pub struct TimelineState {
    /// Lane id.
    pub lane: u64,
    /// Root session first, then each subagent drilled into.
    pub stack: Vec<Frame>,
    pub follow: bool,
    pub encoding: Encoding,
    pub jump: JumpKind,
    pub text: TextLevel,
    /// `;` was pressed: the next key picks the jump kind.
    pub awaiting_jump_kind: bool,
    /// Rows added while paused.
    pub new_since_pause: usize,
    /// First visible row of each scrubber bin in the last draw (None for folds and empty bins).
    bin_rows: Vec<Option<usize>>,
}

pub struct Frame {
    /// None: the lane has no session; rows are its status runs.
    pub session: Option<SessionIdx>,
    /// (k, n) among the lane's root sessions, 1-based; root frame only.
    pub root_pos: Option<(usize, usize)>,
    pub index: StepIndex,
    /// None while the frame has no rows.
    pub cursor: Option<StepRef>,
    pub folded_turns: BTreeSet<u32>,
    pub folded_requests: BTreeSet<u32>,
    /// First visible row on screen.
    pub top: usize,
}

impl Frame {
    fn new(session: Option<SessionIdx>, root_pos: Option<(usize, usize)>, store: &Store, lane: &Lane) -> Frame {
        let index = StepIndex::build(session.map(|i| &store.sessions[i]), lane);
        let cursor = index.rows.last().map(|r| r.r);
        Frame { session, root_pos, index, cursor, folded_turns: BTreeSet::new(), folded_requests: BTreeSet::new(), top: 0 }
    }

    fn cursor_row(&self) -> Option<usize> {
        self.cursor.and_then(|r| self.index.position(r))
    }

    /// Row indices the folds leave visible.
    fn visible(&self, session: Option<&Session>) -> Vec<usize> {
        let hidden = |row: &StepRow| match row.r {
            StepRef::Turn(_) => false,
            _ if row.turn.is_some_and(|t| self.folded_turns.contains(&t)) => true,
            StepRef::Span(i) if row.depth == 2 => {
                session.and_then(|s| s.spans[i as usize].request).is_some_and(|q| self.folded_requests.contains(&q))
            }
            _ => false,
        };
        (0..self.index.rows.len()).filter(|&k| !hidden(&self.index.rows[k])).collect()
    }

    /// Opens whatever fold hides row `k`.
    fn reveal(&mut self, k: usize, session: Option<&Session>) {
        let row = &self.index.rows[k];
        if let Some(t) = row.turn
            && row.r != StepRef::Turn(t)
        {
            self.folded_turns.remove(&t);
        }
        if let (StepRef::Span(i), Some(s)) = (row.r, session)
            && let Some(q) = s.spans[i as usize].request
        {
            self.folded_requests.remove(&q);
        }
    }
}

/// What a frame shows: the lane, its session (if any), and now.
struct View<'a> {
    store: &'a Store,
    lane: &'a Lane,
    session: Option<&'a Session>,
    now: i64,
}

impl View<'_> {
    fn end(&self, r: StepRef) -> Option<i64> {
        end_of(r, self.session, self.lane)
    }

    fn matches(&self, jump: &JumpKind, r: StepRef) -> bool {
        let Some(s) = self.session else {
            return *jump == JumpKind::Blocked
                && matches!(r, StepRef::Status(i) if self.lane.steps[i as usize].status == AgentStatus::Blocked);
        };
        match (jump, r) {
            (JumpKind::Error, StepRef::Span(i)) => {
                matches!(s.spans[i as usize].status, SpanStatus::Error | SpanStatus::Interrupted)
            }
            (JumpKind::Error, StepRef::Event(i)) => {
                matches!(s.events[i as usize].kind, EventKind::ApiError | EventKind::Interrupt)
            }
            (JumpKind::Error, StepRef::Turn(i)) => matches!(s.turns[i as usize].outcome, Outcome::Error | Outcome::Aborted),
            (JumpKind::Blocked, StepRef::Span(i)) => s.spans[i as usize].kind == SpanKind::WaitUser,
            (JumpKind::Blocked, StepRef::Status(_)) => true,
            (JumpKind::Compaction, StepRef::Event(i)) => matches!(s.events[i as usize].kind, EventKind::Compaction { .. }),
            (JumpKind::Prompt, StepRef::Turn(i)) => s.turns[i as usize].trigger == Trigger::User,
            (JumpKind::Subagent, StepRef::Span(i)) => s.spans[i as usize].kind == SpanKind::Subagent,
            (JumpKind::Tool(name), StepRef::Span(i)) => s.spans[i as usize].name == *name,
            _ => false,
        }
    }

    /// Requests of turn `t`: up to the next turn's first.
    fn turn_requests(&self, t: u32) -> std::ops::Range<usize> {
        let Some(s) = self.session else { return 0..0 };
        let from = s.turns[t as usize].first_request as usize;
        let to = s.turns.get(t as usize + 1).map_or(s.requests.len(), |n| n.first_request as usize);
        from..to.max(from)
    }

    fn turn_cost(&self, t: u32) -> Option<f64> {
        let s = self.session?;
        self.turn_requests(t).filter_map(|i| s.requests[i].usage.cost).reduce(|a, b| a + b)
    }

    /// Cost of session `i` and everything it spawned.
    fn tree_cost(&self, i: SessionIdx) -> Option<f64> {
        let s = &self.store.sessions[i];
        let own = s.requests.iter().filter_map(|r| r.usage.cost).reduce(|a, b| a + b);
        s.children.iter().map(|&c| self.tree_cost(c)).fold(own, |acc, c| match (acc, c) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        })
    }

    /// First and last activity of session `i`.
    fn span_of(&self, i: SessionIdx) -> Option<(i64, i64)> {
        let s = &self.store.sessions[i];
        let first = s.turns.first().map(|t| t.started_ms).or(s.started_ms).or(s.requests.first().map(|r| r.started_ms))?;
        let last = s.requests.iter().map(|r| r.ended_ms).chain(s.ended.as_ref().map(|e| e.0)).max().unwrap_or(first);
        Some((first, last))
    }

    /// Context in use at `t` in this frame's session.
    fn ctx_at(&self, t: i64) -> Option<u64> {
        let ctx = &self.session?.ctx;
        ctx[..ctx.partition_point(|s| s.0 <= t)].last().map(|s| s.1)
    }

    /// Context of the main request before request `i`.
    fn prev_ctx(&self, i: usize) -> Option<u64> {
        self.session?.requests[..i].iter().rev().find(|r| r.side.is_none()).map(|r| r.ctx_tokens)
    }
}

fn cat_glyph(c: ToolCat) -> char {
    match c {
        ToolCat::Shell => '$',
        ToolCat::Read => 'r',
        ToolCat::Search => '/',
        ToolCat::Edit => 'e',
        ToolCat::Write => 'w',
        ToolCat::Web => '@',
        ToolCat::Plan => '☰',
        ToolCat::Code => 'ƒ',
        ToolCat::Mcp => 'm',
        ToolCat::Other => '·',
    }
}

fn cat_name(c: ToolCat) -> &'static str {
    match c {
        ToolCat::Shell => "shell",
        ToolCat::Read => "read",
        ToolCat::Search => "search",
        ToolCat::Edit => "edit",
        ToolCat::Write => "write",
        ToolCat::Web => "web",
        ToolCat::Plan => "plan",
        ToolCat::Code => "code",
        ToolCat::Mcp => "mcp",
        ToolCat::Other => "tool",
    }
}

fn status_name(s: SpanStatus) -> &'static str {
    match s {
        SpanStatus::Running => "running",
        SpanStatus::Ok => "ok",
        SpanStatus::Error => "error",
        SpanStatus::Interrupted => "interrupted",
    }
}

/// `+1.2k`, `−3k`.
fn fmt_delta(d: i64) -> String {
    let sign = if d < 0 { '−' } else { '+' };
    let n = d.unsigned_abs();
    if (1000..10_000).contains(&n) { format!("{sign}{:.1}k", n as f64 / 1000.0) } else { format!("{sign}{}", fmt_tokens(n)) }
}

/// Opens the timeline on the highlighted lane: its newest root session, or its status runs.
pub(super) fn open(app: &mut App) {
    let Some(li) = app.selected_lane() else { return };
    let lane = &app.state.lanes[li];
    let roots = app.sessions.roots(lane);
    let frame = match roots.last() {
        Some(&s) => Frame::new(Some(s), Some((roots.len(), roots.len())), &app.sessions, lane),
        None => Frame::new(None, None, &app.sessions, lane),
    };
    app.surface = Surface::Timeline(Box::new(TimelineState {
        lane: lane.id,
        stack: vec![frame],
        follow: true,
        encoding: Encoding::Gap,
        jump: JumpKind::Error,
        text: TextLevel::None,
        awaiting_jump_kind: false,
        new_since_pause: 0,
        bin_rows: Vec::new(),
    }));
}

impl TimelineState {
    fn frame(&self) -> &Frame {
        self.stack.last().expect("the timeline always has a frame")
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.stack.last_mut().expect("the timeline always has a frame")
    }

    /// Indexes what arrived since the last draw; the cursor follows the end while live.
    fn sync(&mut self, v: &View) {
        let follow = self.follow;
        let f = self.frame_mut();
        let before = f.index.rows.len();
        f.index.sync(v.session, v.lane);
        let added = f.index.rows.len() - before;
        if follow {
            f.cursor = f.index.rows.last().map(|r| r.r);
        } else if f.cursor.is_none() {
            f.cursor = f.index.rows.first().map(|r| r.r);
        }
        if !follow {
            self.new_since_pause += added;
        }
    }

    fn pause(&mut self) {
        if self.follow {
            self.follow = false;
            self.new_since_pause = 0;
        }
    }

    /// Moves the cursor to row `k` (revealing it), pausing.
    fn goto(&mut self, k: usize, v: &View) {
        self.pause();
        let f = self.frame_mut();
        f.reveal(k, v.session);
        f.cursor = Some(f.index.rows[k].r);
    }

    /// Moves `by` visible rows.
    fn step(&mut self, by: isize, v: &View) {
        let f = self.frame();
        let vis = f.visible(v.session);
        let Some(at) = f.cursor_row().and_then(|c| vis.iter().position(|&k| k >= c)) else { return };
        let to = (at as isize + by).clamp(0, vis.len() as isize - 1) as usize;
        self.goto(vis[to], v);
    }

    fn jump(&mut self, forward: bool, v: &View) {
        let f = self.frame();
        let Some(c) = f.cursor_row() else { return };
        let hit = |k: &usize| v.matches(&self.jump, f.index.rows[*k].r);
        let k = if forward { (c + 1..f.index.rows.len()).find(hit) } else { (0..c).rev().find(hit) };
        if let Some(k) = k {
            self.goto(k, v);
        }
    }

    fn turn(&mut self, forward: bool, v: &View) {
        let f = self.frame();
        let Some(c) = f.cursor_row() else { return };
        let turns = &f.index.turn_rows;
        let k = if forward { turns.iter().find(|&&t| t as usize > c) } else { turns.iter().rev().find(|&&t| (t as usize) < c) };
        if let Some(&k) = k {
            self.goto(k as usize, v);
        }
    }

    fn bin(&mut self, forward: bool, v: &View) {
        let Some(c) = self.frame().cursor_row() else { return };
        let bins = &self.bin_rows;
        let here = bins.iter().rposition(|b| b.is_some_and(|k| k <= c)).unwrap_or(0);
        let k = if forward { bins[here + 1..].iter().flatten().next() } else { bins[..here].iter().rev().flatten().next() };
        if let Some(&k) = k {
            self.goto(k, v);
        }
    }

    /// Space: a turn header folds its turn; a request folds the tools it called.
    fn toggle_fold(&mut self) {
        fn toggle(set: &mut BTreeSet<u32>, v: u32) {
            if !set.remove(&v) {
                set.insert(v);
            }
        }
        let f = self.frame_mut();
        match f.cursor {
            Some(StepRef::Turn(t)) => toggle(&mut f.folded_turns, t),
            Some(StepRef::Request(q)) => toggle(&mut f.folded_requests, q),
            _ => {}
        }
    }

    /// `z`: every turn but the cursor's folds, and the cursor moves to its turn header.
    fn focus_turn(&mut self) {
        let f = self.frame_mut();
        let Some(c) = f.cursor_row() else { return };
        let mine = f.index.rows[c].turn;
        f.folded_turns =
            f.index.turn_rows.iter().filter_map(|&k| f.index.rows[k as usize].turn).filter(|&t| Some(t) != mine).collect();
    }
}

/// The frame's lane, session and the store, or None when the lane is gone.
fn view<'a>(app: &'a App, tl: &TimelineState) -> Option<View<'a>> {
    let lane = &app.state.lanes[app.state.lane_by_id(tl.lane)?];
    let session = tl.frame().session.map(|i| &app.sessions.sessions[i]);
    Some(View { store: &app.sessions, lane, session, now: app.now_ms })
}

/// Timeline keys; the main keymap doesn't apply while it's open.
pub(super) fn on_key(app: &mut App, key: KeyEvent) {
    let Surface::Timeline(mut tl) = std::mem::replace(&mut app.surface, Surface::Main) else { return };
    if handle(&mut tl, app, key) && matches!(app.surface, Surface::Main) {
        app.surface = Surface::Timeline(tl);
    }
}

/// Handles one key; false leaves the timeline.
fn handle(tl: &mut TimelineState, app: &mut App, key: KeyEvent) -> bool {
    let Some(v) = view(app, tl) else { return false };
    if tl.awaiting_jump_kind {
        tl.awaiting_jump_kind = false;
        tl.jump = match key.code {
            KeyCode::Char('e') => JumpKind::Error,
            KeyCode::Char('b') => JumpKind::Blocked,
            KeyCode::Char('c') => JumpKind::Compaction,
            KeyCode::Char('p') => JumpKind::Prompt,
            KeyCode::Char('s') => JumpKind::Subagent,
            KeyCode::Char('t') => {
                app.prompt = Some(Prompt { kind: PromptKind::JumpTool, text: String::new() });
                return true;
            }
            _ => return true,
        };
        return true;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => tl.step(1, &v),
        KeyCode::Char('k') | KeyCode::Up => tl.step(-1, &v),
        KeyCode::Char('J') => tl.turn(true, &v),
        KeyCode::Char('K') => tl.turn(false, &v),
        KeyCode::Char('h') | KeyCode::Left => tl.bin(false, &v),
        KeyCode::Char('l') | KeyCode::Right => tl.bin(true, &v),
        KeyCode::Char('H') => tl.step(isize::MIN / 2, &v),
        KeyCode::Char('L') => tl.step(isize::MAX / 2, &v),
        KeyCode::Char('n') => tl.jump(true, &v),
        KeyCode::Char('N') => tl.jump(false, &v),
        KeyCode::Char(';') => tl.awaiting_jump_kind = true,
        KeyCode::Char(' ') => tl.toggle_fold(),
        KeyCode::Char('z') => tl.focus_turn(),
        KeyCode::Enter | KeyCode::Char(']') => {
            let child = match (tl.frame().cursor, v.session) {
                (Some(StepRef::Span(i)), Some(s)) => s.spans[i as usize].child,
                _ => None,
            };
            match child {
                Some(c) => {
                    let frame = Frame::new(Some(c), None, v.store, v.lane);
                    tl.stack.push(frame);
                    tl.follow = true;
                }
                None if key.code == KeyCode::Enter => app.focus_requested = Some(v.lane.pane_id.clone()),
                None => {}
            }
        }
        KeyCode::Char('[') | KeyCode::Backspace => back(tl, &v),
        KeyCode::Char('f') => {
            tl.follow = true;
            tl.new_since_pause = 0;
            let f = tl.frame_mut();
            f.cursor = f.index.rows.last().map(|r| r.r);
        }
        KeyCode::Char('t') => {
            tl.encoding = match tl.encoding {
                Encoding::Gap => Encoding::Wall,
                Encoding::Wall => Encoding::Ordinal,
                Encoding::Ordinal => Encoding::Gap,
            }
        }
        KeyCode::Char('T') => {
            let next = match tl.text {
                TextLevel::None => TextLevel::Intent,
                TextLevel::Intent => TextLevel::Prompt,
                TextLevel::Prompt => TextLevel::None,
            };
            // `[privacy] max_text` caps what the timeline may read back.
            tl.text = if next > crate::config::config().max_text { TextLevel::None } else { next };
        }
        KeyCode::Esc => {
            if tl.stack.len() == 1 {
                return false;
            }
            tl.stack.pop();
        }
        KeyCode::Char('?') => app.help = true,
        KeyCode::Char('q') => app.quit = true,
        _ => {}
    }
    true
}

/// `[`: out of a subagent, or at the root to the lane's previous session.
fn back(tl: &mut TimelineState, v: &View) {
    if tl.stack.len() > 1 {
        tl.stack.pop();
        return;
    }
    let Some((k, n)) = tl.frame().root_pos else { return };
    if k > 1 {
        let roots = v.store.roots(v.lane);
        tl.stack[0] = Frame::new(Some(roots[k - 2]), Some((k - 1, n)), v.store, v.lane);
    }
}

/// A click on a list row: the cursor goes there; a double-click opens it as Enter would.
pub(super) fn click(app: &mut App, r: StepRef, double: bool) {
    let Surface::Timeline(mut tl) = std::mem::replace(&mut app.surface, Surface::Main) else { return };
    if let Some(v) = view(app, &tl)
        && let Some(k) = tl.frame().index.position(r)
    {
        tl.goto(k, &v);
    }
    app.surface = Surface::Timeline(tl);
    if double {
        on_key(app, KeyEvent::new(KeyCode::Enter, ratatui::crossterm::event::KeyModifiers::NONE));
    }
}

/// A prompt's accepted text, for the timeline.
pub(super) fn accept_tool(app: &mut App, name: String) {
    if let Surface::Timeline(tl) = &mut app.surface {
        tl.jump = JumpKind::Tool(name);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Idle,
    Model,
    Tool,
    WaitAgents,
    WaitUser,
}

struct Interval {
    from: i64,
    to: i64,
    phase: Phase,
}

/// What the session was doing when: requests think, tools run, waits wait; blocked status runs
/// are waits on the user. Without a session, working runs count as busy.
fn intervals(v: &View) -> Vec<Interval> {
    let mut out = Vec::new();
    if let Some(s) = v.session {
        out.extend(s.requests.iter().map(|r| Interval { from: r.started_ms, to: r.ended_ms, phase: Phase::Model }));
        out.extend(s.spans.iter().map(|sp| Interval {
            from: sp.started_ms,
            to: sp.ended_ms.unwrap_or(v.now),
            phase: match sp.kind {
                SpanKind::WaitUser => Phase::WaitUser,
                SpanKind::WaitAgents => Phase::WaitAgents,
                SpanKind::Tool(_) | SpanKind::Subagent => Phase::Tool,
            },
        }));
    }
    let steps = &v.lane.steps;
    for (i, st) in steps.iter().enumerate() {
        let phase = match st.status {
            AgentStatus::Blocked => Phase::WaitUser,
            AgentStatus::Working if v.session.is_none() => Phase::Tool,
            _ => continue,
        };
        let to = steps.get(i + 1).map(|n| n.at_ms).or(v.lane.closed_ms).unwrap_or(v.now);
        out.push(Interval { from: st.at_ms, to, phase });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mark {
    TurnStart,
    Compaction,
    Spawn,
    Error,
}

fn mark_of(v: &View, r: StepRef) -> Option<Mark> {
    if v.matches(&JumpKind::Error, r) {
        return Some(Mark::Error);
    }
    let s = v.session?;
    match r {
        StepRef::Turn(_) => Some(Mark::TurnStart),
        StepRef::Event(i) if matches!(s.events[i as usize].kind, EventKind::Compaction { .. }) => Some(Mark::Compaction),
        StepRef::Span(i) if s.spans[i as usize].kind == SpanKind::Subagent => Some(Mark::Spawn),
        _ => None,
    }
}

/// One scrubber slot.
#[derive(Debug, Clone, PartialEq)]
enum Bin {
    /// Wall-clock `[from, to)`.
    Time(i64, i64),
    /// Rows `[lo, hi)`.
    Rows(usize, usize),
    /// A folded idle stretch, drawn as its token.
    Fold(String),
}

impl Bin {
    fn width(&self) -> usize {
        match self {
            Bin::Fold(t) => t.chars().count(),
            _ => 1,
        }
    }
}

/// Bins across `width` cells for `[t0, t1)` and `rows` rows. `wide_folds`: a fold may be a
/// `…40m…` token (across); otherwise every fold is one `…`.
fn layout_bins(enc: Encoding, ivs: &[Interval], rows: usize, t0: i64, t1: i64, width: usize, wide_folds: bool) -> Vec<Bin> {
    if width == 0 {
        return Vec::new();
    }
    let span = (t1 - t0).max(1);
    let even = |a: i64, b: i64, n: usize| -> Vec<Bin> {
        (0..n).map(|i| Bin::Time(a + (b - a) * i as i64 / n as i64, a + (b - a) * (i as i64 + 1) / n as i64)).collect()
    };
    match enc {
        Encoding::Wall => even(t0, t0 + span, width),
        Encoding::Ordinal => {
            let n = rows.min(width);
            (0..n).map(|i| Bin::Rows(i * rows / n, (i + 1) * rows / n)).collect()
        }
        Encoding::Gap => {
            // Busy stretches, merged; idle gaps longer than GAP_MS between them fold.
            let mut busy: Vec<(i64, i64)> = ivs.iter().map(|i| (i.from.max(t0), i.to.min(t1).max(i.from.max(t0)))).collect();
            busy.sort();
            let mut segs: Vec<Result<(i64, i64), i64>> = Vec::new();
            let (mut seg_start, mut cur) = (t0, t0);
            for (a, b) in busy {
                if a - cur > GAP_MS {
                    if cur > seg_start {
                        segs.push(Ok((seg_start, cur)));
                    }
                    segs.push(Err(a - cur));
                    seg_start = a;
                }
                cur = cur.max(b);
            }
            if t1 - cur > GAP_MS {
                if cur > seg_start {
                    segs.push(Ok((seg_start, cur)));
                }
                segs.push(Err(t1 - cur));
            } else {
                segs.push(Ok((seg_start, t1.max(seg_start + 1))));
            }
            let token = |gap: i64| format!("…{}…", fmt_short(gap));
            let tokens: usize = segs.iter().filter_map(|s| s.err()).map(|g| token(g).chars().count()).sum();
            let short = !wide_folds || tokens > width / 3;
            let fold_w: usize =
                segs.iter().filter(|s| s.is_err()).map(|s| if short { 1 } else { token(s.unwrap_err()).chars().count() }).sum();
            let room = width.saturating_sub(fold_w);
            let total: i64 = segs.iter().filter_map(|s| s.ok()).map(|(a, b)| b - a).sum::<i64>().max(1);
            let spans = segs.iter().filter(|s| s.is_ok()).count();
            let mut out = Vec::new();
            let mut given = 0;
            let mut seen = 0;
            for s in &segs {
                match *s {
                    Err(g) => out.push(Bin::Fold(if short { "…".into() } else { token(g) })),
                    Ok((a, b)) => {
                        seen += 1;
                        let n = if seen == spans {
                            room.saturating_sub(given).max(1)
                        } else {
                            (((b - a) as f64 / total as f64) * room as f64).round().max(1.0) as usize
                        };
                        given += n;
                        out.extend(even(a, b, n));
                    }
                }
            }
            out
        }
    }
}

/// Glyph and style for one bin; `cursor` is the row under the cursor.
fn bin_cell(bin: &Bin, v: &View, ivs: &[Interval], rows: &[StepRow], cursor: Option<usize>, last: bool) -> (String, Style) {
    let t = theme();
    let (lo, hi) = match bin {
        Bin::Fold(token) => return (token.clone(), Style::new().fg(t.dim)),
        &Bin::Time(a, b) => {
            (rows.partition_point(|r| r.at_ms < a), if last { rows.len() } else { rows.partition_point(|r| r.at_ms < b) })
        }
        &Bin::Rows(a, b) => (a, b),
    };
    if cursor.is_some_and(|c| (lo..hi).contains(&c)) {
        return ("◆".into(), Style::new().fg(t.accent).add_modifier(Modifier::BOLD));
    }
    if let Some(m) = rows[lo..hi].iter().filter_map(|r| mark_of(v, r.r)).max() {
        return match m {
            Mark::Error => ("✕".into(), Style::new().fg(t.error)),
            Mark::Spawn => ("⑂".into(), Style::new().fg(t.accent)),
            Mark::Compaction => ("⇣".into(), Style::new().fg(t.accent)),
            Mark::TurnStart => ("│".into(), Style::new().fg(t.dim)),
        };
    }
    let phase = match *bin {
        Bin::Time(a, b) => ivs.iter().filter(|i| i.from < b && i.to > a).map(|i| i.phase).max().unwrap_or(Phase::Idle),
        _ => rows[lo..hi]
            .iter()
            .map(|r| match (r.r, v.session) {
                (StepRef::Request(_), _) => Phase::Model,
                (StepRef::Span(i), Some(s)) => match s.spans[i as usize].kind {
                    SpanKind::WaitUser => Phase::WaitUser,
                    SpanKind::WaitAgents => Phase::WaitAgents,
                    _ => Phase::Tool,
                },
                (StepRef::Status(i), _) => match v.lane.steps[i as usize].status {
                    AgentStatus::Blocked => Phase::WaitUser,
                    AgentStatus::Working => Phase::Tool,
                    _ => Phase::Idle,
                },
                _ => Phase::Idle,
            })
            .max()
            .unwrap_or(Phase::Idle),
    };
    match phase {
        Phase::WaitUser => ("▲".into(), Style::new().fg(t.blocked)),
        Phase::WaitAgents => ("⧖".into(), Style::new().fg(t.done)),
        Phase::Tool => ("▆".into(), Style::new().fg(t.working)),
        Phase::Model => ("▃".into(), Style::new().fg(t.working)),
        Phase::Idle => ("·".into(), Style::new().fg(t.dim)),
    }
}

/// The session's time range: its first row to its end, or now.
fn time_range(v: &View, rows: &[StepRow]) -> (i64, i64) {
    let t0 = rows.first().map_or(v.lane.opened_ms, |r| r.at_ms);
    let t1 = v.session.and_then(|s| s.ended.as_ref().map(|e| e.0)).or(v.lane.closed_ms).unwrap_or(v.now).max(t0 + 1);
    (t0, t1)
}

fn draw_scrubber(buf: &mut Buffer, area: Rect, vertical: bool, tl: &mut TimelineState, v: &View) {
    let f = tl.frame();
    let rows = &f.index.rows;
    let ivs = intervals(v);
    let (t0, t1) = time_range(v, rows);
    let length = if vertical { area.height } else { area.width } as usize;
    // ↔ gives the range its own label when the bins keep 30 cells.
    let label = format!("{} ─ {}", hm(t0), hm(t1));
    let label_w = label.chars().count() + 1;
    let show_label = !vertical && length >= label_w + 30;
    let bins_w = if show_label { length - label_w } else { length };
    let bins = layout_bins(tl.encoding, &ivs, rows.len(), t0, t1, bins_w, !vertical);
    let cursor = f.cursor_row();
    let visible: BTreeSet<usize> = f.visible(v.session).into_iter().collect();
    let bin_rows = bins
        .iter()
        .map(|b| match *b {
            Bin::Fold(_) => None,
            Bin::Time(a, b) => rows.iter().position(|r| r.at_ms >= a && r.at_ms < b).filter(|k| visible.contains(k)),
            Bin::Rows(lo, _) => Some(lo),
        })
        .collect();
    let mut at = 0u16;
    for (i, b) in bins.iter().enumerate() {
        let (text, style) = bin_cell(b, v, &ivs, rows, cursor, i + 1 == bins.len());
        for (k, ch) in text.chars().enumerate() {
            let at = at + k as u16;
            let (x, y) = if vertical { (area.x, area.y + at) } else { (area.x + at, area.y) };
            put_str(buf, area, x, y, &ch.to_string(), style);
        }
        at += b.width() as u16;
    }
    if show_label {
        put_str(buf, area, area.right() - label.chars().count() as u16, area.y, &label, Style::new().fg(theme().dim));
    }
    tl.bin_rows = bin_rows;
}

/// One step as list text, with its style.
fn row_text(v: &View, tl: &TimelineState, row: &StepRow) -> (String, Style) {
    let t = theme();
    let plain = Style::new();
    let dim = Style::new().fg(t.dim);
    let f = tl.frame();
    let dur = |start: i64, end: Option<i64>| end.map_or_else(|| format!("…{}", fmt_dur(v.now - start)), |e| fmt_dur(e - start));
    let at = hms(row.at_ms);
    let Some(s) = v.session else {
        let StepRef::Status(i) = row.r else { return (String::new(), plain) };
        let st = &v.lane.steps[i as usize];
        let until = v.end(row.r).unwrap_or(v.now);
        return (
            format!("{at}  {} {} {}", status_glyph(st.status), st.status.as_str(), fmt_dur(until - st.at_ms)),
            Style::new().fg(t.status(st.status)),
        );
    };
    match row.r {
        StepRef::Turn(i) => {
            let turn = &s.turns[i as usize];
            let lead = match turn.trigger {
                Trigger::User => '▶',
                Trigger::Agent => '↻',
                Trigger::Unknown => '·',
            };
            let title = (tl.text == TextLevel::Prompt)
                .then(|| turn.prompt.and_then(|src| prompt_text(s, src)))
                .flatten()
                .unwrap_or_else(|| format!("turn {}", i + 1));
            let steps = f.index.rows.iter().filter(|r| r.turn == Some(i) && r.r != row.r).count();
            let outcome = match turn.outcome {
                Outcome::Completed => "✓",
                Outcome::Error | Outcome::Aborted => "✕",
                Outcome::Open => "…",
            };
            let folded = if f.folded_turns.contains(&i) { " (folded)" } else { "" };
            let text = format!(
                "{lead} {at}  {title}  {steps} steps · {} · {} {outcome}{folded}",
                dur(turn.started_ms, turn.ended_ms.or((turn.outcome == Outcome::Open).then_some(v.now))),
                fmt_cost(v.turn_cost(i)),
            );
            (text, Style::new().add_modifier(Modifier::BOLD))
        }
        StepRef::Request(i) => {
            let r = &s.requests[i as usize];
            let model = match &r.side {
                Some(p) => format!("{} ({p})", r.model),
                None => r.model.clone(),
            };
            let delta = match (r.side.is_none(), v.prev_ctx(i as usize)) {
                (true, Some(p)) => format!("  {} ctx {}", fmt_delta(r.ctx_tokens as i64 - p as i64), fmt_tokens(r.ctx_tokens)),
                (true, None) => format!("  ctx {}", fmt_tokens(r.ctx_tokens)),
                (false, _) => String::new(),
            };
            (format!("{at}  ◆ {model}  {}{delta}", fmt_dur(r.ended_ms - r.started_ms)), plain)
        }
        StepRef::Span(i) => {
            let sp = &s.spans[i as usize];
            let took = dur(sp.started_ms, sp.ended_ms);
            match sp.kind {
                SpanKind::Subagent => {
                    let child = sp.child.map(|c| {
                        let label = v.store.sessions[c].label.clone().unwrap_or_default();
                        let d = v.span_of(c).map_or("–".into(), |(a, b)| fmt_dur(b - a));
                        format!("{label}  {d}  {}  ⏎ open", fmt_cost(v.tree_cost(c)))
                    });
                    (format!("{at}  ⑂ {}  {}", sp.name, child.unwrap_or(took)), Style::new().fg(t.accent))
                }
                SpanKind::WaitUser => (format!("{at}  ▲ {} waited {took}", sp.name), Style::new().fg(t.blocked)),
                SpanKind::WaitAgents => (format!("{at}  ⧖ {} {took}", sp.name), Style::new().fg(t.done)),
                SpanKind::Tool(cat) => {
                    let intent = (tl.text >= TextLevel::Intent)
                        .then(|| sp.intent.and_then(|src| intent_text(s, src)))
                        .flatten()
                        .map(|i| format!("  {i}"))
                        .unwrap_or_default();
                    let tail = match (sp.status, sp.exit_code) {
                        (_, Some(code)) if code != 0 || sp.status != SpanStatus::Error => format!("  exit {code}"),
                        (SpanStatus::Error | SpanStatus::Interrupted, _) => "  ✕".into(),
                        _ => String::new(),
                    };
                    let style = if matches!(sp.status, SpanStatus::Error | SpanStatus::Interrupted) {
                        Style::new().fg(t.error)
                    } else {
                        plain
                    };
                    (format!("{at}  {} {}{intent}  {took}{tail}", cat_glyph(cat), sp.name), style)
                }
            }
        }
        StepRef::Event(i) => {
            let e = &s.events[i as usize];
            let (text, style) = match &e.kind {
                EventKind::Compaction { before, after } => {
                    let n = |x: &Option<u64>| x.map_or("?".into(), fmt_tokens);
                    (format!("⇣ compaction {}→{}", n(before), n(after)), Style::new().fg(t.accent))
                }
                EventKind::ModelChange(m) => (format!("⚙ model {m}"), dim),
                EventKind::ModeChange(m) => (format!("⚙ mode {m}"), dim),
                EventKind::ApiError => ("✕ api error".into(), Style::new().fg(t.error)),
                EventKind::Interrupt => ("✕ interrupted".into(), Style::new().fg(t.error)),
                EventKind::Branch => ("⑃ branch".into(), dim),
                EventKind::Exit(reason) => (format!("✕ exit {reason}"), dim),
            };
            (format!("{at}  {text}"), style)
        }
        StepRef::Status(i) => {
            let st = &v.lane.steps[i as usize];
            let until = v.end(row.r).unwrap_or(v.now);
            (
                format!("{at}  {} {} {}", status_glyph(st.status), st.status.as_str(), fmt_dur(until - st.at_ms)),
                Style::new().fg(t.status(st.status)),
            )
        }
    }
}

/// Draws the visible rows around the cursor; returns each drawn step's row rect.
fn draw_list(buf: &mut Buffer, area: Rect, tl: &mut TimelineState, v: &View) -> Vec<(Rect, StepRef)> {
    let dim = Style::new().fg(theme().dim);
    let f = tl.frame();
    let mut hits = Vec::new();
    if f.index.rows.is_empty() {
        put_str(buf, area, area.x + 2, area.y, "no steps yet", dim);
        return hits;
    }
    let vis = f.visible(v.session);
    let gap = tl.encoding == Encoding::Gap;
    let height = area.height as usize;
    // Lines from visible row `top`, idle separators included; the cursor stays on screen.
    let lines_from = |top: usize| -> Vec<Result<usize, i64>> {
        let mut out = Vec::new();
        for (n, &k) in vis.iter().enumerate().skip(top) {
            if gap && n > top {
                let idle = f.index.rows[k].at_ms - f.index.rows[vis[n - 1]].at_ms;
                if idle > GAP_MS {
                    out.push(Err(idle));
                }
            }
            out.push(Ok(k));
            if out.len() >= height {
                break;
            }
        }
        out.truncate(height);
        out
    };
    let cur = f.cursor_row().and_then(|c| vis.iter().position(|&k| k >= c)).unwrap_or(0);
    let mut top = f.top.min(cur);
    while top < cur && !lines_from(top).contains(&Ok(vis[cur])) {
        top += 1;
    }
    let lines = lines_from(top);
    let cursor = f.cursor_row();
    for (y, line) in lines.iter().enumerate() {
        let y = area.y + y as u16;
        match *line {
            Err(idle) => {
                put_str(buf, area, area.x + 2, y, &format!("┄┄ {} idle ┄┄", fmt_short(idle)), dim);
            }
            Ok(k) => {
                let row = &f.index.rows[k];
                let (text, style) = row_text(v, tl, row);
                let indent = "  ".repeat(row.depth as usize);
                let selected = Some(k) == cursor;
                hits.push((Rect { y, height: 1, ..area }, row.r));
                let line = format!("{} {indent}{text}", if selected { '▸' } else { ' ' });
                let style = if selected { style.add_modifier(Modifier::REVERSED) } else { style };
                put_str(buf, area, area.x, y, &truncate(&line, area.width as usize), style);
            }
        }
    }
    tl.frame_mut().top = top;
    hits
}

/// Lines describing the step under the cursor.
fn inspect_lines(v: &View, tl: &TimelineState) -> Vec<String> {
    let f = tl.frame();
    let Some(r) = f.cursor else { return Vec::new() };
    let Some(s) = v.session else {
        let StepRef::Status(i) = r else { return Vec::new() };
        let st = &v.lane.steps[i as usize];
        let until = v.end(r).unwrap_or(v.now);
        return vec![
            format!("{} {}", status_glyph(st.status), st.status.as_str()),
            format!("{} · for {}", hms(st.at_ms), fmt_dur(until - st.at_ms)),
            "no session file: status only".into(),
        ];
    };
    let session_cost = || fmt_cost(s.requests.iter().filter_map(|q| q.usage.cost).reduce(|a, b| a + b));
    match r {
        StepRef::Request(i) => {
            let q = &s.requests[i as usize];
            let input = q.usage.input + q.usage.cache_read + q.usage.cache_write;
            let cache = (q.usage.cache_read * 100).checked_div(input).unwrap_or(0);
            let window = s.requests[..=i as usize].iter().rev().find_map(|r| r.ctx_window);
            let bar = window.map(|w| {
                let full = ((q.ctx_tokens as f64 / w.max(1) as f64) * 16.0).round().clamp(0.0, 16.0) as usize;
                format!(" / {} {}{}", fmt_tokens(w), "█".repeat(full), "░".repeat(16 - full))
            });
            let turn = f.index.rows.iter().find(|row| row.r == r).and_then(|row| row.turn);
            vec![
                format!("◆ model · {}", q.model),
                format!("{} · took {}", hms(q.started_ms), fmt_dur(q.ended_ms - q.started_ms)),
                format!("in {} (cache {cache}%) · out {}", fmt_tokens(input), fmt_tokens(q.usage.output)),
                format!("ctx after {}{}", fmt_tokens(q.ctx_tokens), bar.unwrap_or_default()),
                format!(
                    "cost {} · turn {} · session {}",
                    fmt_cost(q.usage.cost),
                    fmt_cost(turn.and_then(|t| v.turn_cost(t))),
                    session_cost()
                ),
            ]
        }
        StepRef::Span(i) => {
            let sp = &s.spans[i as usize];
            let took =
                sp.ended_ms.map_or_else(|| format!("running {}", fmt_dur(v.now - sp.started_ms)), |e| fmt_dur(e - sp.started_ms));
            if let (SpanKind::Subagent, Some(c)) = (sp.kind, sp.child) {
                let child = &v.store.sessions[c];
                return vec![
                    format!("⑂ {} {}", sp.name, child.label.as_deref().unwrap_or("")),
                    format!("{} turns · {} reqs · {}", child.turns.len(), child.requests.len(), fmt_cost(v.tree_cost(c))),
                    format!("took {}", v.span_of(c).map_or("–".into(), |(a, b)| fmt_dur(b - a))),
                    "⏎ open".into(),
                ];
            }
            let (glyph, cat) = match sp.kind {
                SpanKind::Tool(c) => (cat_glyph(c), cat_name(c)),
                SpanKind::Subagent => ('⑂', "subagent"),
                SpanKind::WaitUser => ('▲', "waits on you"),
                SpanKind::WaitAgents => ('⧖', "waits on agents"),
            };
            let exit = sp.exit_code.map(|c| format!(" · exit {c}")).unwrap_or_default();
            let mut out = vec![format!("{glyph} {} · {cat} · {}{exit}", sp.name, status_name(sp.status))];
            if tl.text >= TextLevel::Intent
                && let Some(intent) = sp.intent.and_then(|src| intent_text(s, src))
            {
                out.push(intent);
            }
            out.push(format!("started {} · took {took}", hms(sp.started_ms)));
            out.push(format!("ctx after {}", v.ctx_at(sp.ended_ms.unwrap_or(v.now)).map_or("–".into(), fmt_tokens)));
            let near = |j: Option<usize>| {
                j.and_then(|j| s.spans.get(j)).map_or("—".into(), |o| {
                    let g = match o.kind {
                        SpanKind::Tool(c) => cat_glyph(c),
                        SpanKind::Subagent => '⑂',
                        SpanKind::WaitUser => '▲',
                        SpanKind::WaitAgents => '⧖',
                    };
                    format!("{g} {} {}", o.name, o.ended_ms.map_or("…".into(), |e| fmt_dur(e - o.started_ms)))
                })
            };
            out.push(format!("prev {} · next {}", near((i as usize).checked_sub(1)), near(Some(i as usize + 1))));
            out
        }
        StepRef::Turn(i) => {
            let turn = &s.turns[i as usize];
            let mut out = Vec::new();
            if tl.text == TextLevel::Prompt
                && let Some(p) = turn.prompt.and_then(|src| prompt_text(s, src))
            {
                out.push(p);
            }
            let steps = f.index.rows.iter().filter(|row| row.turn == Some(i) && row.r != r).count();
            let end = turn.ended_ms.unwrap_or(v.now);
            out.push(format!(
                "turn {} · {steps} steps · {} · {}",
                i + 1,
                fmt_cost(v.turn_cost(i)),
                fmt_dur(end - turn.started_ms)
            ));
            out.push(format!("{:?} · {} chars typed", turn.outcome, turn.prompt_chars).to_lowercase());
            out
        }
        StepRef::Event(i) => {
            let e = &s.events[i as usize];
            vec![format!("{:?}", e.kind).to_lowercase(), hms(e.at_ms)]
        }
        StepRef::Status(i) => {
            let st = &v.lane.steps[i as usize];
            vec![format!("{} {} (status)", status_glyph(st.status), st.status.as_str()), hms(st.at_ms)]
        }
    }
}

fn draw_inspector(buf: &mut Buffer, area: Rect, beside: bool, tl: &TimelineState, v: &View) {
    let dim = Style::new().fg(theme().dim);
    let content = if beside {
        for y in area.y..area.bottom() {
            put_str(buf, area, area.x, y, "│", dim);
        }
        Rect { x: area.x + 2, width: area.width.saturating_sub(2), ..area }
    } else {
        put_str(buf, area, area.x, area.y, &"─".repeat(area.width as usize), dim);
        Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area }
    };
    for (k, line) in inspect_lines(v, tl).iter().take(content.height as usize).enumerate() {
        put_str(buf, content, content.x, content.y + k as u16, &truncate(line, content.width as usize), Style::new());
    }
}

fn header(tl: &TimelineState, v: &View, width: u16) -> Vec<Seg> {
    let dim = Style::new().fg(theme().dim);
    let mut parts = vec![v.lane.label().to_string()];
    if let Some((k, n)) = tl.stack[0].root_pos.filter(|&(_, n)| n > 1) {
        parts[0] += &format!(" ({k}/{n})");
    }
    for f in &tl.stack[1..] {
        let label = f.session.and_then(|i| v.store.sessions[i].label.clone()).unwrap_or_else(|| "subagent".into());
        parts.push(format!("⑂ {label}"));
    }
    let mut path = format!("TIMELINE ▸ {}", parts.join(" ▸ "));
    let budget = (width / 2).max(20) as usize;
    while path.chars().count() > budget && parts.len() > 1 {
        parts.remove(0);
        path = format!("TIMELINE ▸ … ▸ {}", parts.join(" ▸ "));
    }
    let bold = Style::new().fg(theme().accent).add_modifier(Modifier::BOLD);
    let mut segs = vec![Seg::left(0, vec![Span::styled(path, bold)])];
    segs.push(Seg::left(
        2,
        vec![if tl.follow {
            Span::styled("live ●", Style::new().fg(theme().working))
        } else {
            Span::styled(format!("⏸ +{} · f", tl.new_since_pause), Style::new().fg(theme().accent))
        }],
    ));
    let f = tl.frame();
    let count = f.index.rows.iter().filter(|r| v.matches(&tl.jump, r.r)).count();
    let jump = if tl.awaiting_jump_kind { "; e b c p s t".to_string() } else { format!("n: {} ({count})", tl.jump.name()) };
    segs.push(Seg::left(4, vec![Span::styled(jump, dim)]));
    segs.push(Seg::left(5, vec![Span::styled(tl.encoding.name(), dim)]));
    segs.push(Seg::right(7, vec![Span::styled("? keys", dim)]));
    segs
}

pub(super) fn draw(buf: &mut Buffer, header_area: Rect, body: Rect, app: &mut App) {
    let Surface::Timeline(mut tl) = std::mem::replace(&mut app.surface, Surface::Main) else { return };
    let Some(v) = view(app, &tl) else { return };
    tl.sync(&v);
    let narrow = app.orient == Orient::Vertical || body.width < 80;
    let (scrub, list, inspector) = if narrow {
        let scrub = Rect { width: 2.min(body.width), ..body };
        let list = Rect { x: body.x + 2, width: body.width.saturating_sub(2), ..body };
        if body.height >= 30 {
            let insp = Rect { y: list.bottom() - 8, height: 8, ..list };
            (scrub, Rect { height: list.height - 8, ..list }, Some((insp, false)))
        } else {
            (scrub, list, None)
        }
    } else {
        let scrub = Rect { height: 1, ..body };
        let rest = Rect { y: body.y + 1, height: body.height.saturating_sub(1), ..body };
        if body.width >= 140 {
            let list = Rect { width: rest.width - SIDE_W, ..rest };
            (scrub, list, Some((Rect { x: list.right(), width: SIDE_W, ..rest }, true)))
        } else if rest.height > 6 {
            let insp = Rect { y: rest.bottom() - 4, height: 4, ..rest };
            (scrub, Rect { height: rest.height - 4, ..rest }, Some((insp, false)))
        } else {
            (scrub, rest, None)
        }
    };
    draw_scrubber(buf, scrub, narrow, &mut tl, &v);
    let hits = draw_list(buf, list, &mut tl, &v);
    if let Some((area, beside)) = inspector {
        draw_inspector(buf, area, beside, &tl, &v);
    }
    draw_header(buf, header_area, header(&tl, &v, header_area.width));
    app.surface = Surface::Timeline(tl);
    app.hits.extend(hits.into_iter().map(|(r, s)| (r, Hit::Step(s))));
}

#[cfg(test)]
mod tests {
    use super::super::tests::{key, press, render, row, sample_app};
    use super::*;
    use crate::session::index::tests::{request, span, src};
    use crate::session::{Fact, Source};

    /// The sample lane with one omp session: a prompt, a request, and a tool that failed.
    fn with_session() -> (App, SessionIdx) {
        let mut app = sample_app(1, Orient::Horizontal);
        // After the sample lane's blocked step at 5 s, so the session's rows are all its own.
        let mut s = Session::new(Source::Omp, "/nowhere.jsonl".into());
        s.apply(Fact::Prompt { at_ms: 6000, trigger: Trigger::User, chars: 10, src: src() });
        s.apply(Fact::Request(request(6100)));
        s.apply(span("a", 6700));
        s.apply(Fact::SpanEnd { id: "a".into(), at_ms: 6800, status: SpanStatus::Ok, exit_code: Some(0) });
        s.apply(span("b", 6900));
        s.apply(Fact::SpanEnd { id: "b".into(), at_ms: 7000, status: SpanStatus::Error, exit_code: Some(1) });
        let id = app.state.lanes[0].id;
        let i = app.sessions.insert_session(id, s);
        (app, i)
    }

    fn timeline(app: &App) -> &TimelineState {
        match &app.surface {
            Surface::Timeline(tl) => tl,
            Surface::Main => panic!("timeline not open"),
        }
    }

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let buf = render(app, w, h);
        (0..h).map(|y| row(&buf, y, 0, w)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn i_opens_the_newest_session_live_on_its_last_step() {
        let (mut app, _) = with_session();
        key(&mut app, 'I');
        let out = screen(&mut app, 120, 14);
        assert!(out.starts_with("TIMELINE ▸ omp"), "{out}");
        assert!(out.contains("live ●"), "{out}");
        assert_eq!(timeline(&app).frame().cursor, Some(StepRef::Span(1)));
        assert!(out.contains("▸     ") && out.contains("$ bash"), "{out}");
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.surface, Surface::Main));
    }

    #[test]
    fn text_stays_hidden_by_default() {
        let (mut app, _) = with_session();
        key(&mut app, 'I');
        let out = screen(&mut app, 120, 14);
        assert!(out.contains("turn 1"), "{out}");
        assert!(!out.contains("fix the login"), "{out}");
    }

    #[test]
    fn sync_appends_without_moving_the_cursor() {
        let (mut app, i) = with_session();
        key(&mut app, 'I');
        screen(&mut app, 120, 14);
        key(&mut app, 'k');
        let cursor = timeline(&app).frame().cursor;
        assert_eq!(cursor, Some(StepRef::Span(0)));
        app.sessions.sessions[i].apply(Fact::Request(request(8000)));
        app.sessions.sessions[i].apply(span("c", 8100));
        let out = screen(&mut app, 120, 14);
        assert_eq!(timeline(&app).frame().cursor, cursor);
        assert_eq!(timeline(&app).new_since_pause, 2);
        assert!(out.contains("⏸ +2 · f"), "{out}");
        key(&mut app, 'f');
        assert_eq!(timeline(&app).frame().cursor, Some(StepRef::Span(2)));
    }

    #[test]
    fn n_jumps_to_the_next_error() {
        let (mut app, _) = with_session();
        key(&mut app, 'I');
        screen(&mut app, 120, 14);
        key(&mut app, 'H');
        assert_eq!(timeline(&app).frame().cursor, Some(StepRef::Turn(0)));
        key(&mut app, 'n');
        assert_eq!(timeline(&app).frame().cursor, Some(StepRef::Span(1)), "errors is the default jump");
        key(&mut app, 'n');
        assert_eq!(timeline(&app).frame().cursor, Some(StepRef::Span(1)), "no wrap");
        for c in [';', 'c'] {
            key(&mut app, c);
        }
        assert!(screen(&mut app, 120, 14).contains("n: compaction (0)"));
    }

    #[test]
    fn folding_a_turn_hides_its_steps() {
        let (mut app, _) = with_session();
        key(&mut app, 'I');
        key(&mut app, 'H');
        key(&mut app, ' ');
        let out = screen(&mut app, 120, 14);
        assert!(out.contains("(folded)") && !out.contains("$ bash"), "{out}");
    }

    #[test]
    fn status_only_lane_lists_status_runs() {
        let mut app = sample_app(1, Orient::Horizontal);
        key(&mut app, 'I');
        let out = screen(&mut app, 120, 14);
        assert!(out.contains("▲ blocked 3s") && out.contains("● working 3s"), "{out}");
    }

    #[test]
    fn gap_encoding_folds_idle_stretches() {
        let min = 60_000;
        let ivs =
            [Interval { from: 0, to: min, phase: Phase::Model }, Interval { from: 41 * min, to: 42 * min, phase: Phase::Tool }];
        let bins = layout_bins(Encoding::Gap, &ivs, 4, 0, 42 * min, 40, true);
        assert!(bins.contains(&Bin::Fold("…40m…".into())), "{bins:?}");
        assert_eq!(bins.iter().map(Bin::width).sum::<usize>(), 40);
        let narrow = layout_bins(Encoding::Gap, &ivs, 4, 0, 42 * min, 40, false);
        assert!(narrow.contains(&Bin::Fold("…".into())));
    }

    #[test]
    fn enter_on_subagent_drills_in_and_bracket_restores() {
        use crate::session::{RefKind, Roots, SessionRef};
        let dir = std::env::temp_dir().join(format!("rex-trail-timeline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("p")).unwrap();
        let call = r#"{"type":"toolCall","id":"toolu_1","name":"task","arguments":{"tasks":[{"name":"Child1"}]}}"#;
        let assistant = |ts: &str, content: &str| {
            format!(
                r#"{{"type":"message","timestamp":"{ts}","message":{{"role":"assistant","stopReason":"toolUse","usage":{{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.5}}}},"content":[{content}]}}}}"#
            )
        };
        let start = r#"{"type":"custom","customType":"tool_execution_start","timestamp":"2026-11-14T10:00:02Z","data":{"toolCallId":"toolu_1","toolName":"task"}}"#;
        std::fs::write(dir.join("p.jsonl"), format!("{}\n{start}\n", assistant("2026-11-14T10:00:01Z", call))).unwrap();
        std::fs::write(dir.join("p/Child1.jsonl"), format!("{}\n", assistant("2026-11-14T10:00:05Z", ""))).unwrap();

        let mut app = sample_app(1, Orient::Horizontal);
        let value = dir.join("p.jsonl").to_string_lossy().to_string();
        app.state.lanes[0].sessions.push(SessionRef { agent: "omp".into(), kind: RefKind::Path, value });
        app.sessions =
            Store::new(Roots { claude_projects: dir.join("x"), codex_sessions: dir.join("y"), codex_index: dir.join("z") });
        app.sessions.refresh(&app.state.lanes, 0);
        key(&mut app, 'I');
        for c in [';', 's', 'H', 'n'] {
            key(&mut app, c);
        }
        assert_eq!(timeline(&app).frame().cursor, Some(StepRef::Span(0)));
        press(&mut app, KeyCode::Enter);
        assert_eq!(timeline(&app).stack.len(), 2);
        assert!(screen(&mut app, 120, 14).starts_with("TIMELINE ▸ omp ▸ ⑂ Child1"));
        key(&mut app, '[');
        assert_eq!(timeline(&app).stack.len(), 1);
        assert_eq!(timeline(&app).frame().cursor, Some(StepRef::Span(0)), "back on the ⑂ row");
        assert!(app.focus_requested.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
