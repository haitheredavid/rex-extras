//! `rex-trail open split|popup --session S --block B`: opens the trail from an action, as a band
//! (a split off block B sized to `[band] rows` or `columns`) or as a centered floating layer. The
//! band then follows the active window around its session (`follow`).

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use crate::config::{Split, config};
use crate::rex::{Raw, call};

pub const USAGE: &str = "usage: rex-trail open split|popup --session SESSION --block BLOCK";

/// A terminal block running this binary with `args`.
fn trail_layout(args: &[&str]) -> anyhow::Result<Value> {
    let exe = std::env::current_exe().context("locate rex-trail")?;
    let mut command = vec![exe.to_string_lossy().to_string()];
    command.extend(args.iter().map(|a| a.to_string()));
    Ok(json!({ "block": {
        "flavor": "com.superlogical.terminal.shell",
        "label": "Trail",
        "options": { "command": command, "shell": "none" },
    }}))
}

/// Share of the split the anchor keeps so the band after it gets `band` of `total` cells.
fn band_ratio(band: u16, total: f64) -> f64 {
    (1.0 - f64::from(band) / total).clamp(0.2, 0.9)
}

/// The split direction and ratio for a band beside `anchor`, from `[band]` and the anchor's size.
pub fn band_geometry(session: &str, anchor: &str) -> anyhow::Result<(&'static str, f64)> {
    let c = config();
    let size = call("com.superlogical.terminal.size", json!({ "session_id": session, "block_id": anchor, "args": {} }))?;
    let (direction, band, key) = match c.band_split {
        Split::Down => ("vertical", c.band_rows, "rows"),
        Split::Right => ("horizontal", c.band_columns, "columns"),
    };
    let total = size[key].as_f64().filter(|t| *t > 0.0).ok_or_else(|| anyhow!("terminal size: no {key}"))?;
    Ok((direction, band_ratio(band, total)))
}

/// `80%` as `0.8`.
fn fraction(size: &str) -> f64 {
    size.trim_end_matches('%').parse::<f64>().map_or(0.8, |n| n / 100.0)
}

pub enum Target {
    Split,
    Popup,
}

/// `split|popup --session S --block B`.
fn parse(args: &[String]) -> Option<(Target, String, String)> {
    let target = match args.first()?.as_str() {
        "split" => Target::Split,
        "popup" => Target::Popup,
        _ => return None,
    };
    let (mut session, mut block) = (None, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--session" => session = it.next().cloned(),
            "--block" => block = it.next().cloned(),
            _ => return None,
        }
    }
    Some((target, session.filter(|s| !s.is_empty())?, block.filter(|b| !b.is_empty())?))
}

/// None for bad arguments (the caller prints `USAGE`), else the result of opening.
pub fn run(args: &[String]) -> Option<anyhow::Result<()>> {
    let (target, session, block) = parse(args)?;
    Some(match target {
        Target::Split => open_split(&session, &block),
        Target::Popup => open_popup(&session, &block),
    })
}

/// Splits the band off `block`. `new_split` ignores its `ratio`, so the band is then placed again
/// with `move_block`, which honors it; the ratio is from `block`'s size before the split.
fn open_split(session: &str, block: &str) -> anyhow::Result<()> {
    let (direction, ratio) = band_geometry(session, block)?;
    let opened = call(
        "session.new_split",
        json!({
            "session_id": session,
            "anchor_id": block,
            "direction": direction,
            "side": "after",
            "focus": true,
            "layout": trail_layout(&[])?,
        }),
    )?;
    let band = opened["block_ids"][0].as_str().ok_or_else(|| anyhow!("session.new_split: no block id in result"))?;
    place(session, band, block, direction, ratio)
}

fn open_popup(session: &str, block: &str) -> anyhow::Result<()> {
    let view = call("session.view", json!({ "session_id": session }))?;
    let has_block = |w: &&Value| {
        w["layers"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|l| l["blocks"].as_array().into_iter().flatten().any(|b| b["block_id"].as_str() == Some(block)))
    };
    let window = view["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(has_block)
        .and_then(|w| w["window_id"].as_str())
        .or(view["active_window_id"].as_str())
        .ok_or_else(|| anyhow!("session {session} has no window"))?;
    let (w, h) = (fraction(&config().popup_width), fraction(&config().popup_height));
    call(
        "session.new_layer",
        json!({
            "session_id": session,
            "window_id": window,
            "bounds": [(1.0 - w) / 2.0, (1.0 - h) / 2.0, w, h],
            "focus": true,
            "layout": trail_layout(&["--popup"])?,
        }),
    )
    .map(drop)
}

/// The block to place the band beside when `window` of `session` becomes active: that window's
/// focused block. None when the band is already there, or alone in its own window (moving it
/// would leave that window empty).
pub fn follow_anchor(raw: &Raw, session: &str, band: &str, window: &str) -> Option<String> {
    let s = raw.sessions.iter().find(|s| s.session_id == session)?;
    let target = s.windows.iter().find(|w| w.window_id == window)?;
    let has_band = |blocks: &[crate::rex::RawBlock]| blocks.iter().any(|b| b.block_id == band);
    if has_band(&target.blocks) {
        return None;
    }
    let home = s.windows.iter().find(|w| has_band(&w.blocks))?;
    if home.blocks.len() < 2 {
        return None;
    }
    target.focused_block_id.clone().or_else(|| target.blocks.first().map(|b| b.block_id.clone()))
}

/// Moves the band beside `anchor`, split as `[band]` says.
pub fn follow(session: &str, band: &str, anchor: &str) -> anyhow::Result<()> {
    let (direction, ratio) = band_geometry(session, anchor)?;
    place(session, band, anchor, direction, ratio)
}

fn place(session: &str, band: &str, anchor: &str, direction: &str, ratio: f64) -> anyhow::Result<()> {
    call(
        "session.move_block",
        json!({
            "session_id": session,
            "block_id": band,
            "anchor_block_id": anchor,
            "direction": direction,
            "side": "after",
            "ratio": ratio,
        }),
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw() -> Raw {
        serde_json::from_value(json!({"sessions": [{"session_id": "s", "windows": [
            {"window_id": "w1", "focused_block_id": "a", "blocks": [{"block_id": "a"}, {"block_id": "band"}]},
            {"window_id": "w2", "focused_block_id": "c", "blocks": [{"block_id": "b"}, {"block_id": "c"}]},
            {"window_id": "w3", "blocks": {}},
        ]}]}))
        .unwrap()
    }

    #[test]
    fn band_follows_into_another_window_beside_its_focused_block() {
        let r = raw();
        assert_eq!(follow_anchor(&r, "s", "band", "w2").as_deref(), Some("c"));
        assert_eq!(follow_anchor(&r, "s", "band", "w1"), None, "already there");
        assert_eq!(follow_anchor(&r, "s", "band", "w3"), None, "no block to sit beside");
        assert_eq!(follow_anchor(&r, "other", "band", "w2"), None, "another session");
    }

    #[test]
    fn band_alone_in_its_window_stays() {
        let r: Raw = serde_json::from_value(json!({"sessions": [{"session_id": "s", "windows": [
            {"window_id": "w1", "blocks": [{"block_id": "band"}]},
            {"window_id": "w2", "focused_block_id": "c", "blocks": [{"block_id": "c"}]},
        ]}]}))
        .unwrap();
        assert_eq!(follow_anchor(&r, "s", "band", "w2"), None);
    }

    #[test]
    fn ratio_leaves_the_band_its_cells() {
        assert!((band_ratio(14, 56.0) - 0.75).abs() < 1e-9);
        assert!((band_ratio(14, 15.0) - 0.2).abs() < 1e-9, "clamped when the anchor is short");
        assert!((band_ratio(4, 200.0) - 0.9).abs() < 1e-9, "clamped when the anchor is tall");
    }

    #[test]
    fn open_needs_a_target_session_and_block() {
        let args = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(matches!(parse(&args("split --session s --block b")), Some((Target::Split, s, b)) if s == "s" && b == "b"));
        assert!(matches!(parse(&args("popup --block b --session s")), Some((Target::Popup, ..))));
        assert!(parse(&args("split --session s")).is_none());
        assert!(parse(&args("band --session s --block b")).is_none());
        assert!(parse(&args("split --session s --block b --extra")).is_none());
    }
}
