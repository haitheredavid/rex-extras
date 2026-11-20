//! `?` overlay: every key (the timeline's own while it is open), the glyph legend, and whatever
//! `theme.toml` / `config.toml` couldn't use.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget};

use crate::config::config_problems;
use crate::theme::{theme, theme_problems};

type Sections = &'static [(&'static str, &'static [(&'static str, &'static str)])];

/// Sections of (key, meaning).
const HELP: Sections = &[
    (
        "MOVE",
        &[
            ("j/k", "next/previous agent (arrows follow the screen)"),
            ("J/K", "next/previous group"),
            ("b", "next blocked agent"),
            ("h/l H/L", "back/forward in time, a quarter or a whole strip"),
            ("Home/End", "first/last agent; PgUp/PgDn a page"),
            ("f", "follow now"),
            ("0", "follow now and fit time to the history"),
        ],
    ),
    (
        "SHOW",
        &[
            ("Tab", "LANES ⇄ USAGE"),
            ("1-5", "time, sequence, area, heat, dash"),
            ("v", "lens: status, heat, ctx (USAGE: stacked or overlaid)"),
            ("t", "status lens on time or steps"),
            ("Space", "expand the agent into a usage band"),
            ("Z", "density: auto, compact, roomy"),
            ("i", "inspector on/off"),
            ("o", "agents as rows or columns"),
            ("+/-", "zoom time in/out"),
        ],
    ),
    (
        "FILTER",
        &[
            ("/", "search names, tasks, workspaces, folders"),
            ("a", "only active agents (blocked ones always show)"),
            ("c", "show closed agents"),
            ("w", "scope: all, this workspace, this tab"),
            ("s", "sort: stable, attention, recent, name"),
            ("g", "group: workspace, tab, none"),
            ("x", "fold the group (Space or Enter opens it)"),
        ],
    ),
    (
        "ACT",
        &[
            ("Enter", "jump to the agent's pane"),
            ("I", "step through the agent's session"),
            ("r", "refresh now"),
            ("?", "this help"),
            ("Esc", "collapse, then close a popup"),
            ("q", "quit"),
        ],
    ),
];

/// The timeline's keys.
const TIMELINE_HELP: Sections = &[
    (
        "MOVE",
        &[
            ("j/k J/K", "next/previous step / turn"),
            ("h/l", "previous/next stretch of the scrubber"),
            ("H/L", "first/last step"),
            ("n/N", "next/previous match"),
            ("; e b c p s t", "match errors, blocked, compactions, prompts, subagents, a tool"),
            ("f", "follow the live end"),
        ],
    ),
    (
        "SHOW",
        &[
            ("Space", "fold the turn, or the request's tools"),
            ("z", "fold every other turn"),
            ("t", "scrubber: gap, wall clock, one slot per step"),
            ("T", "text: none, intents, prompts"),
        ],
    ),
    (
        "ACT",
        &[
            ("Enter ]", "open a subagent (Enter elsewhere: jump to the pane)"),
            ("[ Bksp", "back out of a subagent, or to the previous session"),
            ("Esc", "back out; leave the timeline at the root"),
            ("?  q", "this help / quit"),
        ],
    ),
];

const LEGEND: [&str; 2] = [
    "◆ created  ◇ seen  ● working  ▲ blocked  ✓ done  ○ idle  · unknown  ✕ closed  ◉ several  ▸ now",
    "━ working/done  ╍ blocked  ─ idle  ┈ unknown",
];

const TIMELINE_LEGEND: [&str; 2] = [
    "▃ model  ▆ tool  ▲ waits on you  ⧖ waits on agents  · idle  │ turn  ⇣ compaction  ✕ error  ⑂ subagent",
    "▶ prompt  ↻ agent turn  ◆ request  $ shell  r read  / search  e edit  w write  @ web  ☰ plan  ƒ code  m mcp",
];

fn lines(timeline: bool) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme().dim);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let (sections, legend) = if timeline { (TIMELINE_HELP, TIMELINE_LEGEND) } else { (HELP, LEGEND) };
    let mut out = Vec::new();
    for (section, keys) in sections {
        out.push(Line::styled(*section, bold));
        for (key, what) in *keys {
            out.push(Line::from(vec![Span::raw(format!("  {key:<14}")), Span::styled(*what, dim)]));
        }
    }
    out.push(Line::raw(""));
    out.extend(legend.iter().map(|l| Line::styled(*l, dim)));
    let error = Style::new().fg(theme().error);
    for (file, problems) in [("theme.toml", theme_problems()), ("config.toml", config_problems())] {
        out.extend(problems.iter().map(|p| Line::styled(format!("{file} {p}"), error)));
    }
    out
}

/// Draws the overlay centered over `body`.
pub(super) fn draw(buf: &mut Buffer, body: Rect, timeline: bool) {
    let lines = lines(timeline);
    let w = body.width.min(100);
    let h = (lines.len() as u16 + 2).min(body.height);
    let area = Rect::new(body.x + (body.width - w) / 2, body.y + (body.height - h) / 2, w, h);
    Clear.render(area, buf);
    Paragraph::new(lines).block(Block::bordered().title(" keys ")).render(area, buf);
}
