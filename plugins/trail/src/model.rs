//! The trail state: one lane per pane lifetime, one step per state change, folded from the history
//! log, snapshots and live events through one `HostEvent` fold.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::config::history_ms;
use crate::rex::Snapshot;
use crate::session::SessionRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }
}

/// Task text from a terminal title: drops leading single-glyph tokens such as omp's `π` and
/// braille spinner frames, so `π ⠇ issues — plan` reads `issues — plan`.
pub fn task_text(title: &str) -> Option<&str> {
    let mut rest = title.trim_start();
    while let Some((head, tail)) = rest.split_once(char::is_whitespace) {
        if head.chars().count() != 1 {
            break;
        }
        rest = tail.trim_start();
    }
    let rest = rest.trim_end();
    (!rest.is_empty() && rest.chars().count() > 1).then_some(rest)
}

/// One pane fact from any source (live event, snapshot, history log), folded by `State::apply`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneMeta {
    pub pane_id: String,
    pub workspace_id: String,
    pub workspace_label: Option<String>,
    pub tab_id: Option<String>,
    pub tab_label: Option<String>,
    pub terminal_id: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub terminal_title: Option<String>,
    pub cwd: Option<String>,
    pub status: Option<AgentStatus>,
    /// Agent session resolved for the pane.
    pub session: Option<SessionRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    Created(PaneMeta),
    /// A full pane record (snapshot or `pane.updated`): its title replaces the lane's, even when empty.
    Updated(PaneMeta),
    Detected {
        meta: PaneMeta,
        released: bool,
    },
    /// `meta.status` is always `Some`.
    Status(PaneMeta),
    Moved {
        previous_pane_id: String,
        pane: PaneMeta,
    },
    Closed {
        pane_id: String,
    },
    Exited {
        pane_id: String,
    },
}

impl HostEvent {
    fn meta(&self) -> Option<&PaneMeta> {
        match self {
            HostEvent::Created(m) | HostEvent::Updated(m) | HostEvent::Status(m) => Some(m),
            HostEvent::Detected { meta, .. } => Some(meta),
            HostEvent::Moved { pane, .. } => Some(pane),
            HostEvent::Closed { .. } | HostEvent::Exited { .. } => None,
        }
    }

    fn pane_id(&self) -> &str {
        match self {
            HostEvent::Closed { pane_id } | HostEvent::Exited { pane_id } => pane_id,
            _ => self.meta().map_or("", |m| m.pane_id.as_str()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// First met already running (snapshot, or a status event with no prior lane).
    Seen,
    Created,
    Status(AgentStatus),
    Closed,
    Exited,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub at_ms: i64,
    pub mark: Mark,
    /// Status in effect after this step.
    pub status: AgentStatus,
}

/// One pane lifetime. A pane id reused after close starts a new lane.
#[derive(Debug, Clone)]
pub struct Lane {
    /// Unique for this run; selection and expansion follow it.
    pub id: u64,
    /// Agent palette slot, assigned once in first-seen order.
    pub slot: Option<usize>,
    pub pane_id: String,
    pub workspace_id: String,
    /// Latest non-empty label from the log.
    pub workspace_label: Option<String>,
    pub tab_id: Option<String>,
    /// Latest non-empty tab label from the log.
    pub tab_label: Option<String>,
    /// Stable across pane moves; a moved pane rejoins its lane by it.
    pub terminal_id: Option<String>,
    /// Latest non-empty agent name; never cleared (release is ignored).
    pub agent: Option<String>,
    pub title: Option<String>,
    pub terminal_title: Option<String>,
    pub cwd: Option<String>,
    pub status: AgentStatus,
    pub opened_ms: i64,
    pub closed_ms: Option<i64>,
    /// Seen in a snapshot or live event this run.
    pub live: bool,
    /// First is Seen|Created; Status steps only on change; Closed|Exited last.
    pub steps: Vec<Step>,
    /// Agent sessions seen for this pane, oldest first; usage history comes from these.
    pub sessions: Vec<SessionRef>,
}

impl Lane {
    pub fn label(&self) -> &str {
        self.title.as_deref().filter(|t| !t.is_empty()).or(self.task()).or(self.agent.as_deref()).unwrap_or(&self.pane_id)
    }

    /// What the agent is doing, from its terminal title.
    pub fn task(&self) -> Option<&str> {
        self.terminal_title.as_deref().and_then(task_text)
    }

    pub fn visible(&self) -> bool {
        self.agent.is_some()
    }

    pub fn is_open(&self) -> bool {
        self.closed_ms.is_none()
    }

    pub fn last_ms(&self) -> i64 {
        self.steps.last().map_or(self.opened_ms, |s| s.at_ms)
    }
}

/// Status of `lane` at `t_ms`; None outside its lifetime.
pub fn status_at(lane: &Lane, t_ms: i64) -> Option<AgentStatus> {
    if t_ms < lane.opened_ms || lane.closed_ms.is_some_and(|c| t_ms >= c) {
        return None;
    }
    lane.steps.iter().take_while(|s| s.at_ms <= t_ms).last().map(|s| s.status)
}

/// Blocked episodes due a notification: open agent lanes blocked for at least `after_ms` and not in
/// `sent`, keyed by (lane id, step count) so each episode notifies once.
pub fn due_notifications(state: &State, now_ms: i64, after_ms: i64, sent: &HashSet<(u64, usize)>) -> Vec<(u64, usize)> {
    state
        .lanes
        .iter()
        .filter(|l| l.visible() && l.is_open() && l.status == AgentStatus::Blocked && now_ms - l.last_ms() >= after_ms)
        .map(|l| (l.id, l.steps.len()))
        .filter(|k| !sent.contains(k))
        .collect()
}

/// Time a lane spent in each status within a window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusTime {
    pub working_ms: i64,
    pub blocked_ms: i64,
    /// Time the lane was open at all within the window.
    pub alive_ms: i64,
}

/// How `lane` spent `[from_ms, to_ms)`: each step's status holds until the next step, the lane's
/// close, or `to_ms`.
pub fn status_time(lane: &Lane, from_ms: i64, to_ms: i64) -> StatusTime {
    let end = lane.closed_ms.unwrap_or(i64::MAX).min(to_ms);
    let mut out = StatusTime::default();
    for (i, step) in lane.steps.iter().enumerate() {
        let next = lane.steps.get(i + 1).map_or(end, |s| s.at_ms.min(end));
        let span = (next.min(end) - step.at_ms.max(from_ms)).max(0);
        if span == 0 {
            continue;
        }
        out.alive_ms += span;
        match step.status {
            AgentStatus::Working => out.working_ms += span,
            AgentStatus::Blocked => out.blocked_ms += span,
            _ => {}
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct State {
    pub lanes: Vec<Lane>,
    pub workspaces: BTreeMap<String, String>,
    /// Tab id → label, from the last snapshot.
    pub tabs: BTreeMap<String, String>,
    pub focused_workspace: Option<String>,
    pub focused_tab: Option<String>,
    pub self_pane: Option<String>,
    next_id: u64,
    next_slot: usize,
}

/// Which lanes the views show, how they sort, and how they group.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub show_closed: bool,
    /// Hide agents idle, done or unknown for longer than the quiet window.
    pub active_only: bool,
    pub scope: Scope,
    /// Case-insensitive; matches label, task, workspace and cwd.
    pub search: String,
    pub sort: Sort,
    pub group: Group,
    /// Keys of folded groups.
    pub collapsed: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    All,
    /// The focused workspace.
    Workspace,
    /// The focused tab.
    Tab,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// Workspace, then open time.
    #[default]
    Stable,
    /// Blocked first (longest waiting first), then working, done, idle, unknown, closed.
    Attention,
    /// Latest change first.
    Recent,
    Name,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Group {
    #[default]
    Workspace,
    Tab,
    None,
}

/// One row of the lanes list.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A group's header; `counts` are its open agents by status, `[blocked, working, done, idle, unknown]`.
    Group {
        key: String,
        label: String,
        counts: [usize; 5],
        collapsed: bool,
    },
    Lane(usize),
    /// Agents the query hides: closed ones, and quiet ones under `active_only`.
    Summary {
        closed: usize,
        quiet: usize,
    },
}

/// What the selection follows: a lane or a group header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    Lane(u64),
    Group(String),
}

impl Row {
    pub fn key(&self, state: &State) -> Option<RowKey> {
        match self {
            Row::Group { key, .. } => Some(RowKey::Group(key.clone())),
            Row::Lane(i) => Some(RowKey::Lane(state.lanes[*i].id)),
            Row::Summary { .. } => None,
        }
    }
}

/// Index of `s` in `Row::Group` counts.
pub fn count_slot(s: AgentStatus) -> usize {
    match s {
        AgentStatus::Blocked => 0,
        AgentStatus::Working => 1,
        AgentStatus::Done => 2,
        AgentStatus::Idle => 3,
        AgentStatus::Unknown => 4,
    }
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.clone().filter(|s| !s.is_empty())
}

impl State {
    pub fn new(self_pane: Option<String>) -> Self {
        State { self_pane, ..Default::default() }
    }

    fn is_self(&self, pane_id: &str) -> bool {
        self.self_pane.as_deref() == Some(pane_id)
    }

    /// Index of the open lane for `pane_id`.
    fn open_lane(&self, pane_id: &str) -> Option<usize> {
        self.lanes.iter().rposition(|l| l.pane_id == pane_id && l.closed_ms.is_none())
    }

    pub fn lane_by_id(&self, id: u64) -> Option<usize> {
        self.lanes.iter().position(|l| l.id == id)
    }

    pub fn open(&mut self, pane_id: &str, workspace_id: &str, at_ms: i64, mark: Mark, status: AgentStatus, live: bool) -> usize {
        self.next_id += 1;
        self.lanes.push(Lane {
            id: self.next_id,
            slot: None,
            pane_id: pane_id.to_string(),
            workspace_id: workspace_id.to_string(),
            workspace_label: None,
            tab_id: None,
            tab_label: None,
            terminal_id: None,
            agent: None,
            title: None,
            terminal_title: None,
            cwd: None,
            status,
            opened_ms: at_ms,
            closed_ms: None,
            live,
            steps: vec![Step { at_ms, mark, status }],
            sessions: Vec::new(),
        });
        self.lanes.len() - 1
    }

    /// Open lane for `pane_id`, opening a `Seen` one with `status` if there is none.
    fn open_or_seen(&mut self, pane_id: &str, workspace_id: &str, at_ms: i64, status: AgentStatus, live: bool) -> usize {
        match self.open_lane(pane_id) {
            Some(i) => i,
            None => self.open(pane_id, workspace_id, at_ms, Mark::Seen, status, live),
        }
    }

    /// Moves a first-seen lane's start back to `t_ms`, when its agent's session shows it was
    /// already running: the time before the panel saw it reads as unknown, not as its first status.
    pub fn backdate(&mut self, idx: usize, t_ms: i64) {
        let lane = &mut self.lanes[idx];
        if lane.steps.first().is_none_or(|s| s.mark != Mark::Seen) || t_ms >= lane.opened_ms {
            return;
        }
        let first = &mut lane.steps[0];
        first.mark = Mark::Status(first.status);
        lane.steps.insert(0, Step { at_ms: t_ms, mark: Mark::Seen, status: AgentStatus::Unknown });
        lane.opened_ms = t_ms;
    }

    pub fn set_status(&mut self, idx: usize, status: AgentStatus, at_ms: i64) {
        let lane = &mut self.lanes[idx];
        if lane.status == status {
            return;
        }
        lane.status = status;
        lane.steps.push(Step { at_ms, mark: Mark::Status(status), status });
    }

    pub fn close(&mut self, pane_id: &str, mark: Mark, at_ms: i64) {
        let Some(i) = self.open_lane(pane_id) else { return };
        let lane = &mut self.lanes[i];
        lane.closed_ms = Some(at_ms);
        lane.steps.push(Step { at_ms, mark, status: lane.status });
    }

    /// A pane that moved comes back under a new pane id: when no open lane has `m.pane_id` but one
    /// has its terminal id, that lane takes the new pane id. True when it did.
    fn rejoin(&mut self, m: &PaneMeta) -> bool {
        if self.open_lane(&m.pane_id).is_some() {
            return false;
        }
        let Some(t) = m.terminal_id.as_deref() else { return false };
        let Some(i) = self.lanes.iter().position(|l| l.is_open() && l.terminal_id.as_deref() == Some(t)) else { return false };
        self.lanes[i].pane_id = m.pane_id.clone();
        true
    }

    /// Takes `m`'s fields onto lane `i`. A full record (`full`) replaces agent, titles and cwd
    /// whenever it carries them, so a title the host cleared doesn't linger; partial facts only
    /// fill in non-empty values.
    fn merge(&mut self, i: usize, m: &PaneMeta, full: bool, live: bool) {
        let lane = &mut self.lanes[i];
        if live {
            lane.live = true;
        }
        if !m.workspace_id.is_empty() {
            lane.workspace_id = m.workspace_id.clone();
        }
        for (slot, value) in [
            (&mut lane.workspace_label, &m.workspace_label),
            (&mut lane.tab_id, &m.tab_id),
            (&mut lane.tab_label, &m.tab_label),
            (&mut lane.terminal_id, &m.terminal_id),
        ] {
            if let Some(v) = nonempty(value) {
                *slot = Some(v);
            }
        }
        for (slot, value) in [(&mut lane.agent, &m.agent), (&mut lane.terminal_title, &m.terminal_title), (&mut lane.cwd, &m.cwd)]
        {
            if full && value.is_some() {
                slot.clone_from(value);
            } else if let Some(v) = nonempty(value) {
                *slot = Some(v);
            }
        }
        if full {
            lane.title.clone_from(&m.title);
        } else if let Some(t) = nonempty(&m.title) {
            lane.title = Some(t);
        }
        if let Some(s) = &m.session
            && !lane.sessions.contains(s)
        {
            lane.sessions.push(s.clone());
        }
    }

    fn update(&mut self, m: &PaneMeta, at_ms: i64, live: bool) {
        // A live block whose agent exited ends that agent's lane, as the recorder logs it; the
        // shell left behind continues on a hidden lane.
        if live
            && m.agent.is_none()
            && let Some(i) = self.open_lane(&m.pane_id)
            && self.lanes[i].agent.is_some()
        {
            self.close(&m.pane_id, Mark::Closed, at_ms);
        }
        let i = self.open_or_seen(&m.pane_id, &m.workspace_id, at_ms, m.status.unwrap_or_default(), live);
        self.merge(i, m, true, live);
        if let Some(s) = m.status {
            self.set_status(i, s, at_ms);
        }
    }

    /// Folds one pane fact at `at_ms`; `live` marks facts from this run (snapshot or subscription).
    pub fn apply(&mut self, e: &HostEvent, at_ms: i64, live: bool) {
        if let HostEvent::Moved { previous_pane_id, pane } = e
            && self.is_self(previous_pane_id)
        {
            // The pinned band followed focus to another workspace: keep skipping it.
            self.self_pane = Some(pane.pane_id.clone());
            return;
        }
        if self.is_self(e.pane_id()) {
            return;
        }
        let rejoined = e.meta().is_some_and(|m| self.rejoin(m));
        match e {
            // A move the host reports as a create must not split the history.
            HostEvent::Created(m) if rejoined => self.update(m, at_ms, live),
            HostEvent::Created(m) => {
                self.close(&m.pane_id, Mark::Closed, at_ms);
                let i = self.open(&m.pane_id, &m.workspace_id, at_ms, Mark::Created, m.status.unwrap_or_default(), live);
                self.merge(i, m, false, live);
            }
            HostEvent::Updated(m) => self.update(m, at_ms, live),
            HostEvent::Detected { meta, released } => {
                let i = self.open_or_seen(&meta.pane_id, &meta.workspace_id, at_ms, meta.status.unwrap_or_default(), live);
                let agent = if *released { None } else { meta.agent.clone() };
                self.merge(i, &PaneMeta { agent, ..meta.clone() }, false, live);
                if let Some(s) = meta.status {
                    self.set_status(i, s, at_ms);
                }
            }
            HostEvent::Status(m) => {
                let status = m.status.unwrap_or_default();
                let i = self.open_or_seen(&m.pane_id, &m.workspace_id, at_ms, status, live);
                self.merge(i, m, false, live);
                self.set_status(i, status, at_ms);
            }
            HostEvent::Moved { previous_pane_id, pane } => {
                if let Some(i) = self.open_lane(previous_pane_id) {
                    self.lanes[i].pane_id = pane.pane_id.clone();
                }
                self.update(pane, at_ms, live);
            }
            HostEvent::Closed { pane_id } => self.close(pane_id, Mark::Closed, at_ms),
            HostEvent::Exited { pane_id } => self.close(pane_id, Mark::Exited, at_ms),
        }
        self.assign_slots();
    }

    pub fn apply_snapshot(&mut self, snap: &Snapshot, now_ms: i64) {
        self.workspaces = snap
            .workspaces
            .iter()
            .map(|w| (w.workspace_id.clone(), w.label.clone().unwrap_or_else(|| w.workspace_id.clone())))
            .collect();
        self.tabs = snap.tabs.iter().map(|t| (t.tab_id.clone(), t.label.clone().unwrap_or_else(|| t.tab_id.clone()))).collect();
        self.focused_workspace.clone_from(&snap.focused_workspace_id);
        self.focused_tab.clone_from(&snap.focused_tab_id);
        for p in &snap.panes {
            self.apply(&HostEvent::Updated(p.into()), now_ms, true);
        }
        let present: BTreeSet<&str> = snap.panes.iter().map(|p| p.pane_id.as_str()).collect();
        let gone: Vec<(String, i64)> = self
            .lanes
            .iter()
            .filter(|l| l.is_open() && !present.contains(l.pane_id.as_str()))
            // A log-only lane's pane died while nothing was watching: end it at its last step.
            .map(|l| (l.pane_id.clone(), if l.live { now_ms } else { l.last_ms() }))
            .collect();
        for (id, at) in gone {
            self.close(&id, Mark::Closed, at);
        }
        self.lanes.retain(|l| l.closed_ms.is_none_or(|c| c >= now_ms - history_ms()));
        self.assign_slots();
    }

    /// Gives each visible lane without a palette slot the next one, in first-seen (vector) order,
    /// so an agent keeps its color however many others come and go.
    pub fn assign_slots(&mut self) {
        for lane in &mut self.lanes {
            if lane.visible() && lane.slot.is_none() {
                lane.slot = Some(self.next_slot);
                self.next_slot += 1;
            }
        }
    }

    fn workspace_of<'a>(&'a self, lane: &'a Lane) -> &'a str {
        self.workspaces
            .get(&lane.workspace_id)
            .map(String::as_str)
            .or(lane.workspace_label.as_deref())
            .unwrap_or(&lane.workspace_id)
    }

    /// Workspace label for display: snapshot label, else the logged one, else the id.
    pub fn workspace_label(&self, idx: usize) -> &str {
        self.workspace_of(&self.lanes[idx])
    }

    /// Tab label for display: snapshot label, else the logged one.
    pub fn tab_label(&self, idx: usize) -> Option<&str> {
        let lane = &self.lanes[idx];
        lane.tab_id.as_ref().and_then(|t| self.tabs.get(t)).map(String::as_str).or(lane.tab_label.as_deref())
    }

    /// Group key and label of lane `i` under `group`.
    pub fn group_of(&self, i: usize, group: Group) -> (String, String) {
        let lane = &self.lanes[i];
        match group {
            Group::Workspace => (lane.workspace_id.clone(), self.workspace_label(i).to_string()),
            Group::Tab => match &lane.tab_id {
                Some(t) => (t.clone(), format!("{} · {}", self.workspace_label(i), self.tab_label(i).unwrap_or(t))),
                None => (String::new(), "no tab".into()),
            },
            Group::None => (String::new(), String::new()),
        }
    }

    /// Rows for `q`: agent lanes filtered, grouped and sorted, group headers (all of them when
    /// `headers`, else only folded ones), and a summary of what the filters hide. An open blocked
    /// agent passes every filter and shows even inside a folded group.
    pub fn rows(&self, q: &Query, now_ms: i64, quiet_after_ms: i64, headers: bool) -> Vec<Row> {
        let search = q.search.to_lowercase();
        let (mut closed, mut quiet) = (0, 0);
        let mut passing = Vec::new();
        for (i, lane) in self.lanes.iter().enumerate().filter(|(_, l)| l.visible()) {
            if lane.is_open() && lane.status == AgentStatus::Blocked {
                passing.push(i);
                continue;
            }
            let in_scope = match q.scope {
                Scope::All => true,
                Scope::Workspace => self.focused_workspace.as_ref().is_none_or(|w| *w == lane.workspace_id),
                Scope::Tab => self.focused_tab.as_ref().is_none_or(|t| Some(t) == lane.tab_id.as_ref()),
            };
            let found = search.is_empty()
                || [Some(lane.label()), lane.task(), Some(self.workspace_label(i)), lane.cwd.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|s| s.to_lowercase().contains(&search));
            if !in_scope || !found {
                continue;
            }
            if !lane.is_open() && !q.show_closed {
                closed += 1;
            } else if q.active_only
                && lane.is_open()
                && matches!(lane.status, AgentStatus::Idle | AgentStatus::Done | AgentStatus::Unknown)
                && now_ms - lane.last_ms() >= quiet_after_ms
            {
                quiet += 1;
            } else {
                passing.push(i);
            }
        }

        let rank = |i: usize| {
            let l = &self.lanes[i];
            if l.is_open() { count_slot(l.status) } else { 5 }
        };
        passing.sort_by(|&a, &b| {
            let (la, lb) = (&self.lanes[a], &self.lanes[b]);
            let stable =
                (self.workspace_of(la), la.opened_ms, &la.pane_id).cmp(&(self.workspace_of(lb), lb.opened_ms, &lb.pane_id));
            match q.sort {
                Sort::Stable => stable,
                Sort::Attention => (rank(a), if rank(a) == 0 { la.last_ms() } else { 0 })
                    .cmp(&(rank(b), if rank(b) == 0 { lb.last_ms() } else { 0 }))
                    .then(stable),
                Sort::Recent => lb.last_ms().cmp(&la.last_ms()).then(stable),
                Sort::Name => la.label().to_lowercase().cmp(&lb.label().to_lowercase()).then(stable),
            }
        });

        // Groups in label order (blocked ones first under Attention), lanes in sort order within.
        let mut groups: Vec<(String, String, Vec<usize>)> = Vec::new();
        for i in passing {
            let (key, label) = self.group_of(i, q.group);
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.2.push(i),
                None => groups.push((key, label, vec![i])),
            }
        }
        let blocked = |g: &(String, String, Vec<usize>)| g.2.iter().any(|&i| rank(i) == 0);
        groups.sort_by(|a, b| {
            let first = if q.sort == Sort::Attention { blocked(b).cmp(&blocked(a)) } else { std::cmp::Ordering::Equal };
            first.then_with(|| a.1.cmp(&b.1))
        });

        let mut rows = Vec::new();
        for (key, label, lanes) in groups {
            let grouped = q.group != Group::None;
            let collapsed = grouped && q.collapsed.contains(&key);
            if grouped && (headers || collapsed) {
                let mut counts = [0; 5];
                for &i in lanes.iter().filter(|&&i| self.lanes[i].is_open()) {
                    counts[count_slot(self.lanes[i].status)] += 1;
                }
                rows.push(Row::Group { key, label, counts, collapsed });
            }
            rows.extend(lanes.into_iter().filter(|&i| !collapsed || rank(i) == 0).map(Row::Lane));
        }
        if closed > 0 || quiet > 0 {
            rows.push(Row::Summary { closed, quiet });
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::LogEvent;
    use crate::rex::{Raw, map_raw};
    use crate::session::RefKind;
    use serde_json::json;

    fn log(ts_ms: i64, event: &str, pane_id: &str, status: Option<AgentStatus>) -> LogEvent {
        LogEvent {
            ts_ms,
            session: "default".into(),
            event: event.into(),
            pane_id: pane_id.into(),
            workspace_id: "w1".into(),
            workspace_label: None,
            tab_label: None,
            cwd: None,
            agent: Some("omp".into()),
            status,
            tab_id: None,
            terminal_id: None,
            agent_session: None,
            previous_pane_id: None,
        }
    }

    fn fold(s: &mut State, e: &LogEvent) {
        if let Some(h) = e.host_event() {
            s.apply(&h, e.ts_ms, false);
        }
    }

    fn marks(lane: &Lane) -> Vec<Mark> {
        lane.steps.iter().map(|s| s.mark).collect()
    }

    #[test]
    fn log_fold_builds_lanes_per_lifetime() {
        use AgentStatus::*;
        let mut s = State::default();
        for e in [
            log(0, "pane.created", "p1", Some(Idle)),
            log(10, "pane.agent_status_changed", "p1", Some(Working)),
            log(11, "pane.agent_status_changed", "p1", Some(Working)),
            log(20, "pane.agent_status_changed", "p1", Some(Blocked)),
            log(30, "pane.closed", "p1", None),
            log(40, "pane.created", "p1", None),
        ] {
            fold(&mut s, &e);
        }
        assert_eq!(s.lanes.len(), 2);
        assert_eq!(marks(&s.lanes[0]), [Mark::Created, Mark::Status(Working), Mark::Status(Blocked), Mark::Closed]);
        assert_eq!(s.lanes[0].closed_ms, Some(30));
        assert!(s.lanes[1].is_open());
        assert_eq!(s.lanes[1].steps[0], Step { at_ms: 40, mark: Mark::Created, status: Unknown });
        assert_eq!(status_at(&s.lanes[0], 15), Some(Working));
        assert_eq!(status_at(&s.lanes[0], 30), None);
    }

    #[test]
    fn snapshot_merges_with_log_lanes() {
        use AgentStatus::*;
        let mut s = State::default();
        fold(&mut s, &log(0, "pane.created", "p1", Some(Idle)));
        fold(&mut s, &log(50, "pane.agent_status_changed", "p1", Some(Working)));
        fold(&mut s, &log(5, "pane.created", "p3", Some(Idle)));
        let snap: Snapshot = serde_json::from_value(json!({
            "workspaces": [{"workspace_id": "w1", "label": "repo"}],
            "tabs": [{"tab_id": "w1:t1", "workspace_id": "w1", "label": "main"}],
            "panes": [
                {"pane_id": "p2", "workspace_id": "w1", "agent": "omp", "agent_status": "idle"},
                {"pane_id": "p3", "workspace_id": "w1", "tab_id": "w1:t1", "agent": "omp", "agent_status": "working",
                 "terminal_title": "π ⠇ fix login",
                 "agent_session": {"kind": "path", "value": "/s/a.jsonl"}},
            ],
        }))
        .unwrap();
        s.apply_snapshot(&snap, 1000);

        let p1 = &s.lanes[s.lanes.iter().position(|l| l.pane_id == "p1").unwrap()];
        assert_eq!(p1.closed_ms, Some(50), "log-only lane ends at its last step");
        assert_eq!(p1.steps.last().unwrap().mark, Mark::Closed);

        let p2 = s.lanes.iter().find(|l| l.pane_id == "p2").unwrap();
        assert_eq!(p2.steps, [Step { at_ms: 1000, mark: Mark::Seen, status: Idle }]);
        assert!(p2.live);

        let i3 = s.lanes.iter().position(|l| l.pane_id == "p3").unwrap();
        let p3 = &s.lanes[i3];
        assert!(p3.live && p3.is_open());
        assert_eq!(p3.steps.last().unwrap(), &Step { at_ms: 1000, mark: Mark::Status(Working), status: Working });
        assert_eq!(p3.label(), "fix login");
        let r = SessionRef { agent: "omp".into(), kind: RefKind::Path, value: "/s/a.jsonl".into() };
        assert_eq!(p3.sessions, [r]);
        assert_eq!(s.tab_label(i3), Some("main"));
        assert_eq!(s.lanes.iter().filter(|l| l.is_open() && l.live).count(), 2, "p2 and p3");
    }

    #[test]
    fn fixture_snapshot_folds_into_pane_lanes() {
        let raw: Raw = serde_json::from_str(include_str!("../tests/fixtures/snapshot.json")).unwrap();
        let snap = map_raw(&raw, |_, _, _| None);
        assert_eq!(snap.workspaces.len(), 2);
        let mut s = State::default();
        s.apply_snapshot(&snap, 1000);
        let visible: Vec<&Lane> = s.lanes.iter().filter(|l| l.visible()).collect();
        assert_eq!(visible.len(), 1);
        assert_eq!((visible[0].agent.as_deref(), visible[0].status), (Some("omp"), AgentStatus::Working));
        assert_eq!(visible[0].cwd.as_deref(), Some("/Users/u/projects/rex-extras"));
        let shell = s.lanes.iter().find(|l| l.workspace_id != visible[0].workspace_id).expect("the shell block's lane");
        assert_eq!(shell.agent, None);
        assert_eq!(s.focused_tab.as_deref(), Some(visible[0].tab_id.as_deref().unwrap()));
    }

    #[test]
    fn snapshot_closes_live_lane_now_and_prunes_old() {
        let mut s = State::default();
        let i = s.open("p1", "w1", 0, Mark::Seen, AgentStatus::Idle, true);
        s.lanes[i].agent = Some("omp".into());
        s.apply_snapshot(&Snapshot::default(), 500);
        assert_eq!(s.lanes[0].closed_ms, Some(500));
        s.apply_snapshot(&Snapshot::default(), 500 + history_ms() + 1);
        assert!(s.lanes.is_empty());
    }

    #[test]
    fn moved_renames_open_lane() {
        let mut s = State::default();
        s.open("w1:p1", "w1", 0, Mark::Seen, AgentStatus::Idle, true);
        let pane = PaneMeta {
            pane_id: "w2:p4".into(),
            workspace_id: "w2".into(),
            agent: Some("omp".into()),
            status: Some(AgentStatus::Idle),
            ..PaneMeta::default()
        };
        s.apply(&HostEvent::Moved { previous_pane_id: "w1:p1".into(), pane }, 5, true);
        assert_eq!(s.lanes.len(), 1);
        assert_eq!((s.lanes[0].pane_id.as_str(), s.lanes[0].workspace_id.as_str()), ("w2:p4", "w2"));
        assert_eq!(s.lanes[0].agent.as_deref(), Some("omp"));
    }

    #[test]
    fn moved_self_stays_self() {
        let mut s = State::new(Some("w1:p2".into()));
        let pane = PaneMeta { pane_id: "w3:p5".into(), workspace_id: "w3".into(), ..PaneMeta::default() };
        s.apply(&HostEvent::Moved { previous_pane_id: "w1:p2".into(), pane: pane.clone() }, 5, true);
        assert!(s.lanes.is_empty());
        s.apply(&HostEvent::Updated(pane), 6, true);
        assert!(s.lanes.is_empty(), "the band's new pane id is still itself");
    }

    #[test]
    fn agent_leaving_a_live_block_closes_its_lane() {
        let omp = PaneMeta {
            pane_id: "b1".into(),
            workspace_id: "s1".into(),
            agent: Some("omp".into()),
            status: Some(AgentStatus::Working),
            ..PaneMeta::default()
        };
        let shell = PaneMeta { agent: None, status: Some(AgentStatus::Unknown), ..omp.clone() };
        let mut s = State::default();
        s.apply(&HostEvent::Updated(omp), 0, true);
        s.apply(&HostEvent::Updated(shell.clone()), 10, true);
        assert_eq!(s.lanes.len(), 2);
        assert_eq!((s.lanes[0].closed_ms, s.lanes[0].steps.last().unwrap().mark), (Some(10), Mark::Closed));
        assert!(s.lanes[1].is_open() && !s.lanes[1].visible(), "the shell left behind gets a hidden lane");
        s.apply(&HostEvent::Updated(shell), 20, true);
        assert_eq!(s.lanes.len(), 2, "a plain shell stays one hidden lane");
    }

    #[test]
    fn moved_pane_rejoins_by_terminal_id() {
        let mut s = State::default();
        let first = PaneMeta {
            pane_id: "w1:p1".into(),
            workspace_id: "w1".into(),
            terminal_id: Some("t1".into()),
            agent: Some("omp".into()),
            ..PaneMeta::default()
        };
        s.apply(&HostEvent::Updated(PaneMeta { status: Some(AgentStatus::Idle), ..first.clone() }), 0, true);
        let moved = PaneMeta { pane_id: "w2:p4".into(), workspace_id: "w2".into(), status: Some(AgentStatus::Working), ..first };
        s.apply(&HostEvent::Status(moved), 10, true);
        assert_eq!(s.lanes.len(), 1);
        assert_eq!((s.lanes[0].pane_id.as_str(), s.lanes[0].workspace_id.as_str()), ("w2:p4", "w2"));
        assert_eq!(s.lanes[0].status, AgentStatus::Working);
    }

    #[test]
    fn agent_slots_are_assigned_once_in_first_seen_order() {
        let mut s = State::default();
        for (t, p) in [(0, "p1"), (1, "p2"), (2, "p3")] {
            fold(&mut s, &log(t, "pane.created", p, Some(AgentStatus::Idle)));
        }
        fold(&mut s, &log(10, "pane.closed", "p1", None));
        let pane = |id: &str| json!({"pane_id": id, "workspace_id": "w1", "agent": "omp", "agent_status": "idle"});
        let snap: Snapshot = serde_json::from_value(json!({"panes": [pane("p2"), pane("p3")]})).unwrap();
        s.apply_snapshot(&snap, 10 + history_ms() + 1);
        assert_eq!(s.lanes.len(), 2, "p1 pruned");
        fold(&mut s, &log(20 + history_ms(), "pane.created", "p4", Some(AgentStatus::Idle)));
        let slots: Vec<_> = s.lanes.iter().map(|l| (l.pane_id.as_str(), l.slot)).collect();
        assert_eq!(slots, [("p2", Some(1)), ("p3", Some(2)), ("p4", Some(3))]);
    }

    #[test]
    fn status_time_splits_a_window_by_status() {
        let mut s = State::default();
        let i = s.open("p1", "w1", 0, Mark::Created, AgentStatus::Idle, true);
        s.set_status(i, AgentStatus::Working, 100);
        s.set_status(i, AgentStatus::Blocked, 300);
        s.close("p1", Mark::Closed, 400);
        let l = &s.lanes[0];
        assert_eq!(status_time(l, 0, 1000), StatusTime { working_ms: 200, blocked_ms: 100, alive_ms: 400 });
        assert_eq!(status_time(l, 150, 350), StatusTime { working_ms: 150, blocked_ms: 50, alive_ms: 200 });
        assert_eq!(status_time(l, 500, 600), StatusTime::default(), "after close");
    }

    #[test]
    fn blocked_notification_fires_once_per_episode() {
        let mut s = four_lanes();
        let mut sent = HashSet::new();
        assert!(due_notifications(&s, 1000, 5000, &sent).is_empty(), "blocked for 899 ms only");
        let due = due_notifications(&s, 6000, 5000, &sent);
        assert_eq!(due.len(), 1);
        sent.extend(due);
        assert!(due_notifications(&s, 9000, 5000, &sent).is_empty(), "once per episode");
        s.set_status(1, AgentStatus::Working, 9100);
        s.set_status(1, AgentStatus::Blocked, 9200);
        assert_eq!(due_notifications(&s, 20_000, 5000, &sent).len(), 1, "a new episode notifies again");
    }

    /// Lanes over workspaces `a` (idle p1, blocked p2) and `b` (working p3, closed p4).
    fn four_lanes() -> State {
        let workspaces = [("a".to_string(), "alpha".to_string()), ("b".to_string(), "beta".to_string())].into();
        let mut s = State { workspaces, ..State::default() };
        for (k, (ws, status)) in
            [("a", AgentStatus::Idle), ("a", AgentStatus::Blocked), ("b", AgentStatus::Working), ("b", AgentStatus::Done)]
                .into_iter()
                .enumerate()
        {
            let i = s.open(&format!("{ws}:p{}", k + 1), ws, k as i64 * 10, Mark::Seen, AgentStatus::Unknown, true);
            s.lanes[i].agent = Some("omp".into());
            s.lanes[i].title = Some(format!("task{}", k + 1));
            s.set_status(i, status, 100 + k as i64);
        }
        s.close("b:p4", Mark::Closed, 200);
        s
    }

    #[test]
    fn rows_filter_sort_and_group() {
        let s = four_lanes();
        let q = Query::default();
        let rows = s.rows(&q, 1000, 500, true);
        let group = |label: &str, counts: [usize; 5]| Row::Group {
            key: label[..1].into(),
            label: label.into(),
            counts,
            collapsed: false,
        };
        assert_eq!(
            rows,
            [
                group("alpha", [1, 0, 0, 1, 0]),
                Row::Lane(0),
                Row::Lane(1),
                group("beta", [0, 1, 0, 0, 0]),
                Row::Lane(2),
                Row::Summary { closed: 1, quiet: 0 }
            ]
        );

        let q = Query { sort: Sort::Attention, group: Group::None, show_closed: true, ..Query::default() };
        assert_eq!(s.rows(&q, 1000, 500, true), [Row::Lane(1), Row::Lane(2), Row::Lane(0), Row::Lane(3)]);

        let q = Query { active_only: true, ..Query::default() };
        let rows = s.rows(&q, 1000, 500, false);
        assert_eq!(
            rows,
            [Row::Lane(1), Row::Lane(2), Row::Summary { closed: 1, quiet: 1 }],
            "no headers, the idle lane is quiet"
        );
    }

    #[test]
    fn blocked_lanes_pass_every_filter() {
        let s = four_lanes();
        let mut q = Query { active_only: true, search: "nothing matches".into(), ..Query::default() };
        assert_eq!(s.rows(&q, 1000, 0, false), [Row::Lane(1)], "only the blocked lane, and nothing counted as hidden");
        q.search.clear();
        q.collapsed.insert("a".into());
        let rows = s.rows(&q, 1000, 0, true);
        assert!(matches!(&rows[0], Row::Group { collapsed: true, .. }));
        assert_eq!(rows[1], Row::Lane(1), "the blocked lane shows inside its folded group");
        assert!(!rows.contains(&Row::Lane(0)));
    }

    #[test]
    fn task_text_drops_leading_glyph_tokens() {
        assert_eq!(task_text("π ⠇ issues — plan for issue 4115"), Some("issues — plan for issue 4115"));
        assert_eq!(task_text("  ● build  "), Some("build"));
        assert_eq!(task_text("david@host:~/x"), Some("david@host:~/x"));
        assert_eq!(task_text("π ⠦"), None);
        assert_eq!(task_text("π ⠦ x"), None);
        assert_eq!(task_text(""), None);
    }
}
