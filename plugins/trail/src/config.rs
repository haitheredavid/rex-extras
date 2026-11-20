//! `config.toml` in the plugin config dir: behavior knobs, read once at start. Every key is
//! optional; a key it can't use is named in the header and keeps its default.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use toml::{Table, Value};

use crate::session::text::TextLevel;
use crate::view::preset_from_name;

/// History window by default: two hours.
const DEFAULT_HISTORY_MS: i64 = 7_200_000;

/// `$REX_TRAIL_CONFIG_DIR`, else `$XDG_CONFIG_HOME/rex-extras/trail`, else `~/.config/rex-extras/trail`.
pub fn config_dir() -> PathBuf {
    crate::history::xdg_dir("REX_TRAIL_CONFIG_DIR", "XDG_CONFIG_HOME", ".config")
}

/// Which way the band splits off its anchor block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    Down,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// `[start] view`: a `--view` name, already checked.
    pub start_view: Option<String>,
    /// `[start] orient`: `h` or `v`, already checked.
    pub start_orient: Option<String>,
    /// `[start] remember`: read and write `prefs.toml`.
    pub remember: bool,
    /// Mouse capture: click to select, double-click to jump.
    pub mouse: bool,
    /// `[history] window`: log load, lane pruning, cost and working windows, the fit cap.
    pub history_ms: i64,
    /// `[history] retain_months`: month files kept; 0 keeps all.
    pub retain_months: u32,
    pub band_split: Split,
    /// `[band] rows`: band height when it splits down.
    pub band_rows: u16,
    /// `[band] columns`: band width when it splits right.
    pub band_columns: u16,
    /// `[band] follow`: pin the band and move it into the focused tab.
    pub band_follow: bool,
    pub popup_width: String,
    pub popup_height: String,
    /// `[filter] quiet_after`: how long an idle, done or unknown agent stays in `active_only`.
    pub quiet_after_ms: i64,
    /// `[notify] blocked_after`: notify when an agent has been blocked this long; None is off.
    pub blocked_after_ms: Option<i64>,
    /// `[privacy] max_text`: the most the timeline's `T` reveals.
    pub max_text: TextLevel,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            start_view: None,
            start_orient: None,
            remember: true,
            mouse: false,
            history_ms: DEFAULT_HISTORY_MS,
            retain_months: 3,
            band_split: Split::Down,
            band_rows: 14,
            band_columns: 50,
            band_follow: true,
            popup_width: "80%".into(),
            popup_height: "80%".into(),
            quiet_after_ms: 30 * 60_000,
            blocked_after_ms: None,
            max_text: TextLevel::Prompt,
        }
    }
}

/// `90s`, `5m`, `2h` in ms.
pub fn parse_dur(s: &str) -> Option<i64> {
    let (n, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = n.parse().ok().filter(|n| *n >= 0)?;
    Some(
        n * match unit {
            "s" => 1000,
            "m" => 60_000,
            "h" => 3_600_000,
            _ => return None,
        },
    )
}

const TABLES: [&str; 7] = ["start", "history", "band", "popup", "filter", "notify", "privacy"];

impl Config {
    /// Applies the keys of a `config.toml`; returns one message per key it couldn't use.
    fn apply(&mut self, table: &Table) -> Vec<String> {
        let mut problems = Vec::new();
        for (key, value) in table {
            let result = match (key.as_str(), value.as_table()) {
                ("mouse", _) => value.as_bool().map(|b| self.mouse = b).ok_or_else(|| "expected true or false".to_string()),
                (t, Some(inner)) if TABLES.contains(&t) => {
                    for (name, v) in inner {
                        if let Err(e) = self.set(t, name, v) {
                            problems.push(format!("{key}.{name}: {e}"));
                        }
                    }
                    Ok(())
                }
                (t, None) if TABLES.contains(&t) => Err("expected a table".into()),
                _ => Err("unknown key".into()),
            };
            if let Err(e) = result {
                problems.push(format!("{key}: {e}"));
            }
        }
        problems
    }

    fn set(&mut self, table: &str, name: &str, value: &Value) -> Result<(), String> {
        let text = || value.as_str().ok_or_else(|| "expected a string".to_string());
        let flag = || value.as_bool().ok_or_else(|| "expected true or false".to_string());
        let dur = || text().and_then(|v| parse_dur(v).ok_or_else(|| format!("expected a duration like \"5m\", got {v:?}")));
        let cells = |lo: i64, hi: i64| {
            value
                .as_integer()
                .filter(|n| (lo..=hi).contains(n))
                .map(|n| n as u16)
                .ok_or_else(|| format!("expected a number from {lo} to {hi}"))
        };
        let size = || {
            text().and_then(|v| {
                v.strip_suffix('%')
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| (10..=100).contains(n))
                    .map(|_| v.to_string())
                    .ok_or_else(|| format!("expected a size like \"80%\", got {v:?}"))
            })
        };
        match (table, name) {
            ("start", "view") => {
                let v = text()?;
                preset_from_name(v).ok_or_else(|| format!("unknown view {v:?}"))?;
                self.start_view = Some(v.to_string());
            }
            ("start", "orient") => {
                let v = text()?;
                if !matches!(v, "h" | "v") {
                    return Err(format!("expected \"h\" or \"v\", got {v:?}"));
                }
                self.start_orient = Some(v.to_string());
            }
            ("start", "remember") => self.remember = flag()?,
            ("history", "window") => self.history_ms = dur()?.max(60_000),
            ("history", "retain_months") => self.retain_months = cells(0, 1200)?.into(),
            ("band", "split") => {
                self.band_split = match text()? {
                    "down" => Split::Down,
                    "right" => Split::Right,
                    v => return Err(format!("expected \"down\" or \"right\", got {v:?}")),
                }
            }
            ("band", "rows") => self.band_rows = cells(4, 200)?,
            ("band", "columns") => self.band_columns = cells(20, 400)?,
            ("band", "follow") => self.band_follow = flag()?,
            ("popup", "width") => self.popup_width = size()?,
            ("popup", "height") => self.popup_height = size()?,
            ("filter", "quiet_after") => self.quiet_after_ms = dur()?,
            ("notify", "blocked_after") => self.blocked_after_ms = Some(dur()?),
            ("privacy", "max_text") => {
                self.max_text = match text()? {
                    "none" => TextLevel::None,
                    "intent" => TextLevel::Intent,
                    "prompt" => TextLevel::Prompt,
                    v => return Err(format!("expected none, intent or prompt, got {v:?}")),
                }
            }
            _ => return Err("unknown key".into()),
        }
        Ok(())
    }
}

/// Defaults with `path` applied; a missing file is not a problem.
pub fn load(path: &Path) -> (Config, Vec<String>) {
    let mut config = Config::default();
    let Ok(text) = std::fs::read_to_string(path) else { return (config, Vec::new()) };
    match text.parse::<Table>() {
        Ok(table) => {
            let problems = config.apply(&table);
            (config, problems)
        }
        Err(e) => (config, vec![format!("{}: {}", path.display(), e.message())]),
    }
}

/// The process config and any problems loading it. Tests always get the defaults.
static CONFIG: LazyLock<(Config, Vec<String>)> =
    LazyLock::new(|| if cfg!(test) { (Config::default(), Vec::new()) } else { load(&config_dir().join("config.toml")) });

pub fn config() -> &'static Config {
    &CONFIG.0
}

/// The history window: what the log loads, how long closed lanes stay, and the cost window.
pub fn history_ms() -> i64 {
    config().history_ms
}

/// Keys of `config.toml` that couldn't be used, for the header.
pub fn config_problems() -> &'static [String] {
    &CONFIG.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_checked_and_bad_ones_keep_defaults() {
        let table: Table = r#"
            bogus = 1
            mouse = true
            [start]
            view = "heat"
            orient = "sideways"
            remember = false
            [history]
            window = "6h"
            retain_months = 0
            [band]
            split = "right"
            columns = 60
            rows = 2
            [popup]
            width = "90%"
            height = "huge"
            [filter]
            quiet_after = "10m"
            [notify]
            blocked_after = "soon"
            [privacy]
            max_text = "intent"
        "#
        .parse()
        .unwrap();
        let mut c = Config::default();
        let mut problems = c.apply(&table);
        problems.sort();
        assert_eq!(c.start_view.as_deref(), Some("heat"));
        assert_eq!(c.start_orient, None, "invalid value keeps the default");
        assert!(c.mouse && !c.remember);
        assert_eq!((c.history_ms, c.retain_months), (6 * 3_600_000, 0));
        assert_eq!((c.band_split, c.band_columns, c.band_rows), (Split::Right, 60, 14));
        assert_eq!((c.popup_width.as_str(), c.popup_height.as_str()), ("90%", "80%"));
        assert_eq!(c.quiet_after_ms, 600_000);
        assert_eq!(c.blocked_after_ms, None);
        assert_eq!(c.max_text, TextLevel::Intent);
        let names: Vec<&str> = problems.iter().map(|p| p.split(':').next().unwrap()).collect();
        assert_eq!(names, ["band.rows", "bogus", "notify.blocked_after", "popup.height", "start.orient"], "{problems:?}");
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_dur("90s"), Some(90_000));
        assert_eq!(parse_dur("5m"), Some(300_000));
        assert_eq!(parse_dur("2h"), Some(7_200_000));
        assert_eq!(parse_dur("5"), None);
        assert_eq!(parse_dur("m"), None);
        assert_eq!(parse_dur(""), None);
    }
}
