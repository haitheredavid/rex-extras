//! Finds an agent process's session: the root session file omp or Codex holds open, or the
//! Claude Code session file naming its pid. Missing tools or files give None, leaving the lane
//! status-only.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::session::{RefKind, SessionRef};

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn env_dir(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key).filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(default))
}

/// The session for `agent` running as `pid`, and Claude Code's `status` for it.
pub fn session_ref(agent: &str, pid: u32) -> Option<(SessionRef, Option<String>)> {
    let path_ref = |p: &Path| SessionRef { agent: agent.to_string(), kind: RefKind::Path, value: p.to_string_lossy().into() };
    match agent {
        "omp" => omp_session(&home().join(".omp/agent/sessions"), &open_files(pid)).map(|p| (path_ref(p), None)),
        "codex" => {
            codex_session(&env_dir("CODEX_HOME", ".codex").join("sessions"), &open_files(pid)).map(|p| (path_ref(p), None))
        }
        "claude" => claude_session(&env_dir("CLAUDE_CONFIG_DIR", ".claude").join("sessions"), pid),
        _ => None,
    }
}

/// omp's root session, `<root>/<project dir>/<file>.jsonl`; subagent files sit a level deeper.
fn omp_session<'a>(root: &Path, files: &'a [PathBuf]) -> Option<&'a Path> {
    files.iter().map(PathBuf::as_path).find(|f| {
        f.extension().is_some_and(|e| e == "jsonl") && f.strip_prefix(root).is_ok_and(|rel| rel.components().count() == 2)
    })
}

/// Codex's rollout, `<root>/**/rollout-*.jsonl`.
fn codex_session<'a>(root: &Path, files: &'a [PathBuf]) -> Option<&'a Path> {
    files.iter().map(PathBuf::as_path).find(|f| {
        f.starts_with(root)
            && f.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeFile {
    pid: u32,
    session_id: String,
    #[serde(default)]
    status: Option<String>,
}

/// The `<dir>/*.json` whose `pid` is `pid`: its session id and status. File names aren't always pids.
fn claude_session(dir: &Path, pid: u32) -> Option<(SessionRef, Option<String>)> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).find_map(
        |p| {
            let f: ClaudeFile = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
            (f.pid == pid && !f.session_id.is_empty())
                .then(|| (SessionRef { agent: "claude".into(), kind: RefKind::Id, value: f.session_id }, f.status))
        },
    )
}

/// Files `pid` has open: `/proc/<pid>/fd` on Linux, `lsof` elsewhere.
fn open_files(pid: u32) -> Vec<PathBuf> {
    if cfg!(target_os = "linux") {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return Vec::new() };
        return fds.flatten().filter_map(|fd| std::fs::read_link(fd.path()).ok()).collect();
    }
    let Ok(out) = Command::new("lsof")
        .args(["-w", "-a", "-p", &pid.to_string(), "-Fn"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.strip_prefix('n')).map(PathBuf::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omp_takes_the_root_file_not_a_subagents() {
        let root = Path::new("/h/.omp/agent/sessions");
        let files: Vec<PathBuf> = [
            "/dev/ttys003",
            "/h/.omp/agent/sessions/-proj/2026-11-18_abc/0-Child.jsonl",
            "/h/.omp/agent/sessions/-proj/2026-11-18_abc.jsonl",
            "/h/.omp/agent/sessions/-proj/2026-11-18_def.jsonl",
        ]
        .map(PathBuf::from)
        .into();
        assert_eq!(omp_session(root, &files), Some(Path::new("/h/.omp/agent/sessions/-proj/2026-11-18_abc.jsonl")));
        assert_eq!(omp_session(root, &files[..2]), None, "a subagent file alone is not the session");
    }

    #[test]
    fn codex_takes_a_rollout_anywhere_under_sessions() {
        let root = Path::new("/h/.codex/sessions");
        let files: Vec<PathBuf> =
            ["/h/.codex/log/codex-tui.log", "/h/.codex/sessions/2026/11/18/rollout-2026-11-18T01-00-00-x.jsonl"]
                .map(PathBuf::from)
                .into();
        assert_eq!(codex_session(root, &files), Some(files[1].as_path()));
        assert_eq!(codex_session(root, &files[..1]), None);
    }

    #[test]
    fn claude_matches_on_the_pid_field_not_the_file_name() {
        let dir = std::env::temp_dir().join(format!("rex-trail-claude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("41.json"), r#"{"pid":42,"sessionId":"s-42","status":"busy"}"#).unwrap();
        std::fs::write(dir.join("42.json"), r#"{"pid":7,"sessionId":"s-7"}"#).unwrap();
        let (r, status) = claude_session(&dir, 42).unwrap();
        assert_eq!((r.kind, r.value.as_str(), status.as_deref()), (RefKind::Id, "s-42", Some("busy")));
        assert!(claude_session(&dir, 99).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
