//! Agent history log: JSONL, one file per UTC month, written by `rex-trail record` and loaded
//! on start so the track shows what happened before the panel opened.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{AgentStatus, HostEvent, PaneMeta};
use crate::session::SessionRef;

/// One line of the history log, written by `rex-trail record`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    pub ts_ms: i64,
    /// The Rex server (`session_name()`), `default` when unknown.
    pub session: String,
    /// Dotted event name, e.g. `pane.agent_status_changed`.
    pub event: String,
    pub pane_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<SessionRef>,
    /// `pane.moved`: the id the pane had before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_pane_id: Option<String>,
}

impl LogEvent {
    /// The model event this line records; None for events the fold doesn't track.
    pub fn host_event(&self) -> Option<HostEvent> {
        let meta = PaneMeta {
            pane_id: self.pane_id.clone(),
            workspace_id: self.workspace_id.clone(),
            workspace_label: self.workspace_label.clone(),
            tab_id: self.tab_id.clone(),
            tab_label: self.tab_label.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: self.cwd.clone(),
            agent: self.agent.clone(),
            status: self.status,
            session: self.agent_session.clone(),
            ..PaneMeta::default()
        };
        Some(match self.event.as_str() {
            "pane.created" => HostEvent::Created(meta),
            "pane.agent_detected" => HostEvent::Detected { meta, released: false },
            "pane.agent_status_changed" if self.status.is_some() => HostEvent::Status(meta),
            "pane.closed" => HostEvent::Closed { pane_id: meta.pane_id },
            "pane.exited" => HostEvent::Exited { pane_id: meta.pane_id },
            "pane.moved" => HostEvent::Moved { previous_pane_id: self.previous_pane_id.clone()?, pane: meta },
            _ => return None,
        })
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// `$<override>`, else `$<xdg>/rex-extras/trail`, else `~/<fallback>/rex-extras/trail`.
pub fn xdg_dir(override_key: &str, xdg_key: &str, fallback: &str) -> PathBuf {
    if let Some(dir) = env_nonempty(override_key) {
        return PathBuf::from(dir);
    }
    let base = env_nonempty(xdg_key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback));
    base.join("rex-extras/trail")
}

/// `$REX_TRAIL_STATE_DIR`, else `$XDG_STATE_HOME/rex-extras/trail`, else `~/.local/state/rex-extras/trail`.
pub fn log_dir() -> PathBuf {
    xdg_dir("REX_TRAIL_STATE_DIR", "XDG_STATE_HOME", ".local/state")
}

fn month_file(dir: &Path, year: i32, month: u32) -> PathBuf {
    dir.join(format!("events-{year:04}-{month:02}.jsonl"))
}

fn utc(ts_ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ts_ms).unwrap_or_default()
}

/// `dir/events-YYYY-MM.jsonl` for the UTC month of `ts_ms`.
pub fn log_file(dir: &Path, ts_ms: i64) -> PathBuf {
    let t = utc(ts_ms);
    month_file(dir, t.year(), t.month())
}

fn previous_month(year: i32, month: u32) -> (i32, u32) {
    if month <= 1 { (year - 1, 12) } else { (year, month - 1) }
}

fn parse_lines(text: &str) -> impl Iterator<Item = LogEvent> + '_ {
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok())
}

/// Events of the month files the window from `since_ms` to `now_ms` touches (the previous UTC month
/// only when `since_ms` falls before this one), oldest first. Missing files, unparseable lines and
/// a torn trailing line from a concurrent append are skipped.
pub fn load_events(dir: &Path, now_ms: i64, since_ms: i64) -> Vec<LogEvent> {
    let now = utc(now_ms);
    let month_start = Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0).single().map_or(0, |t| t.timestamp_millis());
    let mut paths = Vec::new();
    if since_ms < month_start {
        let (py, pm) = previous_month(now.year(), now.month());
        paths.push(month_file(dir, py, pm));
    }
    paths.push(month_file(dir, now.year(), now.month()));
    let mut events = Vec::new();
    for path in paths {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        events.extend(parse_lines(&String::from_utf8_lossy(&bytes)));
    }
    events.sort_by_key(|e| e.ts_ms);
    events
}

/// The Rex server this runs against: `$REX_SERVER`, else the server's primary listener (the same
/// address, for a recorder Rex started without the pane environment), with every byte outside
/// `[A-Za-z0-9._-]` replaced by `_`; None when neither is known.
pub fn server_name() -> Option<String> {
    env_nonempty("REX_SERVER").or_else(crate::rex::server_endpoint).map(|s| session_from(Some(s)))
}

/// `server_name()`, else `default`.
pub fn session_name() -> String {
    server_name().unwrap_or_else(|| session_from(None))
}

fn session_from(server: Option<String>) -> String {
    server
        .map(|s| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"._-".contains(&b) { b as char } else { '_' }).collect())
        .unwrap_or_else(|| "default".into())
}

/// Events of `session` at or after `since_ms`, oldest first.
pub fn load_since(dir: &Path, session: &str, now_ms: i64, since_ms: i64) -> Vec<LogEvent> {
    load_events(dir, now_ms, since_ms).into_iter().filter(|e| e.session == session && e.ts_ms >= since_ms).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rex-trail-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn line(ts_ms: i64, session: &str) -> String {
        format!(r#"{{"ts_ms":{ts_ms},"session":"{session}","event":"pane.closed","pane_id":"w1:p1","workspace_id":"w1"}}"#)
    }

    fn ms(y: i32, m: u32, d: u32) -> i64 {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap().timestamp_millis()
    }

    #[test]
    fn load_reads_previous_month_only_when_the_window_crosses_it() {
        let dir = temp("months");
        std::fs::write(dir.join("events-2026-10.jsonl"), line(ms(2026, 10, 31), "default") + "\n").unwrap();
        std::fs::write(dir.join("events-2026-11.jsonl"), line(ms(2026, 11, 14), "default") + "\n").unwrap();
        let now = ms(2026, 11, 14);
        assert_eq!(load_events(&dir, now, now - 3_600_000).len(), 1, "a window inside November reads one file");
        assert_eq!(load_events(&dir, now, ms(2026, 10, 30)).len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_skips_torn_and_foreign_session_lines() {
        let dir = temp("torn");
        let now = ms(2026, 11, 14);
        let text = format!("{}\n{}\n{}", line(now - 10, "default"), line(now - 5, "other"), &line(now, "default")[..20]);
        std::fs::write(dir.join("events-2026-11.jsonl"), text).unwrap();
        let events = load_since(&dir, "default", now, now - 1000);
        assert_eq!(events.iter().map(|e| e.ts_ms).collect::<Vec<_>>(), [now - 10]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn session_name_from_server_address() {
        assert_eq!(
            session_from(Some("unix:///Users/u/Library/Application%20Support/rex/server.sock".into())),
            "unix____Users_u_Library_Application_20Support_rex_server.sock"
        );
        assert_eq!(session_from(Some("studio-1.local".into())), "studio-1.local");
        assert_eq!(session_from(None), "default");
    }
}
