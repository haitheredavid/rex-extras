//! Text on demand: a prompt or a tool's intent, read back from the session file at the byte range
//! a `SourceRef` names. Only the cursor's step and the visible turn headers ask; nothing is cached,
//! and thinking or tool output is never read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use serde_json::Value;

use super::{Session, Source, SourceRef};

/// How much text the timeline shows; `T` steps through these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TextLevel {
    None,
    Intent,
    Prompt,
}

const MAX_CHARS: usize = 200;

fn line_at(s: &Session, src: SourceRef) -> Option<Value> {
    let mut f = File::open(&s.path).ok()?;
    f.seek(SeekFrom::Start(src.offset)).ok()?;
    let mut bytes = vec![0; src.len as usize];
    f.read_exact(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}
/// First non-blank line, at most `MAX_CHARS`.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    if line.chars().count() <= MAX_CHARS {
        return Some(line.to_string());
    }
    Some(line.chars().take(MAX_CHARS - 1).chain(['…']).collect())
}

/// Joined `text` blocks of a content array, or the content itself when it's a string.
fn content_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let texts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

pub fn prompt_text(s: &Session, src: SourceRef) -> Option<String> {
    let v = line_at(s, src)?;
    let text = match s.source {
        Source::Omp | Source::Claude => content_text(v.pointer("/message/content")?)?,
        // Older rollouts: an `event_msg` `user_message`; newer: an `item_completed` `UserMessage`.
        Source::Codex => match v.pointer("/payload/message").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => content_text(v.pointer("/payload/item/content")?)?,
        },
    };
    first_line(&text)
}

pub fn intent_text(s: &Session, src: SourceRef) -> Option<String> {
    let v = line_at(s, src)?;
    first_line(v.pointer("/data/intent")?.as_str()?)
}

#[cfg(test)]
mod tests {
    use super::super::omp::tests::{TOOL_START, USER};
    use super::*;

    #[test]
    fn prompt_text_reads_the_line_at_its_offset() {
        let dir = std::env::temp_dir().join(format!("rex-trail-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let head = r#"{"type":"session","id":"x"}"#;
        std::fs::write(&path, format!("{head}\n{USER}\n{TOOL_START}\n")).unwrap();
        let s = Session::new(Source::Omp, path);
        let at = |offset: usize, line: &str| SourceRef { session: 0, offset: offset as u64, len: line.len() as u32 };
        assert_eq!(prompt_text(&s, at(head.len() + 1, USER)).as_deref(), Some("fix the login"));
        let intent = at(head.len() + 1 + USER.len() + 1, TOOL_START);
        assert_eq!(intent_text(&s, intent).as_deref(), Some("Dispatching a reviewer"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_prompt_text_reads_both_rollout_forms() {
        let dir = std::env::temp_dir().join(format!("rex-trail-codex-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.jsonl");
        let old = r#"{"type":"event_msg","payload":{"type":"user_message","message":"list files"}}"#;
        let new = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"Read the repo"}]}}}"#;
        std::fs::write(&path, format!("{old}\n{new}\n")).unwrap();
        let s = Session::new(Source::Codex, path);
        let at = |offset: usize, line: &str| SourceRef { session: 0, offset: offset as u64, len: line.len() as u32 };
        assert_eq!(prompt_text(&s, at(0, old)).as_deref(), Some("list files"));
        assert_eq!(prompt_text(&s, at(old.len() + 1, new)).as_deref(), Some("Read the repo"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
