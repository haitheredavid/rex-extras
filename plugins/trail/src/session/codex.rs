//! Codex rollout files (`~/.codex/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`): every line is
//! `{timestamp, type, payload}`. A request is a change in the running token total; tool calls are
//! `response_item` function or custom tool calls. A prompt is a `user_message` event in older
//! rollouts and an `item_completed` `UserMessage` in newer ones (a file has one or the other).
//! Codex records no dollars.

use serde::Deserialize;
use serde_json::value::RawValue;

use super::{
    Decoder, Event, EventKind, Fact, Outcome, Request, SourceRef, SpanKind, SpanStatus, Stop, Trigger, Usage, rfc3339_ms,
    tool_cat,
};

const WANTED: [&[u8]; 13] = [
    br#""type":"session_meta""#,
    br#""type":"turn_context""#,
    br#""type":"token_count""#,
    br#""type":"task_started""#,
    br#""type":"task_complete""#,
    br#""type":"user_message""#,
    br#""type":"UserMessage""#,
    br#""type":"turn_aborted""#,
    br#""type":"context_compacted""#,
    br#""type":"error""#,
    br#""type":"collab_agent_spawn_end""#,
    br#""type":"function_call"#,
    br#""type":"custom_tool_call"#,
];

#[derive(Default)]
pub struct Codex {
    /// Running token total at the last request; a token count that doesn't move it is a repeat.
    last_total: u64,
    /// When the current request started: the turn start or the previous request's end.
    last_ts: i64,
    model: Option<String>,
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "type")]
    ty: String,
    #[serde(borrow)]
    payload: &'a RawValue,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Payload {
    #[serde(rename = "type")]
    ty: String,
    id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    message: Option<String>,
    info: Option<Info>,
    call_id: Option<String>,
    name: Option<String>,
    new_thread_id: Option<String>,
    reason: Option<String>,
    item: Option<Item>,
}

/// An `item_completed` item; only `UserMessage` is read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Item {
    #[serde(rename = "type")]
    ty: String,
    content: Vec<TextPart>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TextPart {
    text: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Info {
    total_token_usage: Tokens,
    last_token_usage: Tokens,
    model_context_window: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Tokens {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl Codex {
    /// `task_started` lands a few ms before the prompt; the turn's first request starts at the prompt, so
    /// it sorts under its turn.
    fn prompt(&mut self, ts: i64, chars: u32, src: SourceRef, out: &mut Vec<Fact>) {
        self.last_ts = self.last_ts.max(ts);
        out.push(Fact::Prompt { at_ms: ts, trigger: Trigger::User, chars, src });
    }

    fn event(&mut self, p: Payload, ts: i64, src: SourceRef, out: &mut Vec<Fact>) {
        match p.ty.as_str() {
            "task_started" => self.last_ts = ts,
            "user_message" => {
                let chars = p.message.as_deref().map_or(0, |m| m.chars().count()) as u32;
                self.prompt(ts, chars, src, out);
            }
            "item_completed" => {
                let Some(item) = p.item.filter(|i| i.ty == "UserMessage") else { return };
                let chars = item.content.iter().filter_map(|t| t.text.as_deref()).map(|t| t.chars().count()).sum::<usize>();
                self.prompt(ts, chars as u32, src, out);
            }
            "token_count" => {
                let Some(info) = p.info else { return };
                let total = info.total_token_usage.total_tokens;
                if total == self.last_total {
                    return;
                }
                let last = &info.last_token_usage;
                out.push(Fact::Request(Request {
                    started_ms: self.last_ts,
                    ended_ms: ts,
                    model: self.model.clone().unwrap_or_default(),
                    usage: Usage {
                        input: last.input_tokens.saturating_sub(last.cached_input_tokens),
                        output: last.output_tokens,
                        cache_read: last.cached_input_tokens,
                        cache_write: last.cache_write_input_tokens,
                        reasoning: last.reasoning_output_tokens,
                        cost: None,
                    },
                    ctx_tokens: last.input_tokens,
                    ctx_window: info.model_context_window,
                    thinking_ms: None,
                    stop: Stop::Other,
                    side: None,
                }));
                self.last_total = total;
                self.last_ts = ts;
            }
            "task_complete" => out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Completed }),
            "turn_aborted" => {
                if p.reason.as_deref() == Some("interrupted") {
                    out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::Interrupt }));
                }
                out.push(Fact::TurnEnd { at_ms: ts, outcome: Outcome::Aborted });
            }
            "context_compacted" => {
                out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::Compaction { before: None, after: None } }));
            }
            "error" => out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::ApiError })),
            "collab_agent_spawn_end" => {
                if let (Some(span_id), Some(key)) = (p.call_id, p.new_thread_id) {
                    out.push(Fact::Child { span_id, key });
                }
            }
            _ => {}
        }
    }

    fn item(p: Payload, ts: i64, out: &mut Vec<Fact>) {
        let Some(id) = p.call_id else { return };
        match p.ty.as_str() {
            "function_call" | "custom_tool_call" => {
                let name = p.name.unwrap_or_default();
                let kind = match name.as_str() {
                    "spawn_agent" => SpanKind::Subagent,
                    "request_user_input" => SpanKind::WaitUser,
                    "wait_agent" => SpanKind::WaitAgents,
                    n => SpanKind::Tool(tool_cat(n)),
                };
                out.push(Fact::SpanStart { id, at_ms: ts, kind, name, intent: None });
            }
            "function_call_output" | "custom_tool_call_output" => {
                out.push(Fact::SpanEnd { id, at_ms: ts, status: SpanStatus::Ok, exit_code: None });
            }
            _ => {}
        }
    }
}

impl Decoder for Codex {
    fn wants(&self, line: &[u8]) -> bool {
        WANTED.iter().any(|w| line.windows(w.len()).any(|x| x == *w))
    }

    fn decode(&mut self, line: &[u8], src: SourceRef, out: &mut Vec<Fact>) {
        let Ok(l) = serde_json::from_slice::<Line>(line) else { return };
        let ts = l.timestamp.as_deref().and_then(rfc3339_ms).unwrap_or(self.last_ts);
        let p: Payload = serde_json::from_str(l.payload.get()).unwrap_or_default();
        match l.ty.as_str() {
            "session_meta" => out.push(Fact::Meta { id: p.id, cwd: p.cwd, at_ms: Some(ts) }),
            "turn_context" => {
                if let Some(m) = p.model.filter(|m| self.model.as_ref() != Some(m)) {
                    out.push(Fact::Point(Event { at_ms: ts, kind: EventKind::ModelChange(m.clone()) }));
                    self.model = Some(m);
                }
            }
            "event_msg" => self.event(p, ts, src, out),
            "response_item" => Codex::item(p, ts, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    fn decode_all(lines: &[&str]) -> Session {
        let mut s = Session::new(Source::Codex, "/x.jsonl".into());
        let mut d = Codex::default();
        for l in lines {
            let mut out = Vec::new();
            if d.wants(l.as_bytes()) {
                d.decode(l.as_bytes(), SourceRef { session: 0, offset: 0, len: l.len() as u32 }, &mut out);
            }
            out.into_iter().for_each(|f| s.apply(f));
        }
        s
    }

    const META: &str = r#"{"timestamp":"2026-11-12T03:44:36.844Z","type":"session_meta","payload":{"id":"01a1","cwd":"/repo"}}"#;
    const CONTEXT: &str = r#"{"timestamp":"2026-11-12T03:44:37.000Z","type":"turn_context","payload":{"model":"gpt-5.5-codex"}}"#;
    const STARTED: &str = r#"{"timestamp":"2026-11-12T03:44:38.000Z","type":"event_msg","payload":{"type":"task_started"}}"#;
    const PROMPT: &str =
        r#"{"timestamp":"2026-11-12T03:44:38.100Z","type":"event_msg","payload":{"type":"user_message","message":"list files"}}"#;
    const CALL: &str = r#"{"timestamp":"2026-11-12T03:44:40.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"call_1","input":"ls"}}"#;
    const OUTPUT: &str = r#"{"timestamp":"2026-11-12T03:44:41.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_1","output":"a b"}}"#;
    const TOKENS: &str = r#"{"timestamp":"2026-11-12T03:44:42.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":13205},"last_token_usage":{"input_tokens":13118,"cached_input_tokens":12000,"cache_write_input_tokens":0,"output_tokens":87,"reasoning_output_tokens":34,"total_tokens":13205},"model_context_window":258400}}}"#;
    const DONE: &str = r#"{"timestamp":"2026-11-12T03:44:43.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#;

    #[test]
    fn codex_token_counts_become_requests_once() {
        let s = decode_all(&[META, CONTEXT, STARTED, PROMPT, CALL, OUTPUT, TOKENS, TOKENS, DONE]);
        assert_eq!(s.requests.len(), 1, "a repeated total is the same request");
        let r = &s.requests[0];
        assert_eq!(r.ctx_tokens, 13118);
        assert_eq!((r.usage.input, r.usage.cache_read, r.usage.reasoning), (1118, 12000, 34));
        assert_eq!(r.ctx_window, Some(258400));
        assert_eq!(r.model, "gpt-5.5-codex");
        assert_eq!(r.started_ms, rfc3339_ms("2026-11-12T03:44:38.100Z").unwrap(), "from the prompt, not task_started");
        assert_eq!(s.spans.len(), 1);
        assert_eq!((s.spans[0].kind, s.spans[0].status), (SpanKind::Tool(ToolCat::Shell), SpanStatus::Ok));
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].outcome, Outcome::Completed);
        assert_eq!(s.id.as_deref(), Some("01a1"));
    }

    #[test]
    fn newer_rollouts_prompt_through_user_message_items() {
        const ITEM: &str = r#"{"timestamp":"2026-11-15T15:54:56.437Z","type":"event_msg","payload":{"type":"item_completed","thread_id":"t1","turn_id":"u1","item":{"type":"UserMessage","id":"m1","content":[{"type":"text","text":"Read the repo","text_elements":[]}]}}}"#;
        const AGENT: &str = r#"{"timestamp":"2026-11-15T15:54:59.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"m2"}}}"#;
        let s = decode_all(&[META, STARTED, ITEM, AGENT, TOKENS, DONE]);
        assert_eq!(s.turns.len(), 1);
        assert_eq!((s.turns[0].trigger, s.turns[0].prompt_chars), (Trigger::User, 13));
        assert!(s.turns[0].prompt.is_some());
    }
}
