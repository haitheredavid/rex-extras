//! Agent sessions, whatever the agent: turns, model requests, tool spans and point events, built
//! from the `Fact`s that per-agent decoders (omp, Claude Code, Codex) read out of session files.
//! Text is never stored; `SourceRef`s point back into the file for reading it on demand.

mod builder;
mod claude;
mod codex;
pub mod index;
mod omp;
mod store;
mod tail;
pub mod text;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use store::{Roots, Store};

/// A pane's agent session: a file path or an id (`agent_session` in snapshots and the log).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionRef {
    #[serde(default)]
    pub agent: String,
    pub kind: RefKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Id,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Omp,
    Claude,
    Codex,
}

impl Source {
    /// The decoder for an agent name; None for agents without one.
    pub fn of_agent(agent: &str) -> Option<Source> {
        match agent {
            "omp" => Some(Source::Omp),
            "claude" | "claude-code" => Some(Source::Claude),
            "codex" => Some(Source::Codex),
            _ => None,
        }
    }
}

pub type SessionIdx = usize;

/// A byte range in a session's file, for reading text lazily; text itself is never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRef {
    pub session: SessionIdx,
    pub offset: u64,
    pub len: u32,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub source: Source,
    pub path: PathBuf,
    pub id: Option<String>,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub started_ms: Option<i64>,
    pub ended: Option<(i64, String)>,
    pub parent: Option<SessionIdx>,
    /// Span in `parent` that spawned this session.
    pub spawn_span: Option<u32>,
    pub children: Vec<SessionIdx>,
    pub turns: Vec<Turn>,
    pub requests: Vec<Request>,
    pub spans: Vec<Span>,
    pub events: Vec<Event>,
    /// Main-request context by end time, sorted; side calls excluded.
    pub ctx: Vec<(i64, u64)>,
    /// Bumped on every applied fact; the timeline index syncs against it.
    pub revision: u64,
    open_spans: HashMap<String, u32>,
    /// Child link key (omp task name, Claude tool_use id, Codex thread id) → spawning span id.
    pub(crate) child_keys: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub trigger: Trigger,
    pub prompt: Option<SourceRef>,
    pub prompt_chars: u32,
    pub outcome: Outcome,
    pub first_request: u32,
    pub first_span: u32,
    pub first_event: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    User,
    Agent,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Open,
    Completed,
    Aborted,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub started_ms: i64,
    pub ended_ms: i64,
    pub model: String,
    pub usage: Usage,
    pub ctx_tokens: u64,
    pub ctx_window: Option<u64>,
    pub thinking_ms: Option<u32>,
    pub stop: Stop,
    /// Purpose of a side call (omp `model_usage`: titles, summaries); None for the main loop.
    pub side: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    /// Dollars, when the agent records them.
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    ToolUse,
    EndTurn,
    Aborted,
    Error,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub id: String,
    pub kind: SpanKind,
    pub name: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub status: SpanStatus,
    /// The request that issued the call.
    pub request: Option<u32>,
    pub intent: Option<SourceRef>,
    pub child: Option<SessionIdx>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Tool(ToolCat),
    Subagent,
    WaitUser,
    WaitAgents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCat {
    Shell,
    Read,
    Search,
    Edit,
    Write,
    Web,
    Plan,
    Code,
    Mcp,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanStatus {
    Running,
    Ok,
    Error,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub at_ms: i64,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    Compaction { before: Option<u64>, after: Option<u64> },
    ModelChange(String),
    ModeChange(String),
    ApiError,
    Interrupt,
    Branch,
    Exit(String),
}

/// Everything a decoder can say about a session; `Session::apply` is the only place that knows structure.
#[derive(Debug, Clone, PartialEq)]
pub enum Fact {
    Meta {
        id: Option<String>,
        cwd: Option<String>,
        at_ms: Option<i64>,
    },
    Label(String),
    Prompt {
        at_ms: i64,
        trigger: Trigger,
        chars: u32,
        src: SourceRef,
    },
    Request(Request),
    /// Replaces the last request (a Claude message whose blocks arrive as several records).
    RequestUpdate(Request),
    SpanStart {
        id: String,
        at_ms: i64,
        kind: SpanKind,
        name: String,
        intent: Option<SourceRef>,
    },
    SpanEnd {
        id: String,
        at_ms: i64,
        status: SpanStatus,
        exit_code: Option<i32>,
    },
    Child {
        span_id: String,
        key: String,
    },
    TurnEnd {
        at_ms: i64,
        outcome: Outcome,
    },
    Point(Event),
}

pub trait Decoder {
    /// Cheap byte prefilter; false skips JSON parsing entirely.
    fn wants(&self, line: &[u8]) -> bool;
    fn decode(&mut self, line: &[u8], src: SourceRef, out: &mut Vec<Fact>);
}

/// A fresh decoder for `source`.
pub fn decoder(source: Source) -> Box<dyn Decoder> {
    match source {
        Source::Omp => Box::new(omp::Omp),
        Source::Claude => Box::new(claude::Claude::default()),
        Source::Codex => Box::new(codex::Codex::default()),
    }
}

/// Tool category from a tool name, across agents.
pub fn tool_cat(name: &str) -> ToolCat {
    let name = name.to_lowercase();
    if name.starts_with("mcp__") {
        return ToolCat::Mcp;
    }
    match name.as_str() {
        "bash" | "exec_command" | "exec" | "shell" | "write_stdin" | "bashoutput" | "killshell" => ToolCat::Shell,
        "read" | "notebookread" | "view" => ToolCat::Read,
        "grep" | "glob" | "ls" | "find" | "ast_grep" | "search" => ToolCat::Search,
        "edit" | "multiedit" | "apply_patch" | "ast_edit" | "notebookedit" => ToolCat::Edit,
        "write" => ToolCat::Write,
        "web_search" | "websearch" | "webfetch" | "fetch" | "browser" => ToolCat::Web,
        "todo" | "todowrite" | "update_plan" => ToolCat::Plan,
        "eval" | "lsp" | "debug" => ToolCat::Code,
        _ => ToolCat::Other,
    }
}

/// RFC 3339 timestamp in ms.
pub(crate) fn rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.timestamp_millis())
}

/// `312k`, `1.25M`, `2M`.
pub fn fmt_tokens(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{:.2}", n as f64 / 1e6).trim_end_matches('0').trim_end_matches('.').to_string() + "M",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_short() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(312_400), "312k");
        assert_eq!(fmt_tokens(1_250_000), "1.25M");
        assert_eq!(fmt_tokens(1_500_000), "1.5M");
        assert_eq!(fmt_tokens(2_000_000), "2M");
    }

    #[test]
    fn tool_names_map_to_categories() {
        assert_eq!(tool_cat("Bash"), ToolCat::Shell);
        assert_eq!(tool_cat("exec_command"), ToolCat::Shell);
        assert_eq!(tool_cat("mcp__custom-tools__bash"), ToolCat::Mcp);
        assert_eq!(tool_cat("MultiEdit"), ToolCat::Edit);
        assert_eq!(tool_cat("TodoWrite"), ToolCat::Plan);
        assert_eq!(tool_cat("frobnicate"), ToolCat::Other);
    }
}
