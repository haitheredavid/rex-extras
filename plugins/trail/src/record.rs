//! `rex-trail record`: the always-on recorder. Follows the Rex event stream, snapshots on change,
//! and appends one JSON line per agent lifecycle change to the monthly history log, so the trail
//! keeps its lanes and sessions across restarts. One recorder per state dir (`recorder.lock`).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::Datelike;

use crate::history::{LogEvent, log_dir, log_file, server_name};
use crate::model::{AgentStatus, PaneMeta};
use crate::rex::{Host, Msg, Snapshot, Subscriber};

/// Events arrive in bursts; one snapshot covers a burst.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
/// A snapshot this often even without events, and this soon after a failed one.
pub const SNAPSHOT_EVERY: Duration = Duration::from_secs(5);
pub const RETRY_EVERY: Duration = Duration::from_secs(2);
/// Restart backoff after the event stream ends: doubles per consecutive loss up to the cap.
pub const RESUBSCRIBE_FIRST: Duration = Duration::from_secs(1);
pub const RESUBSCRIBE_MAX: Duration = Duration::from_secs(30);

pub fn run() -> anyhow::Result<()> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let lock_path = dir.join("recorder.lock");
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", lock_path.display())),
    }

    // Named once known: without `REX_SERVER` the name comes from the server, and history logged
    // under a guess would never show in a pane's trail.
    let mut session: Option<String> = None;
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut host = Host::default();
    let mut agents: Option<BTreeMap<String, PaneMeta>> = None;
    let mut _sub = Some(Subscriber::start(tx.clone()));
    let mut resubscribe_at: Option<Instant> = None;
    let mut backoff = RESUBSCRIBE_FIRST;
    let mut snapshot_at = Instant::now();

    loop {
        let now = Instant::now();
        let next = resubscribe_at.map_or(snapshot_at, |r| r.min(snapshot_at));
        match rx.recv_timeout(next.saturating_duration_since(now)) {
            Ok(Msg::Lost(_)) => {
                _sub = None;
                resubscribe_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(RESUBSCRIBE_MAX);
            }
            Ok(_) => {
                backoff = RESUBSCRIBE_FIRST;
                snapshot_at = snapshot_at.min(Instant::now() + DEBOUNCE);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => unreachable!("run holds a sender"),
        }
        let now = Instant::now();
        if resubscribe_at.is_some_and(|t| now >= t) {
            resubscribe_at = None;
            _sub = Some(Subscriber::start(tx.clone()));
        }
        if now < snapshot_at {
            continue;
        }
        match host.snapshot() {
            Ok(snap) => {
                if session.is_none() {
                    session = server_name();
                }
                let Some(session) = session.as_deref() else {
                    snapshot_at = Instant::now() + RETRY_EVERY;
                    continue;
                };
                let current = agent_metas(&snap);
                let ts_ms = chrono::Utc::now().timestamp_millis();
                let events: Vec<LogEvent> =
                    diff(agents.as_ref(), &current).into_iter().map(|(event, m)| log_event(event, m, session, ts_ms)).collect();
                // A failed write keeps the old state, so the next snapshot logs the same changes.
                match append(&dir, ts_ms, &events) {
                    Ok(()) => agents = Some(current),
                    Err(e) => eprintln!("rex-trail record: {e:#}"),
                }
                snapshot_at = Instant::now() + SNAPSHOT_EVERY;
            }
            // Rex unreachable: nothing to record until it's back.
            Err(_) => snapshot_at = Instant::now() + RETRY_EVERY,
        }
    }
}

/// Blocks running an agent, by block id, with their session and window labels.
fn agent_metas(snap: &Snapshot) -> BTreeMap<String, PaneMeta> {
    snap.panes
        .iter()
        .filter(|p| p.agent.is_some())
        .map(|p| {
            let mut m = PaneMeta::from(p);
            m.workspace_label = snap.workspaces.iter().find(|w| w.workspace_id == p.workspace_id).and_then(|w| w.label.clone());
            m.tab_label = snap.tabs.iter().find(|t| p.tab_id.as_ref() == Some(&t.tab_id)).and_then(|t| t.label.clone());
            (p.pane_id.clone(), m)
        })
        .collect()
}

fn known(m: &PaneMeta) -> bool {
    m.status.is_some_and(|s| s != AgentStatus::Unknown)
}

/// What changed from `prev` to `cur`, as log events. With no `prev` (the recorder just started)
/// running agents are logged as detected, not created, so a restart doesn't split their lanes.
fn diff(prev: Option<&BTreeMap<String, PaneMeta>>, cur: &BTreeMap<String, PaneMeta>) -> Vec<(&'static str, PaneMeta)> {
    let mut out = Vec::new();
    let Some(prev) = prev else {
        out.extend(cur.values().map(|m| ("pane.agent_detected", m.clone())));
        return out;
    };
    for (id, m) in cur {
        match prev.get(id) {
            None => {
                out.push(("pane.created", PaneMeta { status: None, ..m.clone() }));
                if known(m) {
                    out.push(("pane.agent_status_changed", m.clone()));
                }
            }
            Some(p) => {
                if m.status != p.status && known(m) {
                    out.push(("pane.agent_status_changed", m.clone()));
                }
                if m.session.is_some() && m.session != p.session {
                    out.push(("pane.agent_detected", m.clone()));
                }
            }
        }
    }
    for (id, p) in prev {
        if !cur.contains_key(id) {
            out.push((
                "pane.closed",
                PaneMeta { pane_id: id.clone(), workspace_id: p.workspace_id.clone(), ..PaneMeta::default() },
            ));
        }
    }
    out
}

fn log_event(event: &str, m: PaneMeta, session: &str, ts_ms: i64) -> LogEvent {
    LogEvent {
        ts_ms,
        session: session.to_string(),
        event: event.to_string(),
        pane_id: m.pane_id,
        workspace_id: m.workspace_id,
        workspace_label: m.workspace_label,
        tab_label: m.tab_label,
        cwd: m.cwd,
        agent: m.agent,
        status: m.status.filter(|s| *s != AgentStatus::Unknown),
        tab_id: m.tab_id,
        terminal_id: m.terminal_id,
        agent_session: m.session,
        previous_pane_id: None,
    }
}

fn append(dir: &Path, ts_ms: i64, events: &[LogEvent]) -> anyhow::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = log_file(dir, ts_ms);
    let new_month = !path.exists();
    let mut text = String::new();
    for e in events {
        text.push_str(&serde_json::to_string(e)?);
        text.push('\n');
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .with_context(|| format!("append {}", path.display()))?;
    if new_month {
        prune_months(dir, ts_ms, crate::config::config().retain_months);
    }
    Ok(())
}

/// Deletes month files `retain` or more months older than `now_ms`'s month; 0 keeps them all.
/// Runs when a new month's file starts, and ignores what it can't delete.
fn prune_months(dir: &Path, now_ms: i64, retain: u32) {
    if retain == 0 {
        return;
    }
    let now = chrono::DateTime::from_timestamp_millis(now_ms).unwrap_or_default();
    let current = now.year() * 12 + now.month() as i32;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let Some(ym) = name.strip_prefix("events-").and_then(|n| n.strip_suffix(".jsonl")) else { continue };
        let Some((y, m)) = ym.split_once('-').and_then(|(y, m)| Some((y.parse::<i32>().ok()?, m.parse::<i32>().ok()?))) else {
            continue;
        };
        if y * 12 + m <= current - retain as i32 {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HostEvent, State};
    use crate::session::{RefKind, SessionRef};

    fn agent(id: &str, status: AgentStatus) -> PaneMeta {
        PaneMeta {
            pane_id: id.into(),
            workspace_id: "s1".into(),
            workspace_label: Some("Onboarding".into()),
            tab_id: Some("w1".into()),
            terminal_id: Some(id.into()),
            agent: Some("omp".into()),
            cwd: Some("/repo".into()),
            status: Some(status),
            ..PaneMeta::default()
        }
    }

    fn names(events: &[(&str, PaneMeta)]) -> Vec<String> {
        events.iter().map(|(e, m)| format!("{e} {}", m.pane_id)).collect()
    }

    #[test]
    fn diff_logs_lifecycle_changes_only() {
        use AgentStatus::*;
        let first: BTreeMap<_, _> = [("b1".to_string(), agent("b1", Working))].into();
        assert_eq!(names(&diff(None, &first)), ["pane.agent_detected b1"], "a fresh recorder doesn't create running agents");
        assert!(diff(Some(&first), &first).is_empty(), "nothing changed");

        let mut next = first.clone();
        next.get_mut("b1").unwrap().status = Some(Idle);
        next.insert("b2".into(), agent("b2", Unknown));
        next.insert("b3".into(), agent("b3", Blocked));
        let d = diff(Some(&first), &next);
        assert_eq!(
            names(&d),
            ["pane.agent_status_changed b1", "pane.created b2", "pane.created b3", "pane.agent_status_changed b3"]
        );
        assert_eq!(d[1].1.status, None, "created carries no status; the status line follows");

        let mut resolved = next.clone();
        resolved.get_mut("b1").unwrap().session =
            Some(SessionRef { agent: "omp".into(), kind: RefKind::Path, value: "/s/a.jsonl".into() });
        resolved.remove("b2");
        let d = diff(Some(&next), &resolved);
        assert_eq!(names(&d), ["pane.agent_detected b1", "pane.closed b2"]);
        assert_eq!((d[1].1.workspace_id.as_str(), d[1].1.agent.as_deref()), ("s1", None));
    }

    #[test]
    fn append_recreates_a_missing_state_dir() {
        let dir = std::env::temp_dir().join(format!("rex-trail-append-{}", std::process::id())).join("gone");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        let e = log_event("pane.closed", agent("b1", AgentStatus::Idle), "srv", 5);
        append(&dir, 5, &[e]).unwrap();
        assert_eq!(std::fs::read_to_string(log_file(&dir, 5)).unwrap().lines().count(), 1);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn logged_lines_replay_into_one_lane_per_agent() {
        use AgentStatus::*;
        let first: BTreeMap<_, _> = [("b1".to_string(), agent("b1", Working))].into();
        let mut idle = first.clone();
        idle.get_mut("b1").unwrap().status = Some(Idle);
        let mut events: Vec<LogEvent> = diff(None, &first).into_iter().map(|(e, m)| log_event(e, m, "srv", 10)).collect();
        events.extend(diff(Some(&first), &idle).into_iter().map(|(e, m)| log_event(e, m, "srv", 20)));
        events.extend(diff(Some(&idle), &BTreeMap::new()).into_iter().map(|(e, m)| log_event(e, m, "srv", 30)));
        let line = serde_json::to_string(&events[0]).unwrap();
        assert!(line.contains(r#""workspace_label":"Onboarding""#) && line.contains(r#""cwd":"/repo""#), "{line}");

        let mut s = State::default();
        for e in &events {
            let h: HostEvent = e.host_event().unwrap();
            s.apply(&h, e.ts_ms, false);
        }
        assert_eq!(s.lanes.len(), 1);
        let statuses: Vec<AgentStatus> = s.lanes[0].steps.iter().map(|st| st.status).collect();
        assert_eq!(statuses, [Working, Idle, Idle]);
        assert_eq!(s.lanes[0].closed_ms, Some(30));
    }

    #[test]
    fn retention_deletes_old_months_on_rollover() {
        let dir = std::env::temp_dir().join(format!("rex-trail-retain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for m in ["2026-07", "2026-08", "2026-09", "2026-10", "2026-11"] {
            std::fs::write(dir.join(format!("events-{m}.jsonl")), "").unwrap();
        }
        std::fs::write(dir.join("prefs.toml"), "").unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:05Z").unwrap().timestamp_millis();
        prune_months(&dir, now, 3);
        let mut left: Vec<String> =
            std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["events-2026-09.jsonl", "events-2026-10.jsonl", "events-2026-11.jsonl", "prefs.toml"]);
        prune_months(&dir, now, 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 4, "0 keeps every month");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
