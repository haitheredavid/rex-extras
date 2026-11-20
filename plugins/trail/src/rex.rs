//! Rex host access through the `rex` CLI: one-shot `api call`s, a snapshot of every session via
//! `rex do`, and the `rex events` stream, mapped onto the host-neutral wire types the model folds.

mod resolve;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::model::{AgentStatus, PaneMeta};
use crate::session::{RefKind, SessionRef};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
}

/// One terminal block: `pane_id` and `terminal_id` are its block id, `workspace_id` its session,
/// `tab_id` its window.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PaneInfo {
    pub pane_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
}

/// An agent's session file or id, e.g. `{"agent":"omp","kind":"path","value":"….jsonl"}`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentSession {
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

impl AgentSession {
    /// The ref, when `kind` is one trail knows and `value` is set; `agent` falls back to the pane's.
    fn session_ref(&self, pane_agent: Option<&str>) -> Option<SessionRef> {
        let kind = match self.kind.as_str() {
            "id" => RefKind::Id,
            "path" => RefKind::Path,
            _ => return None,
        };
        let agent = if self.agent.is_empty() { pane_agent.unwrap_or_default().to_string() } else { self.agent.clone() };
        (!self.value.is_empty()).then(|| SessionRef { agent, kind, value: self.value.clone() })
    }
}

impl From<SessionRef> for AgentSession {
    fn from(r: SessionRef) -> AgentSession {
        let kind = match r.kind {
            RefKind::Id => "id",
            RefKind::Path => "path",
        };
        AgentSession { agent: r.agent, kind: kind.into(), value: r.value }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

impl From<&PaneInfo> for PaneMeta {
    fn from(p: &PaneInfo) -> PaneMeta {
        PaneMeta {
            pane_id: p.pane_id.clone(),
            workspace_id: p.workspace_id.clone(),
            tab_id: p.tab_id.clone(),
            terminal_id: p.terminal_id.clone(),
            agent: p.agent.clone(),
            title: p.title.clone(),
            terminal_title: p.terminal_title.clone(),
            cwd: p.cwd.clone(),
            status: Some(p.agent_status),
            session: p.agent_session.as_ref().and_then(|s| s.session_ref(p.agent.as_deref())),
            ..PaneMeta::default()
        }
    }
}

/// `$REX_BIN_DIR/rex`, else the macOS app bundle's, else `rex` on PATH.
pub fn rex_bin() -> PathBuf {
    let bundled = std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join("Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex")
    });
    std::env::var_os("REX_BIN_DIR")
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("rex"))
        .into_iter()
        .chain(bundled)
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("rex"))
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).lines().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string()
}

/// Runs `rex` with `args`, `stdin` written to it; its stdout, or an error naming `what` with the
/// first stderr line. trail never starts a server: a recorder outliving `rex server stop` would.
fn run(what: &str, args: &[&str], stdin: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let mut child = Command::new(rex_bin())
        .arg("--autostart=false")
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("rex not found ({})", rex_bin().display()))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(text.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("rex {what}: {}", first_line(&out.stderr));
    }
    Ok(out.stdout)
}

/// `rex api call METHOD PARAMS`: the result JSON.
pub fn call(method: &str, params: Value) -> anyhow::Result<Value> {
    let out = run(method, &["--timeout", "2s", "api", "call", method, &params.to_string()], None)?;
    serde_json::from_slice(&out).with_context(|| format!("rex {method}: parse result"))
}

/// The server's primary listener address, as panes get it in `REX_SERVER`.
pub fn server_endpoint() -> Option<String> {
    let status = call("server.status", serde_json::json!({})).ok()?;
    let listeners = status["listeners"].as_array()?;
    let primary = listeners.iter().find(|l| l["primary"] == true).or(listeners.first())?;
    primary["endpoint"].as_str().map(str::to_string)
}

/// `rex do ACTION ARGS…` as the connected app client, e.g. `session.select session_id=S`.
pub fn client_action(action: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut all = vec!["--timeout", "2s", "do", action];
    all.extend_from_slice(args);
    run(action, &all, None).map(drop)
}

/// An empty Lua table encodes as `{}`; read it, and null, as an empty list.
fn seq<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(Vec::new()),
        Value::Object(m) if m.is_empty() => Ok(Vec::new()),
        v => serde_json::from_value(v).map_err(D::Error::custom),
    }
}

/// What `rex/snapshot.lua` returns.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Raw {
    #[serde(default, deserialize_with = "seq")]
    pub sessions: Vec<RawSession>,
    #[serde(default)]
    pub focused_session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSession {
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub active_window_id: Option<String>,
    #[serde(default, deserialize_with = "seq")]
    pub windows: Vec<RawWindow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWindow {
    pub window_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub focused_block_id: Option<String>,
    #[serde(default, deserialize_with = "seq")]
    pub blocks: Vec<RawBlock>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawBlock {
    pub block_id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    process: Option<Process>,
    #[serde(default)]
    status: Option<ProgramStatus>,
}

#[derive(Debug, Clone, Deserialize)]
struct Process {
    #[serde(default)]
    foreground: Option<Foreground>,
}

#[derive(Debug, Clone, Deserialize)]
struct Foreground {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProgramStatus {
    #[serde(default, deserialize_with = "seq")]
    records: Vec<StatusRecord>,
}

/// One OSC 7501 program status record.
#[derive(Debug, Clone, Deserialize)]
struct StatusRecord {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    effective_app: Option<String>,
    #[serde(default)]
    owner: Option<Owner>,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Owner {
    #[serde(default)]
    pid: Option<u32>,
}

/// The agent trail knows a program as: `session::Source::of_agent` keys.
pub fn agent_of(app: Option<&str>, fg_name: Option<&str>) -> Option<&'static str> {
    match app.or(fg_name)? {
        "omp" => Some("omp"),
        "claude" | "claude-code" => Some("claude"),
        "codex" => Some("codex"),
        _ => None,
    }
}

/// An OSC 7501 state; `error` ends the turn, so it reads as done.
fn status_of(state: &str) -> AgentStatus {
    match state {
        "idle" => AgentStatus::Idle,
        "working" => AgentStatus::Working,
        "blocked" => AgentStatus::Blocked,
        "done" | "error" => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}

/// A Claude Code session file's `status`.
fn claude_status(status: &str) -> AgentStatus {
    match status {
        "busy" => AgentStatus::Working,
        "idle" => AgentStatus::Idle,
        _ => AgentStatus::Unknown,
    }
}

/// Rex labels shell blocks `shell` until renamed; that says nothing, so it isn't a title.
const DEFAULT_LABEL: &str = "shell";

/// A resolved session ref and Claude's session-file status, for an agent at a pid in a block.
type Resolved = Option<(SessionRef, Option<String>)>;

/// Maps the raw snapshot onto panes, asking `resolve(block_id, agent, pid)` for agent sessions.
pub(crate) fn map_raw(raw: &Raw, mut resolve: impl FnMut(&str, &str, u32) -> Resolved) -> Snapshot {
    let mut snap = Snapshot { focused_workspace_id: raw.focused_session_id.clone(), ..Snapshot::default() };
    for s in &raw.sessions {
        snap.workspaces.push(WorkspaceInfo { workspace_id: s.session_id.clone(), label: s.label.clone() });
        if raw.focused_session_id.as_deref() == Some(s.session_id.as_str()) {
            snap.focused_tab_id.clone_from(&s.active_window_id);
        }
        for w in &s.windows {
            snap.tabs.push(TabInfo { tab_id: w.window_id.clone(), label: w.label.clone() });
            for b in &w.blocks {
                let fg = b.process.as_ref().and_then(|p| p.foreground.as_ref());
                let root = b.status.as_ref().and_then(|st| st.records.iter().find(|r| r.id.as_deref().is_none_or(str::is_empty)));
                let agent = agent_of(root.and_then(|r| r.effective_app.as_deref()), fg.and_then(|f| f.name.as_deref()));
                let pid = root.and_then(|r| r.owner.as_ref()?.pid).or(fg.and_then(|f| f.pid));
                let resolved = agent.zip(pid).and_then(|(a, pid)| resolve(&b.block_id, a, pid));
                let status = match (root.and_then(|r| r.state.as_deref()), &resolved) {
                    (Some(state), _) => status_of(state),
                    (None, Some((_, Some(claude)))) if agent == Some("claude") => claude_status(claude),
                    _ => AgentStatus::Unknown,
                };
                snap.panes.push(PaneInfo {
                    pane_id: b.block_id.clone(),
                    workspace_id: s.session_id.clone(),
                    tab_id: Some(w.window_id.clone()),
                    terminal_id: Some(b.block_id.clone()),
                    agent: agent.map(str::to_string),
                    title: b.label.clone().filter(|l| l != DEFAULT_LABEL),
                    terminal_title: None,
                    cwd: fg.and_then(|f| f.cwd.clone()),
                    agent_status: status,
                    agent_session: resolved.map(|(r, _)| r.into()),
                });
            }
        }
    }
    snap
}

/// How long a found session is trusted before its pid's open files are listed again (omp opens a
/// new file on `/new`), and how long a miss waits before the next try.
const RECHECK_FOUND: Duration = Duration::from_secs(30);
const RETRY_MISSING: Duration = Duration::from_secs(10);

struct Cached {
    found: Resolved,
    at: Instant,
}

/// Snapshots with session resolution cached per block and pid; `raw` is the latest raw snapshot.
#[derive(Default)]
pub struct Host {
    cache: HashMap<(String, u32), Cached>,
    pub raw: Raw,
}

impl Host {
    pub fn snapshot(&mut self) -> anyhow::Result<Snapshot> {
        let out = run("do", &["--timeout", "5s", "do", "-", "--json"], Some(include_str!("rex/snapshot.lua")))?;
        let raw: Raw = serde_json::from_slice(&out).context("rex do: parse snapshot")?;
        let now = Instant::now();
        let mut seen = Vec::new();
        let snap = map_raw(&raw, |block, agent, pid| {
            let key = (block.to_string(), pid);
            seen.push(key.clone());
            if let Some(c) = self.cache.get(&key) {
                // Claude's status lives in its session file, so a found Claude session is reread.
                let ttl = match &c.found {
                    Some(_) if agent == "claude" => Duration::ZERO,
                    Some(_) => RECHECK_FOUND,
                    None => RETRY_MISSING,
                };
                if now.duration_since(c.at) < ttl {
                    return c.found.clone();
                }
            }
            let found = resolve::session_ref(agent, pid);
            self.cache.insert(key, Cached { found: found.clone(), at: now });
            found
        });
        self.cache.retain(|k, _| seen.contains(k));
        self.raw = raw;
        Ok(snap)
    }
}

#[derive(Debug, PartialEq)]
pub enum Msg {
    /// Something a snapshot shows may have changed.
    Changed,
    ActiveWindow {
        session_id: String,
        window_id: String,
    },
    /// The event stream ended or never started.
    Lost(String),
}

/// Server events that change what a snapshot shows.
const SERVER_CHANGES: [&str; 2] = ["session_created", "session_destroyed"];
/// Session and terminal events that do. Others, like `title_changed` (omp's spinner, several a
/// second) and `client_connected` (every `rex` call, ours included), would only cost snapshots.
const SESSION_CHANGES: [&str; 13] = [
    "window_created",
    "window_closed",
    "window_label_changed",
    "block_created",
    "block_closed",
    "session_label_changed",
    "session_view_changed",
    "program_status_changed",
    "program_status_removed",
    "process_changed",
    "child_exited",
    "pwd_changed",
    "active_window_changed",
];

/// One `rex events --json` line as a message; None for events trail ignores.
fn parse_line(line: &str) -> Option<Msg> {
    let v: Value = serde_json::from_str(line).ok()?;
    let kind = v["type"].as_str()?;
    if kind != "session_event" {
        return SERVER_CHANGES.contains(&kind).then_some(Msg::Changed);
    }
    let event = &v["payload"]["event"];
    let name = match event["type"].as_str()? {
        "block_event" => event["payload"]["name"].as_str()?,
        other => other,
    };
    if name == "active_window_changed" {
        return Some(Msg::ActiveWindow {
            session_id: v["payload"]["session_id"].as_str()?.to_string(),
            window_id: event["payload"]["window_id"].as_str()?.to_string(),
        });
    }
    SESSION_CHANGES.contains(&name).then_some(Msg::Changed)
}

/// `rex events --all --json`, read on a thread into `tx`. Dropping it ends the stream.
pub struct Subscriber {
    child: Option<Child>,
}

impl Subscriber {
    /// Starts the stream; a spawn failure arrives as `Msg::Lost`.
    pub fn start(tx: Sender<Msg>) -> Subscriber {
        let spawned = Command::new(rex_bin())
            .args(["--autostart=false", "events", "--all", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Msg::Lost(format!("rex events: {e}")));
                return Subscriber { child: None };
            }
        };
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        thread::spawn(move || {
            if let Some(out) = stdout {
                for line in BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    let Some(msg) = parse_line(&line) else { continue };
                    let also_changed = matches!(msg, Msg::ActiveWindow { .. });
                    if tx.send(msg).is_err() || (also_changed && tx.send(Msg::Changed).is_err()) {
                        return;
                    }
                }
            }
            let mut err = Vec::new();
            if let Some(mut e) = stderr {
                let _ = e.read_to_end(&mut err);
            }
            let reason = if err.is_empty() { "event stream ended".to_string() } else { first_line(&err) };
            let _ = tx.send(Msg::Lost(format!("rex events: {reason}")));
        });
        Subscriber { child: Some(child) }
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_of_prefers_the_status_app_and_knows_three_agents() {
        assert_eq!(agent_of(Some("claude-code"), Some("node")), Some("claude"));
        assert_eq!(agent_of(None, Some("claude")), Some("claude"));
        assert_eq!(agent_of(Some("omp"), Some("zsh")), Some("omp"));
        assert_eq!(agent_of(None, Some("codex")), Some("codex"));
        assert_eq!(agent_of(Some("vim"), Some("omp")), None, "the status app wins over the process name");
        assert_eq!(agent_of(None, Some("zsh")), None);
        assert_eq!(agent_of(None, None), None);
    }

    #[test]
    fn status_states_map_onto_agent_status() {
        assert_eq!(status_of("working"), AgentStatus::Working);
        assert_eq!(status_of("blocked"), AgentStatus::Blocked);
        assert_eq!(status_of("error"), AgentStatus::Done);
        assert_eq!(status_of("later"), AgentStatus::Unknown);
        assert_eq!(claude_status("busy"), AgentStatus::Working);
    }

    #[test]
    fn claude_without_a_record_takes_its_session_file_status() {
        let raw: Raw = serde_json::from_value(serde_json::json!({"sessions": [{"session_id": "s", "windows": [{"window_id": "w",
            "blocks": [{"block_id": "b", "label": "shell", "process": {"foreground": {"pid": 7, "name": "claude"}}, "status": {"records": {}}}]}]}]}))
        .unwrap();
        let r = SessionRef { agent: "claude".into(), kind: RefKind::Id, value: "abc".into() };
        let snap = map_raw(&raw, |block, agent, pid| {
            assert_eq!((block, agent, pid), ("b", "claude", 7));
            Some((r.clone(), Some("busy".into())))
        });
        let p = &snap.panes[0];
        assert_eq!((p.agent.as_deref(), p.agent_status, p.title.as_deref()), (Some("claude"), AgentStatus::Working, None));
        assert_eq!(PaneMeta::from(p).session, Some(r));
    }

    #[test]
    fn parse_line_keeps_only_events_that_change_the_snapshot() {
        let session = |event: &str| format!(r#"{{"type":"session_event","payload":{{"session_id":"s1","event":{event}}}}}"#);
        let block = |name: &str| {
            session(&format!(r#"{{"type":"block_event","payload":{{"block_id":"b","name":"{name}","payload":{{}}}}}}"#))
        };
        assert_eq!(parse_line(&block("program_status_changed")), Some(Msg::Changed));
        assert_eq!(parse_line(&block("title_changed")), None);
        assert_eq!(parse_line(&session(r#"{"type":"client_connected","payload":{}}"#)), None);
        assert_eq!(parse_line(&session(r#"{"type":"block_closed","payload":{"block_id":"b"}}"#)), Some(Msg::Changed));
        assert_eq!(
            parse_line(&session(r#"{"type":"active_window_changed","payload":{"window_id":"w2"}}"#)),
            Some(Msg::ActiveWindow { session_id: "s1".into(), window_id: "w2".into() })
        );
        assert_eq!(parse_line(r#"{"type":"session_created","payload":{}}"#), Some(Msg::Changed));
        assert_eq!(parse_line("not json"), None);
    }
}
