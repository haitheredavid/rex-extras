//! Every session the panel knows, keyed to lanes: finds the files each pane's session ref names, tails
//! them, discovers subagent sessions next to them, and answers the views' per-lane questions.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::tail::Tail;
use super::{Decoder, EventKind, RefKind, Session, SessionIdx, SessionRef, Source, SourceRef, decoder};
use crate::model::Lane;

/// Store refreshes at most this often.
const REFRESH_MS: i64 = 1000;
/// Subagent discovery walks directories at most this often.
const SCAN_MS: i64 = 2000;
/// A session ref that didn't resolve is tried again after this long.
const RETRY_LOCATE_MS: i64 = 10_000;

/// Where agents keep their session files.
#[derive(Debug, Clone)]
pub struct Roots {
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
    pub codex_index: PathBuf,
}

impl Roots {
    /// `$CLAUDE_CONFIG_DIR` or `~/.claude`, + `/projects`; `$CODEX_HOME` or `~/.codex`, + `/sessions` and `/session_index.jsonl`.
    pub fn from_env() -> Roots {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let dir = |var: &str, default: &str| {
            std::env::var_os(var).filter(|v| !v.is_empty()).map_or_else(|| home.join(default), PathBuf::from)
        };
        let claude = dir("CLAUDE_CONFIG_DIR", ".claude");
        let codex = dir("CODEX_HOME", ".codex");
        Roots {
            claude_projects: claude.join("projects"),
            codex_sessions: codex.join("sessions"),
            codex_index: codex.join("session_index.jsonl"),
        }
    }
}

/// Requests and their dollar cost in a window; `cost` is None when none of them recorded one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spend {
    pub requests: usize,
    pub cost: Option<f64>,
}

pub struct Store {
    pub sessions: Vec<Session>,
    tails: Vec<Tail>,
    decoders: Vec<Box<dyn Decoder>>,
    by_path: HashMap<PathBuf, SessionIdx>,
    /// Lane id → root sessions, oldest first.
    lanes: HashMap<u64, Vec<SessionIdx>>,
    /// Locate result per ref and when it was tried.
    located: HashMap<SessionRef, (Option<PathBuf>, i64)>,
    /// Child waiting for its spawn span, by link key.
    unlinked: Vec<(SessionIdx, String)>,
    /// (parent, key) of every child found, so a rescan doesn't add it twice.
    found: HashSet<(SessionIdx, String)>,
    codex_titles: (Option<SystemTime>, HashMap<String, String>),
    roots: Roots,
    last_scan_ms: Option<i64>,
    last_refresh_ms: Option<i64>,
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "jsonl") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    out.sort();
    out
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// Subdirectories of `dir`, newest name first (Codex's `YYYY`, `MM`, `DD`).
fn dirs_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    out.sort_by(|a, b| b.cmp(a));
    out
}

impl Store {
    pub fn new(roots: Roots) -> Store {
        Store {
            sessions: Vec::new(),
            tails: Vec::new(),
            decoders: Vec::new(),
            by_path: HashMap::new(),
            lanes: HashMap::new(),
            located: HashMap::new(),
            unlinked: Vec::new(),
            found: HashSet::new(),
            codex_titles: (None, HashMap::new()),
            roots,
            last_scan_ms: None,
            last_refresh_ms: None,
        }
    }

    /// Session for `path`, created (unread) when new; the bool says it was.
    fn ensure(&mut self, path: PathBuf, source: Source) -> (SessionIdx, bool) {
        if let Some(&i) = self.by_path.get(&path) {
            return (i, false);
        }
        let i = self.sessions.len();
        self.by_path.insert(path.clone(), i);
        self.sessions.push(Session::new(source, path));
        self.tails.push(Tail::default());
        self.decoders.push(decoder(source));
        (i, true)
    }

    fn locate(&mut self, r: &SessionRef, source: Source, now_ms: i64) -> Option<PathBuf> {
        if let Some((found, at)) = self.located.get(r)
            && (found.is_some() || now_ms - at < RETRY_LOCATE_MS)
        {
            return found.clone();
        }
        let found = match (r.kind, source) {
            (RefKind::Path, _) => Some(PathBuf::from(&r.value)),
            (RefKind::Id, Source::Claude) => std::fs::read_dir(&self.roots.claude_projects)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path().join(format!("{}.jsonl", r.value)))
                .find(|p| p.is_file()),
            (RefKind::Id, Source::Codex) => self.codex_by_id(&r.value),
            (RefKind::Id, Source::Omp) => None,
        };
        self.located.insert(r.clone(), (found.clone(), now_ms));
        found
    }

    /// `rollout-*-<id>.jsonl`, walking `YYYY/MM/DD` newest first.
    fn codex_by_id(&self, id: &str) -> Option<PathBuf> {
        let suffix = format!("-{id}.jsonl");
        for y in dirs_desc(&self.roots.codex_sessions) {
            for m in dirs_desc(&y) {
                for d in dirs_desc(&m) {
                    if let Some(p) = json_files(&d).into_iter().find(|p| {
                        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        name.starts_with("rollout-") && name.ends_with(&suffix)
                    }) {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    /// Reads what was appended to session `i` and applies its facts.
    fn tail(&mut self, i: SessionIdx) {
        let path = self.sessions[i].path.clone();
        let mut facts = Vec::new();
        let (tail, dec) = (&mut self.tails[i], &mut self.decoders[i]);
        // omp rewrites its padded title record on line 1 in place; the tail never sees that again.
        if self.sessions[i].source == Source::Omp
            && let Some(line) = tail.header_changed(&path)
            && line.starts_with(br#"{"type":"title""#)
        {
            dec.decode(&line, SourceRef { session: i, offset: 0, len: line.len() as u32 }, &mut facts);
        }
        tail.read_new(&path, |offset, line| {
            if dec.wants(line) {
                dec.decode(line, SourceRef { session: i, offset, len: line.len() as u32 }, &mut facts);
            }
        });
        for f in facts {
            self.sessions[i].apply(f);
        }
    }

    fn add_child(&mut self, parent: SessionIdx, path: PathBuf, source: Source, key: String, label: Option<String>) {
        if !self.found.insert((parent, key.clone())) {
            return;
        }
        let (c, new) = self.ensure(path, source);
        if !new {
            return;
        }
        self.sessions[c].parent = Some(parent);
        if self.sessions[c].label.is_none() {
            self.sessions[c].label = label;
        }
        self.sessions[parent].children.push(c);
        self.unlinked.push((c, key));
    }

    /// Subagent sessions spawned by session `i`, by where each agent keeps them.
    fn discover(&mut self, i: SessionIdx, now_ms: i64) {
        let s = &self.sessions[i];
        let dir = s.path.with_extension("");
        match s.source {
            Source::Omp => {
                for p in json_files(&dir) {
                    let key = stem(&p);
                    self.add_child(i, p, Source::Omp, key.clone(), Some(key));
                }
            }
            Source::Claude => {
                for p in json_files(&dir.join("subagents")).into_iter().filter(|p| stem(p).starts_with("agent-")) {
                    let meta: serde_json::Value = std::fs::read(p.with_extension("meta.json"))
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or_default();
                    let Some(key) = meta.get("toolUseId").and_then(|v| v.as_str()) else { continue };
                    let label = meta.get("agentType").and_then(|v| v.as_str()).map(str::to_string);
                    self.add_child(i, p.clone(), Source::Claude, key.to_string(), label);
                }
            }
            Source::Codex => {
                let keys: Vec<String> =
                    s.child_keys.keys().filter(|k| !self.found.contains(&(i, (*k).clone()))).cloned().collect();
                for key in keys {
                    let r = SessionRef { agent: "codex".into(), kind: RefKind::Id, value: key.clone() };
                    if let Some(p) = self.locate(&r, Source::Codex, now_ms) {
                        self.add_child(i, p, Source::Codex, key, None);
                    }
                }
            }
        }
    }

    /// Spawn spans found for children waiting on them.
    fn link(&mut self) {
        let sessions = &mut self.sessions;
        self.unlinked.retain(|(c, key)| {
            let Some(p) = sessions[*c].parent else { return false };
            let Some(span_id) = sessions[p].child_keys.get(key) else { return true };
            let Some(si) = sessions[p].spans.iter().position(|s| &s.id == span_id) else { return true };
            sessions[p].spans[si].child = Some(*c);
            sessions[*c].spawn_span = Some(si as u32);
            false
        });
    }

    /// Codex sessions take their thread name from `session_index.jsonl`, reread when it changes.
    fn codex_labels(&mut self) {
        if !self.sessions.iter().any(|s| s.source == Source::Codex && s.label.is_none()) {
            return;
        }
        let mtime = std::fs::metadata(&self.roots.codex_index).and_then(|m| m.modified()).ok();
        if mtime != self.codex_titles.0 {
            let text = std::fs::read_to_string(&self.roots.codex_index).unwrap_or_default();
            let titles = text
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter_map(|v| Some((v.get("id")?.as_str()?.to_string(), v.get("thread_name")?.as_str()?.to_string())))
                .collect();
            self.codex_titles = (mtime, titles);
        }
        for s in self.sessions.iter_mut().filter(|s| s.source == Source::Codex && s.label.is_none()) {
            s.label = s.id.as_ref().and_then(|id| self.codex_titles.1.get(id)).cloned();
        }
    }

    /// Finds, reads and links sessions for `lanes`; at most once a second, except the first call.
    pub fn refresh(&mut self, lanes: &[Lane], now_ms: i64) {
        if self.last_refresh_ms.is_some_and(|t| now_ms - t < REFRESH_MS) {
            return;
        }
        self.last_refresh_ms = Some(now_ms);
        for lane in lanes {
            for r in &lane.sessions {
                let Some(source) = Source::of_agent(&r.agent).or_else(|| lane.agent.as_deref().and_then(Source::of_agent)) else {
                    continue;
                };
                let Some(path) = self.locate(r, source, now_ms) else { continue };
                let (i, _) = self.ensure(path, source);
                let roots = self.lanes.entry(lane.id).or_default();
                if !roots.contains(&i) {
                    roots.push(i);
                }
            }
        }
        let scan = self.last_scan_ms.is_none_or(|t| now_ms - t >= SCAN_MS);
        if scan {
            self.last_scan_ms = Some(now_ms);
        }
        // Children (and theirs) found in a pass are read and scanned in the next, so one refresh
        // takes in a whole tree.
        let mut from = 0;
        let mut pass_scans = scan;
        loop {
            let upto = self.sessions.len();
            for i in from..upto {
                self.tail(i);
            }
            if pass_scans {
                for i in from..upto {
                    self.discover(i, now_ms);
                }
            }
            if self.sessions.len() == upto {
                break;
            }
            from = upto;
            pass_scans = true;
        }
        self.link();
        self.codex_labels();
    }

    pub fn roots(&self, lane: &Lane) -> &[SessionIdx] {
        self.lanes.get(&lane.id).map_or(&[], Vec::as_slice)
    }

    fn root_ctx<'a>(&'a self, lane: &Lane) -> impl Iterator<Item = &'a [(i64, u64)]> + use<'a> {
        self.roots(lane).iter().map(|&i| self.sessions[i].ctx.as_slice())
    }

    /// Context in use at `t_ms`: the latest main request at or before it across the lane's roots.
    pub fn ctx_at(&self, lane: &Lane, t_ms: i64) -> Option<u64> {
        self.root_ctx(lane).filter_map(|s| s[..s.partition_point(|x| x.0 <= t_ms)].last()).max_by_key(|s| s.0).map(|s| s.1)
    }

    /// Time of the first main request across the lane's roots.
    pub fn first_at(&self, lane: &Lane) -> Option<i64> {
        self.root_ctx(lane).filter_map(|s| s.first()).map(|s| s.0).min()
    }

    /// Largest context in `[from_ms, to_ms]`, counting the value already in effect at `from_ms`.
    pub fn max_between(&self, lane: &Lane, from_ms: i64, to_ms: i64) -> Option<u64> {
        let inside = self.root_ctx(lane).flat_map(|s| s.iter().filter(|x| x.0 > from_ms && x.0 <= to_ms)).map(|s| s.1).max();
        inside.max(self.ctx_at(lane, from_ms))
    }

    pub fn has_usage(&self, lane: &Lane) -> bool {
        self.root_ctx(lane).any(|s| !s.is_empty())
    }

    /// The lane's roots and every session they spawned.
    fn tree(&self, lane: &Lane) -> Vec<SessionIdx> {
        let mut out = Vec::new();
        let mut stack: Vec<SessionIdx> = self.roots(lane).to_vec();
        while let Some(i) = stack.pop() {
            if out.contains(&i) {
                continue;
            }
            out.push(i);
            stack.extend(&self.sessions[i].children);
        }
        out
    }

    /// Requests ended in `[from_ms, to_ms]` across the lane's roots and all their descendants,
    /// side calls included, and what they cost.
    pub fn spend_between(&self, lane: &Lane, from_ms: i64, to_ms: i64) -> Spend {
        let mut spend = Spend::default();
        for i in self.tree(lane) {
            for r in self.sessions[i].requests.iter().filter(|r| r.ended_ms >= from_ms && r.ended_ms <= to_ms) {
                spend.requests += 1;
                if let Some(c) = r.usage.cost {
                    spend.cost = Some(spend.cost.unwrap_or(0.0) + c);
                }
            }
        }
        spend
    }

    /// Context window of the lane's latest root request that reports one.
    pub fn ctx_window(&self, lane: &Lane) -> Option<u64> {
        self.roots(lane)
            .iter()
            .flat_map(|&i| self.sessions[i].requests.iter().filter(|r| r.side.is_none()))
            .filter_map(|r| r.ctx_window.map(|w| (r.ended_ms, w)))
            .max_by_key(|&(t, _)| t)
            .map(|(_, w)| w)
    }

    /// `(at_ms, before, after)` for the roots' compactions.
    pub fn compactions(&self, lane: &Lane) -> Vec<(i64, Option<u64>, Option<u64>)> {
        let mut out: Vec<_> = self
            .roots(lane)
            .iter()
            .flat_map(|&i| &self.sessions[i].events)
            .filter_map(|e| match e.kind {
                EventKind::Compaction { before, after } => Some((e.at_ms, before, after)),
                _ => None,
            })
            .collect();
        out.sort_by_key(|c| c.0);
        out
    }

    /// A built session attached as lane `lane_id`'s newest root.
    #[cfg(test)]
    pub fn insert_session(&mut self, lane_id: u64, session: Session) -> SessionIdx {
        let (i, _) = self.ensure(session.path.clone(), session.source);
        self.sessions[i] = session;
        self.lanes.entry(lane_id).or_default().push(i);
        i
    }

    /// A synthetic omp session for lane `lane_id`: one main request per `(at_ms, ctx, cost)`.
    #[cfg(test)]
    pub fn insert_samples(&mut self, lane_id: u64, samples: &[(i64, u64, Option<f64>)]) {
        use super::{Fact, Request, Stop, Usage};
        let (i, _) = self.ensure(PathBuf::from(format!("/test/lane-{lane_id}.jsonl")), Source::Omp);
        for &(at, ctx, cost) in samples {
            self.sessions[i].apply(Fact::Request(Request {
                started_ms: at,
                ended_ms: at,
                model: "m".into(),
                usage: Usage { cost, ..Usage::default() },
                ctx_tokens: ctx,
                ctx_window: None,
                thinking_ms: None,
                stop: Stop::ToolUse,
                side: None,
            }));
        }
        let roots = self.lanes.entry(lane_id).or_default();
        if !roots.contains(&i) {
            roots.push(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentStatus, Mark, State};

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rex-trail-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn roots(dir: &Path) -> Roots {
        Roots {
            claude_projects: dir.join("projects"),
            codex_sessions: dir.join("sessions"),
            codex_index: dir.join("index.jsonl"),
        }
    }

    fn lane_with(r: SessionRef, agent: &str) -> (State, usize) {
        let mut s = State::default();
        let i = s.open("w1:p1", "w1", 0, Mark::Seen, AgentStatus::Working, true);
        s.lanes[i].agent = Some(agent.into());
        s.lanes[i].sessions.push(r);
        (s, i)
    }

    fn assistant(ts: &str, cost: f64, extra: &str) -> String {
        format!(
            r#"{{"type":"message","timestamp":"{ts}","message":{{"role":"assistant","stopReason":"toolUse","usage":{{"input":10,"output":1,"cacheRead":90,"cacheWrite":0,"cost":{{"total":{cost}}}}},"content":[{extra}]}}}}"#
        )
    }

    #[test]
    fn omp_spend_counts_subagents_and_side_calls() {
        let dir = temp("omp");
        let parent = dir.join("p.jsonl");
        let call = r#"{"type":"toolCall","id":"toolu_1","name":"task","arguments":{"tasks":[{"name":"Child1"}]}}"#;
        let start = r#"{"type":"custom","customType":"tool_execution_start","timestamp":"2026-11-14T10:00:02Z","data":{"toolCallId":"toolu_1","toolName":"task"}}"#;
        let side = r#"{"type":"model_usage","timestamp":"2026-11-14T10:00:03Z","purpose":"title","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"cost":{"total":0.1}}}"#;
        std::fs::write(&parent, format!("{}\n{start}\n{side}\n", assistant("2026-11-14T10:00:01Z", 1.0, call))).unwrap();
        std::fs::create_dir_all(dir.join("p")).unwrap();
        std::fs::write(dir.join("p/Child1.jsonl"), format!("{}\n", assistant("2026-11-14T10:00:05Z", 0.5, ""))).unwrap();

        let r = SessionRef { agent: "omp".into(), kind: RefKind::Path, value: parent.to_string_lossy().into() };
        let (state, li) = lane_with(r, "omp");
        let mut store = Store::new(roots(&dir));
        store.refresh(&state.lanes, 0);
        let lane = &state.lanes[li];
        let spend = store.spend_between(lane, 0, i64::MAX);
        assert_eq!(spend.requests, 3);
        assert!((spend.cost.unwrap() - 1.6).abs() < 1e-9, "{spend:?}");
        let p = store.roots(lane)[0];
        let child = store.sessions[p].children[0];
        assert_eq!(store.sessions[p].spans[0].child, Some(child), "linked to its spawn span");
        assert_eq!(store.sessions[child].spawn_span, Some(0));
        assert_eq!(store.sessions[child].label.as_deref(), Some("Child1"));
        assert_eq!(store.ctx_at(lane, i64::MAX), Some(100), "only the parent's main request sets context");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ids_resolve_under_each_agents_root() {
        let dir = temp("locate");
        std::fs::create_dir_all(dir.join("projects/-x")).unwrap();
        std::fs::write(dir.join("projects/-x/abc.jsonl"), "").unwrap();
        std::fs::create_dir_all(dir.join("sessions/2026/11/11")).unwrap();
        let codex = dir.join("sessions/2026/11/11/rollout-2026-11-11T21-31-43-01a1.jsonl");
        std::fs::write(&codex, "").unwrap();
        let mut store = Store::new(roots(&dir));
        let claude = SessionRef { agent: "claude".into(), kind: RefKind::Id, value: "abc".into() };
        assert_eq!(store.locate(&claude, Source::Claude, 0), Some(dir.join("projects/-x/abc.jsonl")));
        let id = SessionRef { agent: "codex".into(), kind: RefKind::Id, value: "01a1".into() };
        assert_eq!(store.locate(&id, Source::Codex, 0), Some(codex));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
