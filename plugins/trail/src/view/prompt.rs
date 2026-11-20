//! One-line text input drawn over the body's last row: typing appends, Backspace deletes, Enter
//! accepts and Esc cancels. The timeline's tool jump asks for a tool name with it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::put_str;
use crate::theme::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `/`: filters the lanes as you type.
    Search,
    /// Timeline `; t`: the tool name `n` jumps to.
    JumpTool,
}

impl PromptKind {
    fn label(self) -> &'static str {
        match self {
            PromptKind::Search => "search",
            PromptKind::JumpTool => "jump to tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
}

pub(super) fn draw(buf: &mut Buffer, body: Rect, p: &Prompt) {
    let Some(y) = body.bottom().checked_sub(1) else { return };
    for x in body.x..body.right() {
        buf[(x, y)].reset();
    }
    put_str(buf, body, body.x, y, &format!("{}: {}▏", p.kind.label(), p.text), Style::new().fg(theme().accent));
}
