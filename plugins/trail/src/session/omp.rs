//! omp session files: `message` records (user prompts, assistant requests, tool results),
//! `custom` tool starts and exits, compactions, side-call `model_usage`, titles and mode changes.
//! Tool output is most of the bytes and is never parsed past its id.

use std::borrow::Cow;

use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

use super::{
    Decoder, Event, EventKind, Fact, Outcome, Request, SourceRef, SpanKind, SpanStatus, Stop, Trigger, Usage, rfc3339_ms,
    tool_cat,
};

const WANTED: [&[u8]; 10] = [
    br#""type":"message""#,
    br#""tool_execution_start""#,
    br#""type":"compaction""#,
    br#""type":"model_usage""#,
    br#""type":"session""#,
    br#""type":"title"#,
    br#""type":"model_change""#,
    br#""type":"mode_change""#,
    br#""session_exit""#,
    br#""type":"branch_summary""#,
];

pub struct Omp;

#[derive(Deserialize)]
struct Rec<'a> {
    #[serde(rename = "type", borrow)]
    ty: Cow<'a, str>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, borrow)]
    message: Option<Msg<'a>>,
    #[serde(default, rename = "customType")]
    custom_type: Option<String>,
    #[serde(default, borrow)]
    data: Option<&'a RawValue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Msg<'a> {
    role: String,
    timestamp: Option<f64>,
    #[serde(rename = "completedAt")]
    completed_at: Option<f64>,
    model: Option<String>,
    #[serde(rename = "stopReason")]
    stop_reason: Option<String>,
    usage: Option<OmpUsage>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(rename = "toolCallId")]
    tool_call_id: Option<String>,
    #[serde(rename = "isError")]
    is_error: bool,
    attribution: Option<String>,
    #[serde(borrow)]
    details: Option<&'a RawValue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Details {
    #[serde(rename = "exitCode")]
    exit_code: Option<i32>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct OmpUsage {
    input: u64,
    output: u64,
    #[serde(rename = "cacheRead")]
    cache_read: u64,
    #[serde(rename = "cacheWrite")]
    cache_write: u64,
    cost: Option<Cost>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Cost {
    total: f64,
}

impl OmpUsage {
    fn usage(&self) -> Usage {
        Usage {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            reasoning: 0,
            cost: Some(self.cost.as_ref().map_or(0.0, |c| c.total)),
        }
    }

    fn ctx(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

#[derive(Deserialize)]
struct TextBlock {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct CallBlock<'a> {
    #[serde(rename = "type")]
    ty: Cow<'a, str>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, borrow)]
    arguments: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct TaskArgs {
    #[serde(default)]
    tasks: Vec<TaskItem>,
}

#[derive(Deserialize)]
struct TaskItem {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct ToolStart {
    #[serde(rename = "toolCallId")]
    tool_call_id: String,
    #[serde(rename = "toolName", default)]
    tool_name: String,
    #[serde(rename = "startedAt", default)]
    started_at: Option<String>,
    #[serde(default)]
    intent: Option<String>,
}

#[derive(Deserialize)]
struct Exit {
    #[serde(default)]
    reason: String,
}

fn stop(reason: Option<&str>) -> Stop {
    match reason {
        Some("toolUse") => Stop::ToolUse,
        Some("stop") => Stop::EndTurn,
        Some("aborted") => Stop::Aborted,
        Some("error") => Stop::Error,
        _ => Stop::Other,
    }
}

fn u64_at(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

impl Omp {
    fn message(m: Msg, ts: i64, src: SourceRef, out: &mut Vec<Fact>) {
        match m.role.as_str() {
            "user" => {
                let blocks: Vec<TextBlock> = m.content.and_then(|c| serde_json::from_str(c.get()).ok()).unwrap_or_default();
                let chars = blocks
                    .iter()
                    .filter(|b| b.ty == "text")
                    .filter_map(|b| b.text.as_ref())
                    .map(|t| t.chars().count())
                    .sum::<usize>();
                let trigger = if m.attribution.as_deref() == Some("agent") { Trigger::Agent } else { Trigger::User };
                out.push(Fact::Prompt { at_ms: ts, trigger, chars: chars as u32, src });
            }
            "assistant" => {
                let usage = m.usage.unwrap_or_default();
                let stop = stop(m.stop_reason.as_deref());
                out.push(Fact::Request(Request {
                    started_ms: m.timestamp.map_or(ts, |t| t as i64),
                    ended_ms: m.completed_at.map_or(ts, |t| t as i64),
                    model: m.model.unwrap_or_default(),
                    usage: usage.usage(),
                    ctx_tokens: usage.ctx(),
                    ctx_window: None,
                    thinking_ms: None,
                    stop,
                    side: None,
                }));
                let blocks: Vec<CallBlock> = m.content.and_then(|c| serde_json::from_str(c.get()).ok()).unwrap_or_default();
                for b in blocks.iter().filter(|b| b.ty == "toolCall" && b.name.as_deref() == Some("task")) {
                    let (Some(id), Some(args)) = (&b.id, b.arguments) else { continue };
                    let Ok(args) = serde_json::from_str::<TaskArgs>(args.get()) else { continue };
                    for t in args.tasks {
                        out.push(Fact::Child { span_id: id.clone(), key: t.name });
                    }
                }
                match stop {
                    Stop::EndTurn => out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Completed }),
                    Stop::Aborted => out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Aborted }),
                    Stop::Error => {
                        out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::ApiError }));
                        out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Error });
                    }
                    Stop::ToolUse | Stop::Other => {}
                }
            }
            "toolResult" => {
                let Some(id) = m.tool_call_id else { return };
                let details: Details = m.details.and_then(|d| serde_json::from_str(d.get()).ok()).unwrap_or_default();
                let status = if m.is_error { SpanStatus::Error } else { SpanStatus::Ok };
                out.push(Fact::SpanEnd { id, at_ms: ts, status, exit_code: details.exit_code });
            }
            _ => {}
        }
    }

    fn custom(kind: &str, data: &RawValue, ts: i64, src: SourceRef, out: &mut Vec<Fact>) {
        match kind {
            "tool_execution_start" => {
                let Ok(d) = serde_json::from_str::<ToolStart>(data.get()) else { return };
                let kind = match d.tool_name.as_str() {
                    "task" => SpanKind::Subagent,
                    "ask" => SpanKind::WaitUser,
                    "wait" => SpanKind::WaitAgents,
                    name => SpanKind::Tool(tool_cat(name)),
                };
                out.push(Fact::SpanStart {
                    id: d.tool_call_id,
                    at_ms: d.started_at.as_deref().and_then(rfc3339_ms).unwrap_or(ts),
                    kind,
                    name: d.tool_name,
                    intent: d.intent.filter(|i| !i.is_empty()).map(|_| src),
                });
            }
            "session_exit" => {
                let reason = serde_json::from_str::<Exit>(data.get()).map(|e| e.reason).unwrap_or_default();
                out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::Exit(reason) }));
            }
            _ => {}
        }
    }

    /// The small record types, parsed whole.
    fn other(ty: &str, line: &[u8], ts: Option<i64>, out: &mut Vec<Fact>) {
        let Ok(v) = serde_json::from_slice::<Value>(line) else { return };
        let at = ts.unwrap_or(0);
        match ty {
            "session" => out.push(Fact::Meta { id: str_at(&v, "id"), cwd: str_at(&v, "cwd"), at_ms: ts }),
            "title" | "title_change" => {
                if let Some(t) = str_at(&v, "title").filter(|t| !t.is_empty()) {
                    out.push(Fact::Label(t));
                }
            }
            "compaction" => out.push(Fact::Point(Event {
                at_ms: at,
                kind: EventKind::Compaction { before: u64_at(&v, "tokensBefore"), after: u64_at(&v, "tokensAfter") },
            })),
            "model_usage" => {
                let usage: OmpUsage = v.get("usage").cloned().and_then(|u| serde_json::from_value(u).ok()).unwrap_or_default();
                out.push(Fact::Request(Request {
                    started_ms: at,
                    ended_ms: at,
                    model: str_at(&v, "model").unwrap_or_default(),
                    usage: usage.usage(),
                    ctx_tokens: 0,
                    ctx_window: None,
                    thinking_ms: None,
                    stop: Stop::Other,
                    side: Some(str_at(&v, "purpose").unwrap_or_default()),
                }));
            }
            "model_change" => {
                out.push(Fact::Point(Event { at_ms: at, kind: EventKind::ModelChange(str_at(&v, "model").unwrap_or_default()) }))
            }
            "mode_change" => {
                out.push(Fact::Point(Event { at_ms: at, kind: EventKind::ModeChange(str_at(&v, "mode").unwrap_or_default()) }))
            }
            "branch_summary" => out.push(Fact::Point(Event { at_ms: at, kind: EventKind::Branch })),
            _ => {}
        }
    }
}

impl Decoder for Omp {
    fn wants(&self, line: &[u8]) -> bool {
        WANTED.iter().any(|w| line.windows(w.len()).any(|x| x == *w))
    }

    fn decode(&mut self, line: &[u8], src: SourceRef, out: &mut Vec<Fact>) {
        let Ok(rec) = serde_json::from_slice::<Rec>(line) else { return };
        let ts = rec.timestamp.as_deref().and_then(rfc3339_ms);
        match rec.ty.as_ref() {
            "message" => {
                if let Some(m) = rec.message {
                    Omp::message(m, ts.unwrap_or(0), src, out);
                }
            }
            "custom" => {
                if let (Some(kind), Some(data)) = (rec.custom_type.as_deref(), rec.data) {
                    Omp::custom(kind, data, ts.unwrap_or(0), src, out);
                }
            }
            ty => Omp::other(ty, line, ts, out),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::*;
    use super::*;

    pub const USER: &str = r#"{"type":"message","timestamp":"2026-11-14T23:14:00.000Z","message":{"role":"user","attribution":"user","content":[{"type":"text","text":"fix the login"}]}}"#;
    pub const ASSISTANT: &str = r#"{"type":"message","timestamp":"2026-11-14T23:14:05.400Z","message":{"role":"assistant","timestamp":1794587642195,"completedAt":1794587645372,"model":"claude-opus-5-5","stopReason":"toolUse","usage":{"input":2,"output":253,"cacheRead":0,"cacheWrite":25581,"cost":{"total":0.132973}},"content":[{"type":"thinking","thinking":"…"},{"type":"toolCall","id":"toolu_1","name":"task","arguments":{"tasks":[{"name":"Child1","task":"go"}]}}]}}"#;
    pub const TOOL_START: &str = r#"{"type":"custom","customType":"tool_execution_start","data":{"toolCallId":"toolu_1","toolName":"task","startedAt":"2026-11-14T23:14:23.244Z","intent":"Dispatching a reviewer"},"timestamp":"2026-11-14T23:14:23.244Z"}"#;
    pub const TOOL_RESULT: &str = r#"{"type":"message","timestamp":"2026-11-14T23:15:00.000Z","message":{"role":"toolResult","toolCallId":"toolu_1","isError":false,"details":{"exitCode":0},"content":[{"type":"text","text":"done"}]}}"#;
    pub const COMPACTION: &str =
        r#"{"type":"compaction","timestamp":"2026-11-14T23:16:00.000Z","tokensBefore":75234,"tokensAfter":39180,"summary":"…"}"#;
    pub const SIDE: &str = r#"{"type":"model_usage","timestamp":"2026-11-14T23:16:30.000Z","purpose":"title","model":"claude-haiku-5-5","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"cost":{"total":0.0001}}}"#;

    pub fn decode_all(lines: &[&str]) -> Session {
        let mut s = Session::new(Source::Omp, "/s.jsonl".into());
        let mut d = Omp;
        let mut offset = 0;
        for l in lines {
            let mut out = Vec::new();
            if d.wants(l.as_bytes()) {
                d.decode(l.as_bytes(), SourceRef { session: 0, offset, len: l.len() as u32 }, &mut out);
            }
            out.into_iter().for_each(|f| s.apply(f));
            offset += l.len() as u64 + 1;
        }
        s
    }

    #[test]
    fn omp_lines_build_requests_spans_children_and_events() {
        let s = decode_all(&[USER, ASSISTANT, TOOL_START, TOOL_RESULT, COMPACTION, SIDE]);
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].trigger, Trigger::User);
        assert_eq!(s.turns[0].prompt_chars, 13);
        let main: Vec<_> = s.requests.iter().filter(|r| r.side.is_none()).collect();
        assert_eq!(main.len(), 1);
        assert_eq!(main[0].ctx_tokens, 25583);
        assert_eq!((main[0].started_ms, main[0].ended_ms), (1794587642195, 1794587645372));
        assert_eq!(main[0].usage.cost, Some(0.132973));
        assert_eq!(s.requests.iter().filter(|r| r.side.as_deref() == Some("title")).count(), 1);
        assert_eq!(s.spans.len(), 1);
        assert_eq!(s.spans[0].kind, SpanKind::Subagent);
        assert_eq!(s.spans[0].status, SpanStatus::Ok);
        assert_eq!(s.spans[0].exit_code, Some(0));
        assert!(s.spans[0].intent.is_some());
        assert_eq!(s.child_keys.get("Child1").map(String::as_str), Some("toolu_1"));
        assert_eq!(
            s.events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::Compaction { before: Some(75234), after: Some(39180) }))
                .count(),
            1
        );
        assert_eq!(s.ctx.len(), 1, "the side call adds no context sample");
    }

    #[test]
    fn prefilter_skips_tool_output_and_unknown_records() {
        assert!(!Omp.wants(br#"{"type":"custom","customType":"system-prompt-digest","data":{}}"#));
        assert!(Omp.wants(TOOL_RESULT.as_bytes()));
        let s = decode_all(&[r#"{"type":"title","title":"rex-trail — rename","pad":"   "}"#]);
        assert_eq!(s.label.as_deref(), Some("rex-trail — rename"));
    }
}
