//! Claude Code session files (`~/.claude/projects/<dir>/<id>.jsonl`): `api-request` starts,
//! `assistant` records (one per content block, repeating the message's usage), `user` prompts and
//! tool results, `system` compactions and errors, and titles. Claude Code records no dollars.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::value::RawValue;

use super::{
    Decoder, Event, EventKind, Fact, Outcome, Request, SourceRef, SpanKind, SpanStatus, Stop, Trigger, Usage, rfc3339_ms,
    tool_cat,
};

const WANTED: [&[u8]; 5] =
    [br#""type":"assistant""#, br#""type":"user""#, br#""type":"system""#, br#""type":"api-request""#, br#"-title""#];

#[derive(Default)]
pub struct Claude {
    /// `message.id` of the request emitted last, and its value: later blocks of the same message update it.
    current: Option<(String, Request)>,
    /// `api-request` id → when it was sent.
    api_starts: HashMap<String, i64>,
    /// Time of the last user or assistant line, the start of a request with no `api-request`.
    last_ts: i64,
}

#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct ApiRequest {
    id: String,
}

#[derive(Deserialize)]
struct Assistant<'a> {
    #[serde(default, rename = "requestRef")]
    request_ref: Option<String>,
    #[serde(default, rename = "thinkingDurationMs")]
    thinking_ms: Option<u32>,
    #[serde(borrow)]
    message: AssistantMsg<'a>,
}

#[derive(Deserialize)]
struct AssistantMsg<'a> {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: ClaudeUsage,
    #[serde(default, borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClaudeUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

#[derive(Deserialize)]
struct ToolUse {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct User<'a> {
    #[serde(default, rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, borrow)]
    message: Option<UserMsg<'a>>,
    #[serde(default, rename = "toolUseResult", borrow)]
    tool_use_result: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct UserMsg<'a> {
    #[serde(default, borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct UserBlock {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ToolUseResult {
    interrupted: bool,
}

#[derive(Deserialize)]
struct System {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default, rename = "compactMetadata")]
    compact: Option<Compact>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Compact {
    #[serde(rename = "preTokens")]
    pre: Option<u64>,
    #[serde(rename = "postTokens")]
    post: Option<u64>,
}

#[derive(Deserialize)]
struct Title {
    #[serde(default, rename = "aiTitle")]
    ai: Option<String>,
    #[serde(default, rename = "customTitle")]
    custom: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// Prompt text of a user message: string content, or its text blocks.
fn user_text(content: &RawValue) -> Option<String> {
    if let Ok(s) = serde_json::from_str::<String>(content.get()) {
        return Some(s);
    }
    let blocks: Vec<UserBlock> = serde_json::from_str(content.get()).ok()?;
    let texts: Vec<String> = blocks.into_iter().filter(|b| b.ty == "text").filter_map(|b| b.text).collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

impl Claude {
    fn assistant(&mut self, line: &[u8], ts: i64, out: &mut Vec<Fact>) {
        let Ok(a) = serde_json::from_slice::<Assistant>(line) else { return };
        let m = a.message;
        let u = &m.usage;
        let id = m.id.unwrap_or_default();
        let same = self.current.as_ref().filter(|(cur, _)| !id.is_empty() && *cur == id).map(|(_, r)| r);
        let started_ms = same
            .map(|r| r.started_ms)
            .unwrap_or_else(|| a.request_ref.as_ref().and_then(|r| self.api_starts.get(r)).copied().unwrap_or(self.last_ts));
        let req = Request {
            started_ms,
            ended_ms: ts,
            model: m.model.unwrap_or_default(),
            usage: Usage {
                input: u.input_tokens,
                output: u.output_tokens,
                cache_read: u.cache_read_input_tokens,
                cache_write: u.cache_creation_input_tokens,
                reasoning: 0,
                cost: None,
            },
            ctx_tokens: u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens,
            ctx_window: None,
            thinking_ms: a.thinking_ms.or(same.and_then(|r| r.thinking_ms)),
            stop: match m.stop_reason.as_deref() {
                Some("tool_use") => Stop::ToolUse,
                Some("end_turn") => Stop::EndTurn,
                _ => Stop::Other,
            },
            side: None,
        };
        // Usage repeats on every block of a message, so the request is replaced, never summed.
        out.push(if same.is_some() { Fact::RequestUpdate(req.clone()) } else { Fact::Request(req.clone()) });
        let end_turn = req.stop == Stop::EndTurn;
        self.current = Some((id, req));

        let blocks: Vec<ToolUse> = m.content.and_then(|c| serde_json::from_str(c.get()).ok()).unwrap_or_default();
        for b in blocks.into_iter().filter(|b| b.ty == "tool_use") {
            let (Some(id), Some(name)) = (b.id, b.name) else { continue };
            let kind = match name.as_str() {
                "Agent" | "Task" => SpanKind::Subagent,
                "AskUserQuestion" | "ExitPlanMode" => SpanKind::WaitUser,
                "TaskOutput" => SpanKind::WaitAgents,
                n => SpanKind::Tool(tool_cat(n)),
            };
            if kind == SpanKind::Subagent {
                out.push(Fact::Child { span_id: id.clone(), key: id.clone() });
            }
            out.push(Fact::SpanStart { id, at_ms: ts, kind, name, intent: None });
        }
        if end_turn {
            out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Completed });
        }
    }

    fn user(line: &[u8], ts: i64, src: SourceRef, out: &mut Vec<Fact>) {
        let Ok(u) = serde_json::from_slice::<User>(line) else { return };
        if u.session_id.is_some() || u.cwd.is_some() {
            out.push(Fact::Meta { id: u.session_id, cwd: u.cwd, at_ms: Some(ts) });
        }
        let Some(content) = u.message.and_then(|m| m.content) else { return };
        let blocks: Vec<UserBlock> = serde_json::from_str(content.get()).unwrap_or_default();
        let results: Vec<&UserBlock> = blocks.iter().filter(|b| b.ty == "tool_result").collect();
        if !results.is_empty() {
            let interrupted = u
                .tool_use_result
                .and_then(|r| serde_json::from_str::<ToolUseResult>(r.get()).ok())
                .is_some_and(|r| r.interrupted);
            for b in results {
                let Some(id) = b.tool_use_id.clone() else { continue };
                let status = if interrupted {
                    SpanStatus::Interrupted
                } else if b.is_error == Some(true) {
                    SpanStatus::Error
                } else {
                    SpanStatus::Ok
                };
                out.push(Fact::SpanEnd { id, at_ms: ts, status, exit_code: None });
            }
            return;
        }
        if u.is_meta == Some(true) {
            return;
        }
        // Text starting with `<` is a command or reminder Claude Code injected, not something typed;
        // `[Request interrupted by user…]` is Claude Code noting an Esc.
        let Some(text) = user_text(content).filter(|t| !t.trim_start().starts_with('<')) else { return };
        if text.starts_with("[Request interrupted by user") {
            out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::Interrupt }));
            out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Aborted });
            return;
        }
        out.push(Fact::Prompt { at_ms: ts, trigger: Trigger::User, chars: text.chars().count() as u32, src });
    }

    fn system(line: &[u8], ts: i64, out: &mut Vec<Fact>) {
        let Ok(s) = serde_json::from_slice::<System>(line) else { return };
        match s.subtype.as_deref() {
            Some("compact_boundary") => {
                let c = s.compact.unwrap_or_default();
                out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::Compaction { before: c.pre, after: c.post } }));
            }
            Some("api_error") => out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::ApiError })),
            Some("turn_duration") => out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Completed }),
            _ => {}
        }
    }
}

impl Decoder for Claude {
    fn wants(&self, line: &[u8]) -> bool {
        WANTED.iter().any(|w| line.windows(w.len()).any(|x| x == *w))
    }

    fn decode(&mut self, line: &[u8], src: SourceRef, out: &mut Vec<Fact>) {
        let Ok(head) = serde_json::from_slice::<Head>(line) else { return };
        let ts = head.timestamp.as_deref().and_then(rfc3339_ms).unwrap_or(self.last_ts);
        match head.ty.as_str() {
            "api-request" => {
                if let Ok(r) = serde_json::from_slice::<ApiRequest>(line) {
                    self.api_starts.insert(r.id, ts);
                }
            }
            "assistant" => {
                self.assistant(line, ts, out);
                self.last_ts = ts;
            }
            "user" => {
                Claude::user(line, ts, src, out);
                self.last_ts = ts;
            }
            "system" => Claude::system(line, ts, out),
            "ai-title" | "custom-title" => {
                if let Ok(t) = serde_json::from_slice::<Title>(line)
                    && let Some(label) = t.ai.or(t.custom).or(t.title).filter(|l| !l.is_empty())
                {
                    out.push(Fact::Label(label));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    fn decode_all(lines: &[&str]) -> Session {
        let mut s = Session::new(Source::Claude, "/c.jsonl".into());
        let mut d = Claude::default();
        for l in lines {
            let mut out = Vec::new();
            if d.wants(l.as_bytes()) {
                d.decode(l.as_bytes(), SourceRef { session: 0, offset: 0, len: l.len() as u32 }, &mut out);
            }
            out.into_iter().for_each(|f| s.apply(f));
        }
        s
    }

    const PROMPT: &str = r#"{"type":"user","timestamp":"2026-11-15T01:03:00.000Z","sessionId":"abc","cwd":"/repo","message":{"role":"user","content":"run the tests"}}"#;
    const API: &str = r#"{"type":"api-request","id":"req-1","timestamp":"2026-11-15T01:03:02.953Z"}"#;
    const THINK: &str = r#"{"type":"assistant","timestamp":"2026-11-15T01:03:07.623Z","requestRef":"req-1","thinkingDurationMs":1413,"message":{"id":"msg_1","model":"claude-opus-5-5","stop_reason":"tool_use","usage":{"input_tokens":2,"cache_read_input_tokens":18481,"cache_creation_input_tokens":83112,"output_tokens":282},"content":[{"type":"thinking","thinking":"…"}]}}"#;
    const CALL: &str = r#"{"type":"assistant","timestamp":"2026-11-15T01:03:08.127Z","requestRef":"req-1","message":{"id":"msg_1","model":"claude-opus-5-5","stop_reason":"tool_use","usage":{"input_tokens":2,"cache_read_input_tokens":18481,"cache_creation_input_tokens":83112,"output_tokens":282},"content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"cargo test"}}]}}"#;
    const RESULT: &str = r#"{"type":"user","timestamp":"2026-11-15T01:03:12.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_9","is_error":true,"content":"failed"}]},"toolUseResult":"Error: failed"}"#;
    const COMPACT: &str = r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-11-15T01:10:00.000Z","compactMetadata":{"trigger":"manual","preTokens":206807,"postTokens":18039}}"#;
    const TITLE: &str = r#"{"type":"ai-title","aiTitle":"Type checker identification","sessionId":"abc"}"#;

    #[test]
    fn claude_blocks_of_one_message_are_one_request() {
        let s = decode_all(&[PROMPT, API, THINK, CALL, RESULT, COMPACT, TITLE]);
        assert_eq!(s.requests.len(), 1, "usage repeats per block; it isn't summed");
        let r = &s.requests[0];
        assert_eq!(r.ctx_tokens, 2 + 18481 + 83112);
        assert_eq!(r.started_ms, rfc3339_ms("2026-11-15T01:03:02.953Z").unwrap(), "starts at its api-request");
        assert_eq!(r.ended_ms, rfc3339_ms("2026-11-15T01:03:08.127Z").unwrap());
        assert_eq!(r.thinking_ms, Some(1413));
        assert_eq!(r.usage.cost, None);
        assert_eq!(s.ctx.len(), 1);
        assert_eq!(s.spans.len(), 1);
        assert_eq!((s.spans[0].kind, s.spans[0].status), (SpanKind::Tool(ToolCat::Shell), SpanStatus::Error));
        assert_eq!(
            s.events,
            [Event {
                at_ms: rfc3339_ms("2026-11-15T01:10:00Z").unwrap(),
                kind: EventKind::Compaction { before: Some(206807), after: Some(18039) }
            }]
        );
        assert_eq!(s.label.as_deref(), Some("Type checker identification"));
        assert_eq!((s.id.as_deref(), s.turns.len(), s.turns[0].prompt_chars), (Some("abc"), 1, 13));
    }

    #[test]
    fn injected_text_is_not_a_prompt() {
        let s = decode_all(&[
            r#"{"type":"user","timestamp":"2026-11-15T01:03:00.000Z","message":{"content":"<command-name>/clear</command-name>"}}"#,
        ]);
        assert!(s.turns.is_empty());
        let s = decode_all(&[
            PROMPT,
            r#"{"type":"user","timestamp":"2026-11-15T01:03:09.000Z","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
        ]);
        assert_eq!(s.turns.len(), 1, "an interrupt isn't a prompt");
        assert_eq!(s.turns[0].outcome, Outcome::Aborted);
        assert_eq!(s.events.iter().filter(|e| e.kind == EventKind::Interrupt).count(), 1);
        assert!(!Claude::default().wants(br#"{"type":"api-request-blob","id":"x"}"#));
    }
}
