//! Color tokens: every color the views draw with, defined once. The defaults below are the
//! built-in look; `theme.toml` in the config dir (`$REX_TRAIL_CONFIG_DIR`) overrides any
//! of them. Colors are ratatui color strings: a name (`green`, `darkgray`), `#rrggbb`, or a
//! 0-255 palette index. The `series` ramp and `background` must be `#rrggbb`, since fills are mixed
//! from them.

use std::path::Path;
use std::str::FromStr;
use std::sync::LazyLock;

use ratatui::style::Color;
use toml::{Table, Value};

use crate::model::AgentStatus;

/// Viridis, as 11 evenly spaced stops.
const VIRIDIS: [&str; 11] =
    ["#440154", "#482475", "#414487", "#355f8d", "#2a788e", "#21918c", "#22a884", "#44bf70", "#7ad151", "#bddf26", "#fde725"];

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Assumed terminal background; fills are mixed toward it.
    pub background: Color,
    /// Captions, axis labels, closed lanes, help.
    pub dim: Color,
    /// Gridlines behind charts.
    pub grid: Color,
    /// The view name in the header.
    pub accent: Color,
    /// Socket and theme errors in the header.
    pub error: Color,
    pub working: Color,
    pub blocked: Color,
    pub done: Color,
    pub idle: Color,
    pub unknown: Color,
    pub created: Color,
    pub seen: Color,
    pub closed: Color,
    /// The one color ramp: agents are spread evenly along it from `series_from` to its end, and
    /// HEAT shades along the same stretch.
    pub series: Vec<(u8, u8, u8)>,
    /// Where along `series` the used stretch starts; skips ramp starts too dark for the background.
    pub series_from: f64,
    /// HEAT cells where the agent was blocked.
    pub heat_blocked: Color,
    /// How much of a series' color its fill lays over what is behind (0-1).
    pub fill_alpha: f64,
}

impl Default for Theme {
    fn default() -> Self {
        let viridis: Vec<(u8, u8, u8)> = VIRIDIS.iter().filter_map(|h| rgb(&parse(h).ok()?)).collect();
        Theme {
            background: Color::Rgb(28, 28, 30),
            dim: Color::DarkGray,
            grid: Color::Rgb(72, 72, 78),
            accent: Color::Cyan,
            error: Color::Red,
            working: Color::Green,
            blocked: Color::Red,
            done: Color::Blue,
            idle: Color::DarkGray,
            unknown: Color::DarkGray,
            created: Color::Cyan,
            seen: Color::Cyan,
            closed: Color::Gray,
            series: viridis,
            series_from: 0.40,
            heat_blocked: Color::Red,
            fill_alpha: 0.3,
        }
    }
}

impl Theme {
    /// The light preset: a light background with darker greys, and the whole ramp, whose dark end
    /// reads on light.
    fn light() -> Theme {
        Theme {
            background: Color::Rgb(0xf5, 0xf5, 0xf7),
            dim: Color::Rgb(0x6e, 0x6e, 0x73),
            grid: Color::Rgb(0xd2, 0xd2, 0xd7),
            accent: Color::Blue,
            series_from: 0.0,
            ..Theme::default()
        }
    }
}

/// Whether a `COLORFGBG` value (`fg;bg`, sometimes with a middle field) names a light background.
fn light_background(colorfgbg: Option<&str>) -> bool {
    colorfgbg.and_then(|v| v.rsplit(';').next()).is_some_and(|bg| matches!(bg.trim(), "7" | "15"))
}

fn parse(s: &str) -> Result<Color, String> {
    Color::from_str(s).map_err(|_| format!("{s:?} is not a color"))
}

fn rgb(c: &Color) -> Option<(u8, u8, u8)> {
    match *c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// `ramp` at `t` in 0..=1, interpolating between evenly spaced stops.
pub fn ramp(stops: &[(u8, u8, u8)], t: f64) -> Color {
    let Some(&first) = stops.first() else { return Color::Reset };
    if stops.len() == 1 {
        return Color::Rgb(first.0, first.1, first.2);
    }
    let x = t.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
    let i = (x.floor() as usize).min(stops.len() - 2);
    let f = x - i as f64;
    let (a, b) = (stops[i], stops[i + 1]);
    let lerp = |p: u8, q: u8| (f64::from(p) + (f64::from(q) - f64::from(p)) * f).round() as u8;
    Color::Rgb(lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

/// `over` laid on `under` at `alpha`; non-RGB colors can't be mixed and return `over`.
pub fn mix(under: Color, over: Color, alpha: f64) -> Color {
    match (rgb(&under), rgb(&over)) {
        (Some(u), Some(o)) => {
            let m = |u: u8, o: u8| (f64::from(u) * (1.0 - alpha) + f64::from(o) * alpha).round() as u8;
            Color::Rgb(m(u.0, o.0), m(u.1, o.1), m(u.2, o.2))
        }
        _ => over,
    }
}

/// sRGB (D65) to CIELAB.
fn lab((r, g, b): (u8, u8, u8)) -> (f64, f64, f64) {
    let lin = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// CIE76 color difference.
fn delta_e(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (p, q) = (lab(a), lab(b));
    ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2) + (p.2 - q.2).powi(2)).sqrt()
}

impl Theme {
    pub fn status(&self, s: AgentStatus) -> Color {
        match s {
            AgentStatus::Working => self.working,
            AgentStatus::Blocked => self.blocked,
            AgentStatus::Done => self.done,
            AgentStatus::Idle => self.idle,
            AgentStatus::Unknown => self.unknown,
        }
    }

    /// The used stretch of `series` at `t` in 0..=1.
    fn along(&self, t: f64) -> Color {
        ramp(&self.series, self.series_from + (1.0 - self.series_from) * t.clamp(0.0, 1.0))
    }

    /// Agent color for palette slot `slot`: golden-ratio steps along the used ramp stretch, so a slot keeps its
    /// color however many agents come and go, and neighbors differ.
    pub fn agent_color(&self, slot: usize) -> Color {
        self.along((0.5 + slot as f64 * 0.618_033_988_749_895).fract())
    }

    /// HEAT's color for a working share, 0% to 100%, along the same stretch as agent colors.
    pub fn heat_color(&self, share: f64) -> Color {
        self.along(share)
    }

    /// Status hues too close to the ramp to tell an agent's line from a status, as messages.
    /// Named ANSI colors render per terminal and aren't checked.
    pub fn collisions(&self) -> Vec<String> {
        let checked = [
            ("status.working", self.working),
            ("status.blocked", self.blocked),
            ("status.done", self.done),
            ("heat_blocked", self.heat_blocked),
        ];
        let ramp: Vec<(u8, u8, u8)> = (0..=32).filter_map(|i| rgb(&self.along(f64::from(i) / 32.0))).collect();
        checked
            .iter()
            .filter_map(|&(name, c)| {
                let c = rgb(&c)?;
                let d = ramp.iter().map(|&r| delta_e(c, r)).fold(f64::INFINITY, f64::min);
                (d < 20.0).then(|| {
                    format!("{name} #{:02x}{:02x}{:02x} is hard to tell apart from the series ramp (ΔE {d:.0})", c.0, c.1, c.2)
                })
            })
            .collect()
    }

    /// Applies the keys of a `theme.toml`; returns one message per key it couldn't use.
    /// `appearance` goes first: it picks the base the other keys adjust.
    pub fn apply(&mut self, table: &Table) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(v) = table.get("appearance") {
            let light = match v.as_str() {
                Some("dark") => Some(false),
                Some("light") => Some(true),
                Some("auto") => Some(light_background(std::env::var("COLORFGBG").ok().as_deref())),
                _ => None,
            };
            match light {
                Some(true) => *self = Theme::light(),
                Some(false) => *self = Theme::default(),
                None => problems.push("appearance: expected \"dark\", \"light\" or \"auto\"".to_string()),
            }
        }
        for (key, value) in table {
            match (key.as_str(), value.as_table()) {
                // Each nested key stands alone, so one bad entry doesn't drop its siblings.
                ("status" | "node", Some(inner)) => {
                    for (name, v) in inner {
                        if let Err(e) = self.set_nested(key, name, v) {
                            problems.push(format!("{key}.{name}: {e}"));
                        }
                    }
                }
                _ => {
                    if let Err(e) = self.set(key, value) {
                        problems.push(format!("{key}: {e}"));
                    }
                }
            }
        }
        problems
    }

    fn set(&mut self, key: &str, value: &Value) -> Result<(), String> {
        let color = || value.as_str().ok_or("expected a color string".to_string()).and_then(parse);
        let ramp = || -> Result<Vec<(u8, u8, u8)>, String> {
            let stops = value.as_array().ok_or("expected a list of #rrggbb")?;
            let stops = stops
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or("expected #rrggbb".to_string())
                        .and_then(parse)
                        .and_then(|c| rgb(&c).ok_or(format!("{c} is not #rrggbb")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if stops.is_empty() { Err("needs at least one stop".into()) } else { Ok(stops) }
        };
        let fraction = || {
            value
                .as_float()
                .or(value.as_integer().map(|i| i as f64))
                .filter(|f| (0.0..=1.0).contains(f))
                .ok_or("expected a number from 0 to 1".to_string())
        };
        match key {
            "background" => {
                let c = color()?;
                rgb(&c).ok_or("background must be #rrggbb")?;
                self.background = c;
            }
            "dim" => self.dim = color()?,
            "grid" => self.grid = color()?,
            "accent" => self.accent = color()?,
            "error" => self.error = color()?,
            "heat_blocked" => self.heat_blocked = color()?,
            "series" => self.series = ramp()?,
            "series_from" => self.series_from = fraction()?,
            "fill_alpha" => self.fill_alpha = fraction()?,
            "status" | "node" => return Err("expected a table".into()),
            // Applied first, by `apply`.
            "appearance" => {}
            _ => return Err("unknown key".into()),
        }
        Ok(())
    }

    /// `[status]` and `[node]` entries.
    fn set_nested(&mut self, table: &str, name: &str, value: &Value) -> Result<(), String> {
        let slot = match (table, name) {
            ("status", "working") => &mut self.working,
            ("status", "blocked") => &mut self.blocked,
            ("status", "done") => &mut self.done,
            ("status", "idle") => &mut self.idle,
            ("status", "unknown") => &mut self.unknown,
            ("node", "created") => &mut self.created,
            ("node", "seen") => &mut self.seen,
            ("node", "closed") => &mut self.closed,
            _ => return Err("unknown key".into()),
        };
        *slot = value.as_str().ok_or("expected a color string".to_string()).and_then(parse)?;
        Ok(())
    }
}

/// `theme.toml` in the plugin config dir.
pub fn theme_path() -> std::path::PathBuf {
    crate::config::config_dir().join("theme.toml")
}

/// The default theme with `path` applied; a missing file is not a problem.
pub fn load(path: &Path) -> (Theme, Vec<String>) {
    let mut theme = Theme::default();
    let Ok(text) = std::fs::read_to_string(path) else { return (theme, Vec::new()) };
    match text.parse::<Table>() {
        Ok(table) => {
            let mut problems = theme.apply(&table);
            problems.extend(theme.collisions());
            (theme, problems)
        }
        Err(e) => (theme, vec![format!("{}: {}", path.display(), e.message())]),
    }
}

/// The process theme and any problems loading it. Tests always get the defaults, whatever
/// `theme.toml` is on the machine.
static THEME: LazyLock<(Theme, Vec<String>)> =
    LazyLock::new(|| if cfg!(test) { (Theme::default(), Vec::new()) } else { load(&theme_path()) });

pub fn theme() -> &'static Theme {
    &THEME.0
}

/// Keys of `theme.toml` that couldn't be used, for the header.
pub fn theme_problems() -> &'static [String] {
    &THEME.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ramp_starts_at_forty_percent() {
        let t = Theme::default();
        assert_eq!(ramp(&t.series, 0.0), Color::Rgb(0x44, 0x01, 0x54));
        assert_eq!(ramp(&t.series, 0.5), Color::Rgb(0x21, 0x91, 0x8c));
        assert_eq!(t.heat_color(0.0), ramp(&t.series, 0.40));
        assert_eq!(t.heat_color(1.0), ramp(&t.series, 1.0));
        assert_ne!(t.agent_color(0), t.agent_color(1));
    }

    #[test]
    fn collisions_flag_status_hues_on_the_ramp() {
        let table = |done: &str| -> Table {
            format!("series = [\"#000000\", \"#ffffff\"]\nseries_from = 0\n[status]\ndone = \"{done}\"").parse().unwrap()
        };
        let mut t = Theme::default();
        assert!(t.apply(&table("#808080")).is_empty());
        assert!(t.collisions().iter().any(|p| p.starts_with("status.done")), "{:?}", t.collisions());
        let mut t = Theme::default();
        t.apply(&table("#ff0000"));
        assert!(t.collisions().is_empty(), "{:?}", t.collisions());
    }

    #[test]
    fn toml_overrides_tokens_and_reports_bad_ones() {
        let table: Table = r##"
            accent = "magenta"
            series = ["#000000", "#ffffff"]
            series_from = 0.0
            fill_alpha = 2.0
            bogus = "red"
            [status]
            working = "#00ff00"
            sleepy = "red"
        "##
        .parse()
        .unwrap();
        let mut t = Theme::default();
        let mut problems = t.apply(&table);
        problems.sort();
        assert_eq!(t.accent, Color::Magenta);
        assert_eq!(t.working, Color::Rgb(0, 255, 0));
        assert_eq!(t.heat_color(0.0), Color::Rgb(0, 0, 0));
        assert_eq!(t.agent_color(0), Color::Rgb(128, 128, 128));
        assert_eq!(t.agent_color(1), Color::Rgb(30, 30, 30));
        assert_eq!(t.fill_alpha, 0.3, "out-of-range value keeps the default");
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("bogus")));
        assert!(problems.iter().any(|p| p.starts_with("status.sleepy")));
    }

    #[test]
    fn appearance_picks_the_base_before_other_keys() {
        let table: Table = "accent = \"magenta\"\nappearance = \"light\"".parse().unwrap();
        let mut t = Theme::default();
        assert!(t.apply(&table).is_empty());
        assert_eq!(t.background, Color::Rgb(0xf5, 0xf5, 0xf7));
        assert_eq!(t.series_from, 0.0);
        assert_eq!(t.accent, Color::Magenta, "keys after the preset still apply");
        assert!(light_background(Some("0;15")) && light_background(Some("0;default;7")));
        assert!(!light_background(Some("15;0")) && !light_background(None));
    }

    #[test]
    fn ramps_must_be_rgb() {
        let table: Table = r##"series = ["red", "#ffffff"]"##.parse().unwrap();
        let mut t = Theme::default();
        assert_eq!(t.apply(&table).len(), 1);
        assert_eq!(t.series, Theme::default().series);
    }

    #[test]
    fn mix_lays_color_over_background() {
        assert_eq!(mix(Color::Rgb(0, 0, 0), Color::Rgb(100, 200, 50), 0.3), Color::Rgb(30, 60, 15));
        assert_eq!(mix(Color::Reset, Color::Rgb(1, 2, 3), 0.3), Color::Rgb(1, 2, 3));
    }
}
