//! Shared frame (one header line over the body), view state, lane layout for both orientations
//! and glyphs. Two modes: LANES (one lane per agent, through a status, heat or context lens, on a
//! time or steps axis) and USAGE (an area chart of context per agent). Lanes are filtered,
//! grouped and sorted rows; the frame's size class decides how much each row carries.

mod area;
mod chrome;
mod ctx;
mod gutter;
mod heat;
mod help;
mod inspect;
mod keys;
mod lanes;
mod layout;
mod prompt;
mod sequence;
mod time;
mod timeline;

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use chrono::{Local, TimeZone};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::config::{config, config_problems, history_ms};
use crate::model::{AgentStatus, Group, Lane, Mark, Query, Row, RowKey, Scope, Sort, State};
use crate::session::index::StepRef;
use crate::session::{Roots, Store, fmt_tokens};
use crate::theme::{theme, theme_problems};
use chrome::Seg;
use gutter::{Cols, display_labels, rollup};
use layout::{SizeClass, size_class};

pub const BUCKETS_S: [i64; 10] = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600];
/// Rows a lane takes when drawn as a usage band; the line sits higher the more context is in use.
const BAND: u16 = 4;
/// Config and theme problems stay in the header this long after start; `?` lists them for good.
const PROBLEMS_MS: i64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One lane per agent.
    Lanes,
    /// Context per agent as an area chart.
    Usage,
}

/// What a lane shows in LANES.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    /// A line in the status color, a node at every change.
    Status,
    /// Working share per time bucket.
    Heat,
    /// Context in use per time bucket.
    Ctx,
}

/// What the status lens lays along the lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Fixed time buckets: length is wall-clock duration.
    Time,
    /// One even slot per state change.
    Steps,
}

/// Whether lanes grow into usage bands (or, in a sidebar, two rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    /// Bigger rows when every lane fits that way.
    Auto,
    Compact,
    Roomy,
}

/// How USAGE lays its series out; `v` toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaMode {
    /// Each series on the ones before it; the top line is the total.
    Stacked,
    /// Every series from zero, largest behind, overlaps blended.
    Overlaid,
}

impl AreaMode {
    fn name(self) -> &'static str {
        match self {
            AreaMode::Stacked => "stacked",
            AreaMode::Overlaid => "overlaid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orient {
    Horizontal,
    Vertical,
}

/// Where a click lands.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Row(RowKey),
    ModeTab(Mode),
    /// The blocked badge.
    Badge,
    /// A timeline list row.
    Step(StepRef),
}

/// The `--view` names, as presets `1`-`5`.
const PRESETS: [&str; 5] = ["time", "sequence", "area", "heat", "dash"];

/// Preset number for a `--view` / `[start] view` name.
pub fn preset_from_name(name: &str) -> Option<u8> {
    PRESETS.iter().position(|&p| p == name).map(|i| i as u8 + 1)
}

pub struct App {
    pub state: State,
    pub mode: Mode,
    pub lens: Lens,
    pub axis: Axis,
    pub density: Density,
    /// Lanes drawn as bands whatever the density.
    pub expanded: BTreeSet<u64>,
    pub orient: Orient,
    pub bucket_idx: usize,
    /// Timed views pick the bucket that fits the history while following; `+`/`-` turn it off, `0` back on.
    pub bucket_auto: bool,
    /// Time axis: the far edge tracks now. Steps axis: no steps hidden from the end.
    pub follow: bool,
    pub view_end_ms: i64,
    /// Steps axis: steps hidden from the end of every lane.
    pub seq_back: usize,
    /// The selected row's key; it stays selected however the rows reorder.
    pub selected: Option<RowKey>,
    /// Row of the last selection, used while the selected row isn't shown.
    sel_hint: usize,
    lane_scroll: usize,
    /// Lanes scrolled out above, drawn, and scrolled out below in the last render.
    shown: (usize, usize, usize),
    /// Largest context on screen when the last render drew usage bands.
    band_max: Option<u64>,
    /// Cells along the lane axis in the last render; scroll steps are a quarter of it.
    time_cells: usize,
    /// The last frame's size class, and whether its group headers are rows.
    class: SizeClass,
    headers: bool,
    pub query: Query,
    pub now_ms: i64,
    started_ms: i64,
    pub error: Option<String>,
    /// A transient header message, and when it expires (ms).
    pub notice: Option<(String, i64)>,
    /// Last snapshot succeeded, so lanes reflect the live panes.
    pub connected: bool,
    /// Running as a popup: Esc closes, and jumping to an agent closes too.
    pub popup: bool,
    /// The `?` overlay is open.
    pub help: bool,
    pub quit: bool,
    pub refresh_requested: bool,
    /// Pane the user asked to jump to; the event loop sends `pane.focus`.
    pub focus_requested: Option<String>,
    pub area_mode: AreaMode,
    pub sessions: Store,
    /// Lane inspector: forced on or off, or None for on when there's room.
    pub inspector: Option<bool>,
    /// The inspector was drawn last frame; `i` flips what's on screen.
    inspector_shown: bool,
    /// One-line text input over the body's last row.
    pub prompt: Option<prompt::Prompt>,
    pub surface: timeline::Surface,
    /// Click targets from the last draw.
    pub hits: Vec<(Rect, Hit)>,
    /// The last left click, for double-clicks.
    last_click: Option<(Hit, Instant)>,
}

impl App {
    pub fn new(state: State, preset: u8, orient: Orient, now_ms: i64) -> Self {
        let mut app = App {
            state,
            mode: Mode::Lanes,
            lens: Lens::Status,
            axis: Axis::Time,
            density: Density::Auto,
            expanded: BTreeSet::new(),
            orient,
            bucket_idx: 4,
            bucket_auto: true,
            follow: true,
            view_end_ms: now_ms,
            seq_back: 0,
            selected: None,
            sel_hint: 0,
            lane_scroll: 0,
            shown: (0, 0, 0),
            band_max: None,
            time_cells: 1,
            class: SizeClass::Panel,
            headers: true,
            query: Query::default(),
            now_ms,
            started_ms: now_ms,
            error: None,
            notice: None,
            connected: false,
            popup: false,
            help: false,
            quit: false,
            refresh_requested: false,
            focus_requested: None,
            area_mode: AreaMode::Stacked,
            sessions: Store::new(Roots::from_env()),
            inspector: None,
            inspector_shown: false,
            prompt: None,
            surface: timeline::Surface::Main,
            hits: Vec::new(),
            last_click: None,
        };
        app.preset(preset);
        app
    }

    /// `1` time, `2` sequence, `3` area, `4` heat, `5` dash: the old views as mode, lens and axis.
    pub fn preset(&mut self, n: u8) {
        (self.mode, self.lens, self.axis) = match n {
            2 => (Mode::Lanes, Lens::Status, Axis::Steps),
            3 => (Mode::Usage, self.lens, self.axis),
            4 => (Mode::Lanes, Lens::Heat, Axis::Time),
            5 => (Mode::Lanes, Lens::Ctx, Axis::Time),
            _ => (Mode::Lanes, Lens::Status, Axis::Time),
        };
        if n == 5 {
            self.bucket_auto = true;
            self.follow = true;
        }
    }

    /// The status lens on the steps axis: cells are steps, not time.
    fn steps(&self) -> bool {
        self.mode == Mode::Lanes && self.lens == Lens::Status && self.axis == Axis::Steps
    }

    /// Reads new usage from every lane's agent sessions (at most once a second), and backdates
    /// first-seen lanes to when their session shows the agent started.
    pub fn refresh_usage(&mut self) {
        self.sessions.refresh(&self.state.lanes, self.now_ms);
        let since = self.now_ms - history_ms();
        let early: Vec<(usize, i64)> = (0..self.state.lanes.len())
            .filter_map(|i| {
                let lane = &self.state.lanes[i];
                let first = self.sessions.first_at(lane)?;
                (first < lane.opened_ms && first >= since).then_some((i, first))
            })
            .collect();
        for (i, t) in early {
            self.state.backdate(i, t);
        }
    }

    fn bucket_ms(&self) -> i64 {
        BUCKETS_S[self.bucket_idx] * 1000
    }

    /// While following with the auto bucket, the smallest bucket whose `n` cells reach back to the
    /// earliest lane (at most the history window), so the strip holds the history and no empty future.
    fn fit_bucket(&mut self, lanes: &[usize], n: usize) {
        if !(self.bucket_auto && self.follow) || n == 0 {
            return;
        }
        let now = self.now_ms;
        let earliest = lanes.iter().map(|&i| self.state.lanes[i].opened_ms).min().unwrap_or(now).max(now - history_ms());
        self.bucket_idx = BUCKETS_S.iter().position(|&b| n as i64 * b * 1000 >= now - earliest).unwrap_or(BUCKETS_S.len() - 1);
    }

    /// The rows the views show, for the last frame's layout; a summary row comes last. Only LANES
    /// rows group: columns and USAGE list every lane.
    pub(crate) fn rows(&self) -> Vec<Row> {
        if self.mode == Mode::Lanes && self.orient == Orient::Horizontal {
            return self.state.rows(&self.query, self.now_ms, config().quiet_after_ms, self.headers);
        }
        let q = Query { group: Group::None, ..self.query.clone() };
        self.state.rows(&q, self.now_ms, config().quiet_after_ms, self.headers)
    }

    /// Highlighted row: the selected key's, else the last selected row (clamped) while it's hidden;
    /// before anything is selected, the first lane rather than a group header.
    pub(crate) fn sel_row(&self, rows: &[Row]) -> Option<usize> {
        let found = self.selected.as_ref().and_then(|k| rows.iter().position(|r| r.key(&self.state).as_ref() == Some(k)));
        let selectable = rows.iter().filter(|r| !matches!(r, Row::Summary { .. })).count();
        let first_lane = || rows.iter().position(|r| matches!(r, Row::Lane(_)));
        let hint = || if self.selected.is_none() { first_lane() } else { None }.unwrap_or(self.sel_hint);
        found.or_else(|| (selectable > 0).then(|| hint().min(selectable - 1)))
    }

    fn select_row(&mut self, rows: &[Row], r: usize) {
        let Some(key) = rows.get(r).and_then(|row| row.key(&self.state)) else { return };
        self.selected = Some(key);
        self.sel_hint = r;
    }

    /// The highlighted lane, when the highlight is on a lane.
    fn selected_lane(&self) -> Option<usize> {
        let rows = self.rows();
        match self.sel_row(&rows).map(|r| &rows[r]) {
            Some(&Row::Lane(i)) => Some(i),
            _ => None,
        }
    }
}

/// Lane indices among `rows`, in order.
fn lanes_of(rows: &[Row]) -> Vec<usize> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Lane(i) => Some(*i),
            _ => None,
        })
        .collect()
}

/// Shape of a status, so state reads without color.
pub fn status_glyph(s: AgentStatus) -> char {
    match s {
        AgentStatus::Working => '●',
        AgentStatus::Blocked => '▲',
        AgentStatus::Done => '✓',
        AgentStatus::Idle => '○',
        AgentStatus::Unknown => '·',
    }
}

pub fn line_glyph(s: AgentStatus, orient: Orient) -> char {
    match (s, orient) {
        (AgentStatus::Working | AgentStatus::Done, Orient::Horizontal) => '━',
        (AgentStatus::Blocked, Orient::Horizontal) => '╍',
        (AgentStatus::Idle, Orient::Horizontal) => '─',
        (AgentStatus::Unknown, Orient::Horizontal) => '┈',
        (AgentStatus::Working | AgentStatus::Done, Orient::Vertical) => '┃',
        (AgentStatus::Blocked, Orient::Vertical) => '╏',
        (AgentStatus::Idle, Orient::Vertical) => '│',
        (AgentStatus::Unknown, Orient::Vertical) => '┆',
    }
}

pub fn node_glyph(mark: Mark) -> (char, Color) {
    match mark {
        Mark::Created => ('◆', theme().created),
        Mark::Seen => ('◇', theme().seen),
        Mark::Status(s) => (status_glyph(s), theme().status(s)),
        Mark::Closed | Mark::Exited => ('✕', theme().closed),
    }
}

fn head_glyph(orient: Orient) -> char {
    match orient {
        Orient::Horizontal => '▸',
        Orient::Vertical => '▾',
    }
}

/// One cell along a lane's time axis.
type Cell = Option<(char, Color)>;

/// One position along a lane: what to draw, how far across the band it sits (0 = least context),
/// and the status of the line through it, which styles any bend into it. `fill` cells cover the
/// whole column width in ↕.
#[derive(Debug, Clone, Copy)]
struct Pt {
    cell: Cell,
    level: u8,
    status: Option<AgentStatus>,
    fill: bool,
}

impl Pt {
    fn new(cell: Cell, level: u8, status: Option<AgentStatus>) -> Pt {
        Pt { cell, level, status, fill: false }
    }
}

/// Maps context tokens to a level across a band, against the largest context on screen.
#[derive(Debug, Clone, Copy)]
struct Scale {
    max: u64,
}

impl Scale {
    fn of(app: &App, lanes: &[usize]) -> Scale {
        let max = lanes
            .iter()
            .filter_map(|&i| {
                let l = &app.state.lanes[i];
                app.sessions.max_between(l, l.opened_ms, l.closed_ms.unwrap_or(app.now_ms))
            })
            .max()
            .unwrap_or(0);
        Scale { max }
    }

    fn level(self, ctx: Option<u64>, band: u16) -> u8 {
        if band <= 1 || self.max == 0 {
            return 0;
        }
        let top = u64::from(band - 1);
        ((ctx.unwrap_or(0).min(self.max) * top + self.max / 2) / self.max) as u8
    }
}

/// Rows lane `lane` takes: a band in the status lens on rows when it has usage and density asks
/// for one (`all_fit` says every lane fits as a band), or it's expanded; else one.
fn row_height(app: &App, lane: &Lane, all_fit: bool) -> u16 {
    let wanted = match app.density {
        Density::Roomy => true,
        Density::Auto => all_fit,
        Density::Compact => false,
    } || app.expanded.contains(&lane.id);
    let band = app.orient == Orient::Horizontal
        && app.class != SizeClass::Sidebar
        && app.lens == Lens::Status
        && wanted
        && app.sessions.has_usage(lane);
    if band { BAND } else { 1 }
}

fn is_line(ch: char) -> bool {
    matches!(ch, '━' | '─' | '┈' | '┃' | '│' | '┆' | '╍' | '╏')
}

/// Corner at the old level, run between, corner at the new level, for a line stepping across the
/// band; `up` means toward more context.
fn bend(up: bool, heavy: bool) -> (char, char, char) {
    match (up, heavy) {
        (true, false) => ('┘', '│', '┌'),
        (true, true) => ('┛', '┃', '┏'),
        (false, false) => ('┐', '│', '└'),
        (false, true) => ('┓', '┃', '┗'),
    }
}

fn line_cell(s: AgentStatus, orient: Orient) -> Cell {
    Some((line_glyph(s, orient), theme().status(s)))
}

fn head_cell(s: AgentStatus, orient: Orient) -> Cell {
    Some((head_glyph(orient), theme().status(s)))
}

fn mark_rank(m: Mark) -> u8 {
    match m {
        Mark::Closed | Mark::Exited => 3,
        Mark::Created | Mark::Seen => 2,
        Mark::Status(_) => 1,
    }
}

/// Node for several steps sharing one cell: lifecycle marks win, ties go to the newest, and a
/// cell holding more than one status change draws `◉`.
fn node_cell(steps: &[crate::model::Step]) -> Cell {
    let (_, winner) = steps.iter().enumerate().max_by_key(|(i, s)| (mark_rank(s.mark), *i))?;
    let (ch, color) = node_glyph(winner.mark);
    let statuses = steps.iter().filter(|s| matches!(s.mark, Mark::Status(_))).count();
    Some((if matches!(winner.mark, Mark::Status(_)) && statuses >= 2 { '◉' } else { ch }, color))
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Writes `s` at (x, y) clipped to `area`; returns the x after the last cell.
pub fn put_str(buf: &mut Buffer, area: Rect, x: u16, y: u16, s: &str, style: Style) -> u16 {
    let mut cx = x;
    if y < area.y || y >= area.bottom() {
        return cx + s.chars().count() as u16;
    }
    for ch in s.chars() {
        if cx >= area.x && cx < area.right() {
            buf[(cx, y)].set_char(ch).set_style(style);
        }
        cx += 1;
    }
    cx
}

/// Writes styled spans from (x, y), at most `max` cells, each over `base`; returns the x after.
fn put_spans(buf: &mut Buffer, area: Rect, x: u16, y: u16, spans: &[Span], max: u16, base: Style) -> u16 {
    let clip = Rect { x, width: max.min(area.right().saturating_sub(x)), ..area };
    let mut cx = x;
    for s in spans {
        cx = put_str(buf, clip, cx, y, &s.content, base.patch(s.style));
    }
    cx
}

fn local(ms: i64, format: &str) -> String {
    Local.timestamp_millis_opt(ms).single().map_or_else(|| "--:--".into(), |t| t.format(format).to_string())
}

fn hms(ms: i64) -> String {
    local(ms, "%H:%M:%S")
}

fn hm(ms: i64) -> String {
    local(ms, "%H:%M")
}

/// Round steps for time-axis ticks, in seconds.
const TICK_STEPS_S: [i64; 15] = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 21600];

/// Ticks for a time axis of `cells` cells of `cell_ms` each from `start_ms`, as (cell, label):
/// the smallest round step whose labels keep apart (a label plus `┊` and a gap across ↔, four rows
/// down ↕), on multiples of the step in local time. Labels read `HH:MM`, with seconds only when
/// the step is under a minute.
fn time_ticks(start_ms: i64, cell_ms: f64, cells: usize, horizontal: bool) -> Vec<(usize, String)> {
    let offset_ms = Local.timestamp_millis_opt(start_ms).single().map_or(0, |t| i64::from(t.offset().local_minus_utc()) * 1000);
    let fits = |step_s: i64| {
        let label_w = if step_s < 60 { 8 } else { 5 };
        let gap = if horizontal { label_w + 3 } else { 4 };
        (step_s * 1000) as f64 >= gap as f64 * cell_ms
    };
    let step_s = TICK_STEPS_S.iter().copied().find(|&s| fits(s)).unwrap_or(TICK_STEPS_S[TICK_STEPS_S.len() - 1]);
    let step = step_s * 1000;
    let format = if step_s < 60 { "%H:%M:%S" } else { "%H:%M" };
    let first = (start_ms + offset_ms + step - 1).div_euclid(step) * step - offset_ms;
    (0..)
        .map(|i| first + i * step)
        .map(|t| (((t - start_ms) as f64 / cell_ms).floor() as usize, t))
        .take_while(|&(cell, _)| cell < cells)
        .map(|(cell, t)| (cell, local(t, format)))
        .collect()
}

fn fmt_bucket(s: i64) -> String {
    if s >= 60 { format!("{}m", s / 60) } else { format!("{s}s") }
}

/// `42s`, `3m`, `2h`, `2h07`: a short span, for gaps and windows.
fn fmt_short(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ if s % 3600 < 60 => format!("{}h", s / 3600),
        _ => format!("{}h{:02}", s / 3600, s % 3600 / 60),
    }
}

/// `42s`, `3m05s`, `2h07m`.
pub fn fmt_dur(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

/// One row of the grid: a lane (with how many rows it takes) or a group header.
#[derive(Debug, Clone, Copy)]
struct Item {
    lane: Option<usize>,
    height: u16,
}

/// Lane placement in the body. `lead` is what precedes the lanes across the lane axis: the time
/// axis row (↔) or the axis gutter (↕). In ↔ each row takes its height, a band is followed by a
/// blank row, and a sidebar's two-row lanes put the strip under the gutter; in ↕ every lane is a
/// column `col_w` wide.
struct Grid {
    orient: Orient,
    body: Rect,
    lead: u16,
    items: Vec<Item>,
    /// ↔ gutter columns.
    cols: Cols,
    /// ↕ column width.
    col_w: u16,
    /// Sidebar lanes: gutter row, then the strip across the width.
    two_row: bool,
}

impl Grid {
    fn new(orient: Orient, body: Rect, lead: u16, items: Vec<Item>, cols: Cols, two_row: bool) -> Grid {
        let col_w = (body.width.saturating_sub(lead) / items.len().max(1) as u16).clamp(6, 18);
        Grid { orient, body, lead, items, cols, col_w, two_row }
    }

    /// Cells along the time axis.
    fn n(&self) -> usize {
        match self.orient {
            Orient::Horizontal if self.two_row => self.body.width.saturating_sub(2) as usize,
            Orient::Horizontal => self.cols.strip(self.body.width) as usize,
            Orient::Vertical => self.body.height.saturating_sub(1) as usize,
        }
    }

    /// First column of the strip (↔).
    fn strip_x(&self) -> u16 {
        self.body.x + if self.two_row { 2 } else { self.cols.left() }
    }

    /// Cells row `k` takes across the lanes, and what it is followed by.
    fn size(&self, k: usize) -> (u16, u16) {
        let item = self.items[k];
        match self.orient {
            Orient::Horizontal => (item.height, u16::from(item.height > 1 && !self.two_row)),
            Orient::Vertical => (self.col_w, 0),
        }
    }

    /// Rows from `scroll` that fit whole, with where each starts.
    fn placed(&self, scroll: usize) -> Vec<(usize, u16)> {
        let (origin, space) = match self.orient {
            Orient::Horizontal => (self.body.y + self.lead, self.body.height.saturating_sub(self.lead)),
            Orient::Vertical => (self.body.x + self.lead, self.body.width.saturating_sub(self.lead)),
        };
        let mut at = 0;
        let mut out = Vec::new();
        for k in scroll..self.items.len() {
            let (size, gap) = self.size(k);
            if at + size > space {
                break;
            }
            out.push((k, origin + at));
            at += size + gap;
        }
        out
    }

    /// Scroll that shows row `sel` whole and as many rows as fit around it.
    fn scroll_for(&self, scroll: usize, sel: usize) -> usize {
        let shows = |s: usize, k: usize| self.placed(s).iter().any(|&(r, _)| r == k);
        let mut s = scroll.min(sel);
        while s < sel && !shows(s, sel) {
            s += 1;
        }
        // Scrolled further than needed (rows went away): pull back while nothing drops off.
        while s > 0 {
            let last = self.placed(s).last().map(|&(r, _)| r);
            if last.is_some_and(|l| shows(s - 1, l)) && shows(s - 1, sel) {
                s -= 1;
            } else {
                break;
            }
        }
        s
    }

    fn cell_pos(&self, start: u16, band: u16, i: usize, level: u8) -> (u16, u16) {
        match self.orient {
            Orient::Horizontal => (self.strip_x() + i as u16, start + u16::from(self.two_row) + (band - 1 - u16::from(level))),
            Orient::Vertical => (start + 2, self.body.y + 1 + i as u16),
        }
    }

    /// Where the time-axis label for cell `i` starts.
    fn axis_pos(&self, i: usize) -> (u16, u16) {
        match self.orient {
            Orient::Horizontal => (self.strip_x() + i as u16, self.body.y),
            Orient::Vertical => (self.body.x, self.body.y + 1 + i as u16),
        }
    }

    /// Where time-axis labels may draw: the strip's span (↔) or the axis gutter (↕).
    fn axis_area(&self) -> Rect {
        match self.orient {
            Orient::Horizontal => Rect::new(self.strip_x(), self.body.y, self.n() as u16, 1),
            Orient::Vertical => Rect::new(self.body.x, self.body.y, self.lead, self.body.height),
        }
    }

    fn put(&self, buf: &mut Buffer, (x, y): (u16, u16), ch: char, color: Color) {
        if x < self.body.right() && y < self.body.bottom() {
            buf[(x, y)].set_char(ch).set_style(Style::new().fg(color));
        }
    }

    /// A group header row: `─ label ─────── ▲1 ●2 ─`, or `▸ label · n agents ▲1` when folded.
    fn draw_group(&self, buf: &mut Buffer, y: u16, row: &Row, selected: bool) {
        let Row::Group { label, counts, collapsed, .. } = row else { return };
        let dim = Style::new().fg(theme().dim);
        let base = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
        let roll = rollup(counts);
        let roll_w: u16 = roll.iter().map(|s| s.content.chars().count() as u16).sum();
        let (x, w) = (self.body.x, self.body.width);
        if *collapsed {
            let n: usize = counts.iter().sum();
            let at = put_str(buf, self.body, x, y, &format!("▸ {label} · {n} agents "), base.patch(dim));
            put_spans(buf, self.body, at, y, &roll, w.saturating_sub(at - x), base);
            return;
        }
        let head = format!("─ {label} ");
        let at = put_str(buf, self.body, x, y, &truncate(&head, w.saturating_sub(roll_w + 4) as usize), base.patch(dim));
        let fill_to = (x + w).saturating_sub(roll_w + 3);
        let at = put_str(buf, self.body, at, y, &"─".repeat(fill_to.saturating_sub(at) as usize + 1), base.patch(dim));
        let at = put_spans(buf, self.body, at, y, &roll, roll_w + 1, base);
        put_str(buf, self.body, at, y, " ─", base.patch(dim));
    }

    /// Draws every placed row: group headers, and each lane's gutter (↔) or column header (↕)
    /// with its cells from `pts(lane, store, n, band)`, scrolling so the selected row stays in
    /// view. Where the level changes between cells, the line bends across the band in the new
    /// cell. In a band the group column labels each group's first lane and rolls it up on the
    /// second.
    fn draw_lanes(&self, buf: &mut Buffer, app: &mut App, rows: &[Row], pts: impl Fn(&Lane, &Store, usize, u16) -> Vec<Pt>) {
        let sel = app.sel_row(rows);
        app.lane_scroll = self.scroll_for(app.lane_scroll, sel.unwrap_or(0));
        let placed = self.placed(app.lane_scroll);
        let lane_before = |k: usize| self.items[..k].iter().filter(|i| i.lane.is_some()).count();
        let total = lane_before(self.items.len());
        let above = lane_before(app.lane_scroll);
        let shown = placed.iter().filter(|&&(k, _)| self.items[k].lane.is_some()).count();
        app.shown = (above, shown, total - above - shown);

        let lanes = lanes_of(rows);
        let labels: HashMap<usize, String> = lanes.iter().copied().zip(display_labels(&app.state, &lanes)).collect();
        // Band group column: each group's first lane names it, its second rolls it up.
        let mut group_cell: HashMap<usize, Result<String, [usize; 5]>> = HashMap::new();
        if self.cols.group_w > 0 {
            let mut seen: HashMap<String, (usize, [usize; 5])> = HashMap::new();
            for &li in &lanes {
                let (key, _) = app.state.group_of(li, app.query.group);
                let lane = &app.state.lanes[li];
                let entry = seen.entry(key).or_insert((0, [0; 5]));
                entry.0 += 1;
                if lane.is_open() {
                    entry.1[crate::model::count_slot(lane.status)] += 1;
                }
            }
            let mut nth: HashMap<String, usize> = HashMap::new();
            for &li in &lanes {
                let (key, label) = app.state.group_of(li, app.query.group);
                let k = nth.entry(key.clone()).or_default();
                *k += 1;
                match *k {
                    1 => group_cell.insert(li, Ok(label)),
                    2 => group_cell.insert(li, Err(seen[&key].1)),
                    _ => None,
                };
            }
        }

        let n = self.n();
        let dim = Style::new().fg(theme().dim);
        for (k, start) in placed {
            let selected = Some(k) == sel;
            let (size, _) = self.size(k);
            let rect = match self.orient {
                Orient::Horizontal => Rect::new(self.body.x, start, self.body.width, size),
                Orient::Vertical => Rect::new(start, self.body.y, size, self.body.height),
            };
            if let Some(key) = rows[k].key(&app.state) {
                app.hits.push((rect, Hit::Row(key)));
            }
            let Row::Lane(li) = rows[k] else {
                self.draw_group(buf, start, &rows[k], selected);
                continue;
            };
            let lane = &app.state.lanes[li];
            let label = labels.get(&li).map_or("", String::as_str);
            let band = match self.orient {
                Orient::Horizontal if !self.two_row => self.items[k].height,
                _ => 1,
            };
            match self.orient {
                Orient::Horizontal => {
                    match group_cell.get(&li) {
                        Some(Ok(name)) => {
                            put_str(buf, self.body, self.body.x, start, &truncate(name, (self.cols.group_w - 1) as usize), dim);
                        }
                        Some(Err(counts)) => {
                            put_spans(buf, self.body, self.body.x, start, &rollup(counts), self.cols.group_w - 1, Style::new());
                        }
                        None => {}
                    }
                    gutter::draw_row(buf, self.body, &self.cols, app, li, label, start, band, selected);
                }
                Orient::Vertical => {
                    let sel_style = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                    let (glyph, gs) = if lane.is_open() {
                        (status_glyph(lane.status), Style::new().fg(theme().status(lane.status)))
                    } else {
                        ('✕', dim)
                    };
                    put_str(buf, self.body, start, self.body.y, &glyph.to_string(), gs.patch(sel_style));
                    let w = usize::from(self.col_w - 2);
                    let label_style = if lane.is_open() { Style::new() } else { dim };
                    put_str(
                        buf,
                        self.body,
                        start + 1,
                        self.body.y,
                        &format!("{:<w$}", truncate(label, w)),
                        label_style.patch(sel_style),
                    );
                }
            }
            let mut prev: Option<u8> = None;
            for (i, pt) in pts(lane, &app.sessions, n, band).into_iter().enumerate().take(n) {
                let Some((mut ch, mut color)) = pt.cell else {
                    prev = None;
                    continue;
                };
                if pt.fill && self.orient == Orient::Vertical {
                    let y = self.body.y + 1 + i as u16;
                    for x in start + 1..start + self.col_w {
                        self.put(buf, (x, y), ch, color);
                    }
                    continue;
                }
                if let Some(p) = prev
                    && p != pt.level
                {
                    let s = pt.status.unwrap_or_default();
                    let heavy = matches!(s, AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done);
                    let c = theme().status(s);
                    let (from, mid, to) = bend(pt.level > p, heavy);
                    self.put(buf, self.cell_pos(start, band, i, p), from, c);
                    for l in p.min(pt.level) + 1..p.max(pt.level) {
                        self.put(buf, self.cell_pos(start, band, i, l), mid, c);
                    }
                    if is_line(ch) {
                        (ch, color) = (to, c);
                    }
                }
                self.put(buf, self.cell_pos(start, band, i, pt.level), ch, color);
                prev = Some(pt.level);
            }
        }
    }
}

/// The lens (and its scale) for the header: `status · time`, `heat` with its key, `ctx`, or the
/// USAGE layout, then the bucket and band scale.
fn lens_spans(app: &App) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme().dim);
    let mut spans = vec![Span::styled(
        match (app.mode, app.lens, app.axis) {
            (Mode::Usage, ..) => app.area_mode.name().to_string(),
            (Mode::Lanes, Lens::Status, Axis::Time) => "status · time".into(),
            (Mode::Lanes, Lens::Status, Axis::Steps) => "status · steps".into(),
            (Mode::Lanes, Lens::Heat, _) => "heat".into(),
            (Mode::Lanes, Lens::Ctx, _) => "ctx".into(),
        },
        dim,
    )];
    if app.mode == Mode::Lanes && app.lens == Lens::Heat {
        spans.push(Span::styled(" 0% ", dim));
        for i in 0..8 {
            spans.push(Span::styled("▀", Style::new().fg(theme().heat_color(f64::from(i) / 7.0))));
        }
        spans.push(Span::styled(" 100% working ", dim));
        spans.push(Span::styled("■ blocked", Style::new().fg(theme().heat_blocked)));
    }
    let fit = app.bucket_auto && app.follow;
    let bucket = fmt_bucket(BUCKETS_S[app.bucket_idx]);
    let scale = match app.mode {
        _ if app.steps() => String::new(),
        Mode::Usage if fit => "fit".into(),
        Mode::Lanes if fit => format!("fit {bucket}"),
        _ => bucket,
    };
    if !scale.is_empty() {
        spans.push(Span::styled(format!(" · {scale}"), dim));
    }
    if let Some(max) = app.band_max {
        spans.push(Span::styled(format!(" · ctx≤{}", fmt_tokens(max)), dim));
    }
    spans
}

/// Non-default query settings for the header: `active · ws · sort recent · by tab · /omp`.
fn query_text(q: &Query) -> String {
    let mut parts: Vec<String> = Vec::new();
    if q.active_only {
        parts.push("active".into());
    }
    match q.scope {
        Scope::All => {}
        Scope::Workspace => parts.push("ws".into()),
        Scope::Tab => parts.push("tab".into()),
    }
    match q.sort {
        Sort::Stable => {}
        Sort::Attention => parts.push("sort attention".into()),
        Sort::Recent => parts.push("sort recent".into()),
        Sort::Name => parts.push("sort name".into()),
    }
    match q.group {
        Group::Workspace => {}
        Group::Tab => parts.push("by tab".into()),
        Group::None => parts.push("ungrouped".into()),
    }
    if !q.search.is_empty() {
        parts.push(format!("/{}", q.search));
    }
    parts.join(" · ")
}

/// Open blocked agent lanes, oldest wait first, whatever the filters.
fn blocked_lanes(app: &App) -> Vec<usize> {
    let mut out: Vec<usize> = (0..app.state.lanes.len())
        .filter(|&i| {
            let l = &app.state.lanes[i];
            l.visible() && l.is_open() && l.status == AgentStatus::Blocked
        })
        .collect();
    out.sort_by_key(|&i| app.state.lanes[i].last_ms());
    out
}

/// Header segments, by priority: error, blocked badge and mode, counts, paused, selected and
/// query, lens, problems, help; with which segments are the badge and the mode tabs.
fn header(app: &App, lanes: &[usize]) -> (Vec<Seg>, Option<usize>, usize) {
    let dim = Style::new().fg(theme().dim);
    let error = Style::new().fg(theme().error);
    let mut segs = Vec::new();
    if let Some(err) = &app.error {
        segs.push(Seg::left(0, vec![Span::styled(format!("⚠ rex: {err} — retrying"), error)]));
    }
    if let Some((text, _)) = app.notice.as_ref().filter(|(_, until)| app.now_ms < *until) {
        segs.push(Seg::left(0, vec![Span::styled(text.clone(), Style::new().fg(theme().accent))]));
    }
    let blocked = blocked_lanes(app);
    let badge = blocked.first().map(|&oldest| {
        let age = fmt_dur(app.now_ms - app.state.lanes[oldest].last_ms());
        segs.push(Seg::left(
            1,
            vec![Span::styled(
                format!("▲ {} blocked {age}", blocked.len()),
                Style::new().fg(theme().blocked).add_modifier(Modifier::BOLD),
            )],
        ));
        segs.len() - 1
    });

    let active = Style::new().fg(theme().accent).add_modifier(Modifier::BOLD);
    let tab = |name: &'static str, mode: Mode| {
        if app.mode == mode { Span::styled(name.to_uppercase(), active) } else { Span::styled(name, dim) }
    };
    let mut modes = vec![tab("lanes", Mode::Lanes), Span::raw(" "), tab("usage", Mode::Usage)];
    if app.orient == Orient::Vertical {
        modes.push(Span::raw(" ↕"));
    }
    segs.push(Seg::left(1, modes));
    let modes = segs.len() - 1;

    let (above, shown, below) = app.shown;
    let mut counts = format!("{shown}/{}", lanes.len());
    if above > 0 {
        counts += &format!(" ↑{above}");
    }
    if below > 0 {
        counts += &format!(" ↓{below}");
    }
    segs.push(Seg::left(2, vec![Span::raw(counts)]));

    if !app.follow {
        let paused =
            if app.steps() { format!("⏸ −{} · f", app.seq_back) } else { format!("⏸ {} · f", hms(app.view_end_ms)) };
        segs.push(Seg::left(3, vec![Span::styled(paused, Style::new().fg(theme().accent))]));
    }
    let query = query_text(&app.query);
    if !query.is_empty() {
        segs.push(Seg::left(4, vec![Span::styled(query, Style::new().fg(theme().accent))]));
    }

    segs.push(Seg::left(5, lens_spans(app)));

    if app.now_ms - app.started_ms < PROBLEMS_MS {
        let mut spans = Vec::new();
        for (file, problems) in [("theme.toml", theme_problems()), ("config.toml", config_problems())] {
            if let [first, rest @ ..] = problems {
                let more = if rest.is_empty() { String::new() } else { format!(" (+{} more)", rest.len()) };
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", dim));
                }
                spans.push(Span::styled(format!("{file} {first}{more}"), error));
            }
        }
        if !spans.is_empty() {
            segs.push(Seg::left(6, spans));
        }
    }

    if let Some(li) = app.selected_lane() {
        let lane = &app.state.lanes[li];
        segs.push(Seg::right(
            4,
            vec![
                Span::raw(lane.label().to_string()),
                Span::styled(format!(" · {} · {}", app.state.workspace_label(li), lane.agent.as_deref().unwrap_or("-")), dim),
            ],
        ));
    }
    segs.push(Seg::right(7, vec![Span::styled("? keys", dim)]));
    (segs, badge, modes)
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App) {
    let [header_area, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    app.hits.clear();
    let in_timeline = matches!(app.surface, timeline::Surface::Timeline(_));
    if in_timeline {
        timeline::draw(buf, header_area, body, app);
    } else {
        draw_main(buf, area, header_area, body, app);
    }
    if let Some(p) = &app.prompt {
        prompt::draw(buf, body, p);
    }
    if app.help {
        help::draw(buf, body, in_timeline);
    }
}

fn draw_main(buf: &mut Buffer, area: Rect, header_area: Rect, body: Rect, app: &mut App) {
    let dim = Style::new().fg(theme().dim);
    app.class = size_class(area);
    // A band keeps groups in a column beside the gutter; other shapes give them header rows.
    app.headers = !(app.class == SizeClass::Band && app.query.group != Group::None && area.width >= 100);
    if !app.steps() && app.follow {
        app.view_end_ms = app.now_ms;
    }
    app.band_max = None;
    let mut rows = app.rows();
    let summary = match rows.last() {
        Some(&Row::Summary { closed, quiet }) => {
            rows.pop();
            Some((closed, quiet))
        }
        _ => None,
    };
    let lanes = lanes_of(&rows);
    app.shown = (0, lanes.len(), 0);
    let has_lane = app.selected_lane().is_some();
    let (body, inspector) = match inspect::place(app, body, rows.len()).filter(|_| has_lane) {
        Some((main, side, bottom)) => (main, Some((side, bottom))),
        None => (body, None),
    };
    app.inspector_shown = inspector.is_some();

    // What the filters hide folds into one summary row at the bottom (USAGE has no rows to fold).
    let summary = summary.filter(|_| app.mode == Mode::Lanes && body.height > 1);
    let lane_body = if summary.is_some() { Rect { height: body.height - 1, ..body } } else { body };
    if rows.is_empty() {
        if summary.is_none() {
            put_str(buf, body, body.x, body.y, "no agents yet — start omp, Claude Code or Codex in a rex pane", dim);
        }
    } else {
        match app.mode {
            Mode::Lanes => lanes::render(buf, lane_body, app, &rows),
            Mode::Usage => area::render(buf, lane_body, app, &lanes),
        }
    }
    if let Some((closed, quiet)) = summary {
        let mut parts = Vec::new();
        if closed > 0 {
            parts.push(format!("{closed} closed in the last {} — c shows them", fmt_short(history_ms())));
        }
        if quiet > 0 {
            parts.push(format!("{quiet} quiet — a shows them"));
        }
        put_str(buf, body, body.x, body.bottom() - 1, &format!("▸ {}", parts.join(" · ")), dim);
    }
    if let Some((area, bottom)) = inspector
        && let Some(li) = app.selected_lane()
    {
        inspect::draw(buf, area, bottom, app, li);
    }

    let (segs, badge, modes) = header(app, &lanes);
    let rects = chrome::draw_header(buf, header_area, segs);
    if let Some(r) = badge.and_then(|b| rects[b]) {
        app.hits.push((r, Hit::Badge));
    }
    if let Some(r) = rects[modes] {
        app.hits.push((Rect { width: 5, ..r }, Hit::ModeTab(Mode::Lanes)));
        app.hits.push((Rect { x: r.x + 6, width: 5, ..r }, Hit::ModeTab(Mode::Usage)));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Step;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Lane opened at 0 (Created, idle), working at 2000, blocked at 5000; `now_ms = 8500`.
    /// Ungrouped, so lane rows sit where the layout tests expect them.
    pub fn sample_app(preset: u8, orient: Orient) -> App {
        let mut state = State::default();
        let i = state.open("w1:p1", "w1", 0, Mark::Created, AgentStatus::Idle, true);
        state.lanes[i].agent = Some("omp".into());
        state.set_status(i, AgentStatus::Working, 2000);
        state.set_status(i, AgentStatus::Blocked, 5000);
        state.assign_slots();
        let mut app = App::new(state, preset, orient, 8500);
        app.query.group = Group::None;
        app
    }

    pub fn row(buf: &Buffer, y: u16, x0: u16, x1: u16) -> String {
        (x0..x1).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()
    }

    pub fn col(buf: &Buffer, x: u16, y0: u16, y1: u16) -> Vec<String> {
        (y0..y1).map(|y| buf[(x, y)].symbol().to_string()).collect()
    }

    pub fn render(app: &mut App, w: u16, h: u16) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        draw(&mut buf, Rect::new(0, 0, w, h), app);
        buf
    }

    pub fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    pub fn key(app: &mut App, c: char) {
        press(app, KeyCode::Char(c));
    }

    pub fn add_lane(app: &mut App, pane: &str, status: AgentStatus) -> usize {
        let i = app.state.open(pane, "w1", 100, Mark::Seen, AgentStatus::Unknown, true);
        app.state.lanes[i].agent = Some("omp".into());
        app.state.set_status(i, status, 200);
        app.state.assign_slots();
        i
    }

    pub fn two_lanes(orient: Orient) -> App {
        let mut app = sample_app(1, orient);
        add_lane(&mut app, "w1:p2", AgentStatus::Idle);
        app
    }

    pub fn sel(app: &App) -> Option<usize> {
        app.sel_row(&app.rows())
    }

    #[test]
    fn selection_stays_on_its_lane_when_a_new_lane_sorts_before_it() {
        let mut state = State::default();
        let b = state.open("b:p1", "b", 0, Mark::Seen, AgentStatus::Idle, true);
        state.lanes[b].agent = Some("omp".into());
        let mut app = App::new(state, 1, Orient::Horizontal, 1000);
        app.query.group = Group::None;
        let rows = app.rows();
        app.select_row(&rows, 0);
        let key = app.selected.clone();
        let a = app.state.open("a:p1", "a", 10, Mark::Seen, AgentStatus::Idle, true);
        app.state.lanes[a].agent = Some("omp".into());
        assert_eq!(app.selected, key);
        assert_eq!(sel(&app), Some(1), "the highlight follows the lane to its new row");
    }

    #[test]
    fn every_status_has_its_own_shape() {
        use AgentStatus::*;
        let mut state = State::default();
        for (k, s) in [Working, Blocked, Done, Idle, Unknown].into_iter().enumerate() {
            let i = state.open(&format!("w1:p{k}"), "w1", k as i64, Mark::Seen, Unknown, true);
            state.lanes[i].agent = Some("omp".into());
            state.set_status(i, s, 1000);
        }
        state.assign_slots();
        let mut app = App::new(state, 1, Orient::Horizontal, 8500);
        app.query.group = Group::None;
        let buf = render(&mut app, 80, 8);
        let glyphs: Vec<String> = (2..7).map(|y| buf[(1, y)].symbol().to_string()).collect();
        assert_eq!(glyphs, ["●", "▲", "✓", "○", "·"]);
        assert!(row(&buf, 3, 33, 62).contains('╍'), "{}", row(&buf, 3, 33, 62));
    }

    #[test]
    fn every_agent_row_shows_its_state() {
        let mut app = two_lanes(Orient::Horizontal);
        let buf = render(&mut app, 80, 6);
        assert!(row(&buf, 2, 0, 33).contains("blocked 3s"), "{}", row(&buf, 2, 0, 33));
        assert!(row(&buf, 3, 0, 33).contains("idle 8s"), "{}", row(&buf, 3, 0, 33));
    }

    #[test]
    fn closed_lanes_fold_into_a_summary_row() {
        let mut app = two_lanes(Orient::Horizontal);
        app.state.close("w1:p2", Mark::Closed, 8000);
        let buf = render(&mut app, 80, 6);
        assert!(row(&buf, 5, 0, 80).starts_with("▸ 1 closed"), "{}", row(&buf, 5, 0, 80));
        assert_eq!(row(&buf, 3, 0, 80), "", "the closed lane's row is gone");
        key(&mut app, 'c');
        let buf = render(&mut app, 80, 6);
        assert!(row(&buf, 3, 0, 33).contains("closed"), "{}", row(&buf, 3, 0, 33));
        assert!(!row(&buf, 5, 0, 80).starts_with('▸'));
    }

    #[test]
    fn hidden_lanes_are_counted() {
        let mut app = sample_app(1, Orient::Horizontal);
        for k in 2..=6 {
            add_lane(&mut app, &format!("w1:p{k}"), AgentStatus::Idle);
        }
        let buf = render(&mut app, 80, 5);
        assert!(row(&buf, 0, 0, 80).contains("3/6 ↓3"), "{}", row(&buf, 0, 0, 80));
    }

    #[test]
    fn header_names_the_mode_and_lens() {
        let mut app = sample_app(1, Orient::Horizontal);
        let header = row(&render(&mut app, 120, 4), 0, 0, 120);
        assert!(header.starts_with("▲ 1 blocked 3s │ LANES usage"), "{header}");
        assert!(header.contains("status · time · fit 1s"), "{header}");
        assert!(header.ends_with("? keys"), "{header}");
        press(&mut app, KeyCode::Tab);
        let header = row(&render(&mut app, 120, 4), 0, 0, 120);
        assert!(header.contains("lanes USAGE") && header.contains("stacked"), "{header}");
    }

    #[test]
    fn presets_match_old_view_names() {
        let mut app = sample_app(1, Orient::Horizontal);
        app.preset(preset_from_name("dash").unwrap());
        assert_eq!((app.mode, app.lens), (Mode::Lanes, Lens::Ctx));
        app.preset(preset_from_name("sequence").unwrap());
        assert_eq!((app.mode, app.lens, app.axis), (Mode::Lanes, Lens::Status, Axis::Steps));
        app.preset(preset_from_name("area").unwrap());
        assert_eq!(app.mode, Mode::Usage);
        assert_eq!(preset_from_name("nope"), None);
    }

    #[test]
    fn auto_density_bands_lanes_only_when_they_fit() {
        let mut app = sample_app(1, Orient::Horizontal);
        add_lane(&mut app, "w1:p2", AgentStatus::Idle);
        for i in 0..2 {
            let id = app.state.lanes[i].id;
            app.sessions.insert_samples(id, &[(0, 100, None), (4000, 400, None)]);
        }
        render(&mut app, 100, 14);
        assert_eq!(app.band_max, Some(400), "two lanes fit as bands");
        for k in 3..=5 {
            let i = add_lane(&mut app, &format!("w1:p{k}"), AgentStatus::Idle);
            let id = app.state.lanes[i].id;
            app.sessions.insert_samples(id, &[(0, 100, None)]);
        }
        let buf = render(&mut app, 100, 14);
        assert_eq!(app.band_max, None, "five don't");
        assert_eq!(app.shown, (0, 5, 0));
        key(&mut app, ' ');
        let buf2 = render(&mut app, 100, 14);
        assert_eq!(app.band_max, Some(400), "Space expands the selected lane");
        assert_ne!(row(&buf, 3, 0, 100), row(&buf2, 3, 0, 100), "the second lane moved down");
        assert!(row(&buf2, 3, 3, 30).starts_with("ctx 400"), "{}", row(&buf2, 3, 0, 100));
        assert_eq!(row(&buf2, 6, 0, 30), "", "a blank row follows the band");
        assert!(row(&buf2, 7, 0, 30).contains("omp"), "then the next lane");
    }

    /// Eight lanes over two workspaces; the last one blocked.
    fn eight_lanes() -> App {
        let mut app = sample_app(1, Orient::Horizontal);
        app.state.set_status(0, AgentStatus::Idle, 6000);
        for k in 2..=8 {
            let i = add_lane(&mut app, &format!("w1:p{k}"), if k == 8 { AgentStatus::Blocked } else { AgentStatus::Idle });
            app.state.lanes[i].opened_ms = k as i64 * 10;
        }
        app
    }

    #[test]
    fn sticky_line_when_blocked_is_scrolled_out() {
        let mut app = eight_lanes();
        let buf = render(&mut app, 100, 6);
        let first = row(&buf, 1, 0, 100);
        assert!(first.starts_with("▲ omp"), "{first}");
        assert!(first.contains("blocked 8s · b"), "{first}");
        key(&mut app, 'b');
        let buf = render(&mut app, 100, 6);
        assert!(!row(&buf, 1, 0, 100).starts_with('▲'), "the blocked lane is in view now");
    }

    #[test]
    fn b_selects_the_next_blocked_lane() {
        let mut app = eight_lanes();
        app.state.set_status(2, AgentStatus::Blocked, 300);
        render(&mut app, 100, 14);
        key(&mut app, 'b');
        assert_eq!(app.selected_lane(), Some(2));
        key(&mut app, 'b');
        assert_eq!(app.selected_lane(), Some(7));
        key(&mut app, 'b');
        assert_eq!(app.selected_lane(), Some(2), "wraps");
    }

    #[test]
    fn sidebar_rows_keep_state_and_ctx() {
        let mut app = two_lanes(Orient::Horizontal);
        let id = app.state.lanes[1].id;
        app.sessions.insert_samples(id, &[(150, 52_000, None)]);
        app.query.group = Group::Workspace;
        let buf = render(&mut app, 50, 20);
        assert!(row(&buf, 2, 0, 50).starts_with("─ w1 "), "{}", row(&buf, 2, 0, 50));
        // Two rows per lane: the gutter, then the strip under it.
        let first = row(&buf, 3, 0, 50);
        assert!(first.contains("blocked 3s") && first.ends_with('–'), "{first}");
        assert!(row(&buf, 4, 2, 50).ends_with('▸'), "{}", row(&buf, 4, 0, 50));
        let second = row(&buf, 5, 0, 50);
        assert!(second.contains("idle 8s") && second.ends_with("52k"), "{second}");
    }

    #[test]
    fn band_groups_sit_in_a_column() {
        let mut app = two_lanes(Orient::Horizontal);
        app.query.group = Group::Workspace;
        let buf = render(&mut app, 120, 6);
        assert!(row(&buf, 2, 0, 10).starts_with("w1"), "{}", row(&buf, 2, 0, 120));
        assert!(row(&buf, 3, 0, 10).starts_with("▲1 ○1"), "{}", row(&buf, 3, 0, 120));
        assert_eq!(buf[(11, 2)].symbol(), "▲", "the gutter starts after the group column");
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(fmt_dur(42_000), "42s");
        assert_eq!(fmt_dur(185_000), "3m05s");
        assert_eq!(fmt_dur(7_620_000), "2h07m");
    }

    #[test]
    fn time_ticks_pick_round_steps_that_keep_labels_apart() {
        // 15 s cells across ↔: `┊HH:MM` plus a gap needs 8 cells, so ticks every 2 minutes.
        let start = 1_794_529_561_427;
        let ticks = time_ticks(start, 15_000.0, 100, true);
        assert!(ticks.len() >= 11, "{ticks:?}");
        assert!(ticks.windows(2).all(|w| w[1].0 - w[0].0 == 8), "{ticks:?}");
        assert!(ticks.iter().all(|(_, l)| l.len() == 5 && l[3..].parse::<u32>().unwrap() % 2 == 0), "{ticks:?}");
        // 1 s cells: seconds show, every 15 s (8 + 3 cells needed).
        let ticks = time_ticks(start, 1000.0, 60, true);
        assert!(ticks.iter().all(|(_, l)| l.len() == 8 && l[6..].parse::<u32>().unwrap() % 15 == 0), "{ticks:?}");
        // Down ↕ only rows matter: 4 rows of 15 s → every minute.
        let ticks = time_ticks(start, 15_000.0, 40, false);
        assert!(ticks.windows(2).all(|w| w[1].0 - w[0].0 == 4), "{ticks:?}");
    }

    #[test]
    fn crowded_cell_prefers_lifecycle_then_counts_statuses() {
        use AgentStatus::*;
        let s = |at_ms, mark, status| Step { at_ms, mark, status };
        let both = [s(0, Mark::Status(Working), Working), s(1, Mark::Status(Blocked), Blocked)];
        assert_eq!(node_cell(&both), Some(('◉', theme().blocked)));
        let closed = [s(0, Mark::Status(Working), Working), s(1, Mark::Closed, Working)];
        assert_eq!(node_cell(&closed), Some(('✕', theme().closed)));
        assert_eq!(node_cell(&[]), None);
    }
}
