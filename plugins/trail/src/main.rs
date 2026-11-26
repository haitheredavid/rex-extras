mod config;
mod history;
mod model;
mod open;
mod prefs;
mod record;
mod rex;
mod session;
mod theme;
mod view;

use std::collections::HashSet;
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use serde_json::json;

use config::{config, history_ms};
use model::{State, due_notifications};
use record::{DEBOUNCE, RESUBSCRIBE_FIRST, RESUBSCRIBE_MAX, RETRY_EVERY, SNAPSHOT_EVERY};
use rex::{Host, Msg, Subscriber};
use view::{App, Orient, fmt_dur, preset_from_name};

const NOTICE_MS: i64 = 5_000;
const REDRAW_EVERY: Duration = Duration::from_secs(1);
const USAGE: &str = "usage: rex-trail [--popup] [--view time|sequence|area|heat|dash] [--orient h|v] | --dump [--width N --height N --keys \"j j Tab\" ...] | --version | open split|popup --session S --block B | record";

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// The block this runs in; its own lane is skipped.
fn self_pane() -> Option<String> {
    env_nonempty("REX_BLOCK")
}

struct Opts {
    popup: bool,
    dump: bool,
    /// `--view` as preset `1`-`5`; None leaves it to `config.toml` and the remembered settings.
    view: Option<u8>,
    orient: Option<Orient>,
    width: u16,
    height: u16,
    /// `--dump` presses these before drawing.
    keys: Vec<KeyEvent>,
}

/// Keys from `--keys`: space-separated single characters, key names, or `Space`.
fn parse_keys(s: &str) -> anyhow::Result<Vec<KeyEvent>> {
    s.split_whitespace()
        .map(|t| {
            let code = match t {
                "Tab" => KeyCode::Tab,
                "BackTab" => KeyCode::BackTab,
                "Esc" => KeyCode::Esc,
                "Enter" => KeyCode::Enter,
                "Backspace" => KeyCode::Backspace,
                "Up" => KeyCode::Up,
                "Down" => KeyCode::Down,
                "Left" => KeyCode::Left,
                "Right" => KeyCode::Right,
                "Home" => KeyCode::Home,
                "End" => KeyCode::End,
                "PgUp" => KeyCode::PageUp,
                "PgDn" => KeyCode::PageDown,
                "Space" => KeyCode::Char(' '),
                _ => {
                    let mut chars = t.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => KeyCode::Char(c),
                        _ => bail!("unknown key {t:?}"),
                    }
                }
            };
            Ok(KeyEvent::new(code, KeyModifiers::NONE))
        })
        .collect()
}

fn parse_orient(v: &str) -> anyhow::Result<Orient> {
    match v {
        "h" => Ok(Orient::Horizontal),
        "v" => Ok(Orient::Vertical),
        v => bail!("unknown orientation {v:?}"),
    }
}

fn parse_opts(args: &[String]) -> anyhow::Result<Opts> {
    let mut o = Opts { popup: false, dump: false, view: None, orient: None, width: 120, height: 30, keys: Vec::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--popup" => o.popup = true,
            "--dump" => o.dump = true,
            "--view" => {
                let v = value()?;
                o.view = Some(preset_from_name(v).with_context(|| format!("unknown view {v:?}"))?);
            }
            "--orient" => o.orient = Some(parse_orient(value()?)?),
            "--width" => o.width = value()?.parse().context("--width")?,
            "--height" => o.height = value()?.parse().context("--height")?,
            "--keys" => o.keys = parse_keys(value()?)?,
            other => bail!("unknown argument {other:?}"),
        }
    }
    if !o.dump && args.iter().any(|a| a == "--width" || a == "--height" || a == "--keys") {
        bail!("--width/--height/--keys only apply to --dump");
    }
    Ok(o)
}

/// What `--version` prints; the install script checks downloads against it.
fn version_line() -> String {
    format!("rex-trail {}", env!("CARGO_PKG_VERSION"))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("{}", version_line());
            Ok(())
        }
        Some(cmd @ ("open" | "record")) => {
            let result = if cmd == "open" {
                open::run(&args[1..]).unwrap_or_else(|| {
                    eprintln!("{}", open::USAGE);
                    std::process::exit(2)
                })
            } else {
                record::run()
            };
            if let Err(e) = result {
                eprintln!("rex-trail {cmd}: {e:#}");
                std::process::exit(1);
            }
            Ok(())
        }
        _ => match parse_opts(&args) {
            Ok(o) if o.dump => {
                dump(&o);
                Ok(())
            }
            Ok(o) => run_tui(&o),
            Err(e) => {
                eprintln!("rex-trail: {e:#}\n{USAGE}");
                std::process::exit(2);
            }
        },
    }
}

fn refresh(app: &mut App, host: &mut Host) -> bool {
    match host.snapshot() {
        Ok(snap) => {
            app.state.apply_snapshot(&snap, now_ms());
            app.refresh_usage();
            app.error = None;
            app.connected = true;
            true
        }
        Err(e) => {
            app.error = Some(format!("{e:#}"));
            app.connected = false;
            false
        }
    }
}

/// App with the logged history folded in, then the live snapshot on top. Settings stack from the
/// defaults through `config.toml [start]` and the remembered ones (not for `--dump`) to the flags.
fn start(o: &Opts) -> (App, Host, bool) {
    let now = now_ms();
    let mut state = State::new(self_pane());
    for e in history::load_since(&history::log_dir(), &history::session_name(), now, now - history_ms()) {
        if let Some(h) = e.host_event() {
            state.apply(&h, e.ts_ms, false);
        }
    }
    let cfg = config();
    let preset = cfg.start_view.as_deref().and_then(preset_from_name).unwrap_or(1);
    let orient = cfg.start_orient.as_deref().and_then(|v| parse_orient(v).ok()).unwrap_or(Orient::Horizontal);
    let mut app = App::new(state, preset, orient, now);
    if !o.dump && cfg.remember {
        prefs::load(&prefs::prefs_path(), &mut app);
    }
    if let Some(v) = o.view {
        app.preset(v);
    }
    if let Some(orient) = o.orient {
        app.orient = orient;
    }
    app.popup = o.popup;
    let mut host = Host::default();
    let connected = refresh(&mut app, &mut host);
    (app, host, connected)
}

fn dump(o: &Opts) {
    let (mut app, _, _) = start(o);
    let area = Rect::new(0, 0, o.width, o.height);
    // A first draw sets the geometry keys depend on (time cells, scroll, layout).
    view::draw(&mut Buffer::empty(area), area, &mut app);
    for &k in &o.keys {
        app.on_key(k);
    }
    let mut buf = Buffer::empty(area);
    view::draw(&mut buf, area, &mut app);
    for y in 0..area.height {
        let line: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        println!("{}", line.trim_end());
    }
}

/// Text safe inside an OSC 777 field: no `;` separators, no control characters.
fn osc_field(s: &str) -> String {
    s.chars().filter(|c| *c != ';' && !c.is_control()).collect()
}

/// Posts a desktop notification (OSC 777) for each agent blocked past `[notify] blocked_after`,
/// once per episode.
fn notify_blocked(app: &mut App, sent: &mut HashSet<(u64, usize)>) {
    let Some(after) = config().blocked_after_ms else { return };
    let now = now_ms();
    for key in due_notifications(&app.state, now, after, sent) {
        sent.insert(key);
        let Some(li) = app.state.lane_by_id(key.0) else { continue };
        let lane = &app.state.lanes[li];
        let title = format!("{} blocked {}", lane.label(), fmt_dur(now - lane.last_ms()));
        let body = format!("{} · {}", app.state.workspace_label(li), lane.agent.as_deref().unwrap_or("-"));
        let mut out = std::io::stdout();
        let written = write!(out, "\x1b]777;notify;{};{}\x1b\\", osc_field(&title), osc_field(&body)).and_then(|()| out.flush());
        if let Err(e) = written {
            app.error = Some(format!("notify: {e}"));
        }
    }
}

/// Focuses a lane's block, selecting its session in the app first when it isn't the shown one.
fn focus(app: &App, block_id: &str) -> anyhow::Result<()> {
    let lane = app.state.lanes.iter().rfind(|l| l.pane_id == block_id).context("no lane for that block")?;
    if app.state.focused_workspace.as_deref() != Some(lane.workspace_id.as_str()) {
        let _ = rex::client_action("session.select", &[&format!("session_id={}", lane.workspace_id)]);
    }
    rex::call("session.focus_block", json!({ "session_id": lane.workspace_id, "block_id": block_id })).map(drop)
}

/// Moves the band into `window` when it becomes the active window of the band's own session, the
/// one holding its block; a band not in the snapshot doesn't follow.
fn follow(app: &App, host: &Host, session_id: &str, window_id: &str) -> anyhow::Result<()> {
    let Some(band) = self_pane() else { return Ok(()) };
    if app.popup || !config().band_follow || host.raw.session_of(&band) != Some(session_id) {
        return Ok(());
    }
    match open::follow_anchor(&host.raw, session_id, &band, window_id) {
        Some(anchor) => open::follow(session_id, &band, &anchor),
        None => Ok(()),
    }
}

fn run_tui(o: &Opts) -> anyhow::Result<()> {
    let (mut app, mut host, connected) = start(o);
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut sub = Some(Subscriber::start(tx.clone()));
    let mut resubscribe_at: Option<Instant> = None;
    let mut backoff = RESUBSCRIBE_FIRST;
    let mut resnap_at: Option<Instant> = None;
    // Episodes already past the threshold when this trail opened were due before it: notifying
    // again on every open (or once per open trail) would repeat them.
    let mut notified: HashSet<(u64, usize)> = match config().blocked_after_ms {
        Some(after) => due_notifications(&app.state, now_ms(), after, &HashSet::new()).into_iter().collect(),
        None => HashSet::new(),
    };

    let mut next_snapshot = Instant::now() + if connected { SNAPSHOT_EVERY } else { RETRY_EVERY };
    let mut terminal = ratatui::init();
    let mouse = config().mouse;
    if mouse {
        execute!(std::io::stdout(), EnableMouseCapture)?;
    }
    let result = (|| -> anyhow::Result<()> {
        // ratatui writes a style reset and cursor hide on every draw, even an unchanged one, and
        // each write makes the terminal recomposite the block; so draw only on change or once a
        // second for the clock.
        let mut dirty = true;
        let mut next_draw = Instant::now();
        loop {
            if dirty || Instant::now() >= next_draw {
                app.now_ms = now_ms();
                terminal.draw(|f| {
                    let area = f.area();
                    view::draw(f.buffer_mut(), area, &mut app)
                })?;
                dirty = false;
                next_draw = Instant::now() + REDRAW_EVERY;
            }

            let wait = next_draw.saturating_duration_since(Instant::now()).min(Duration::from_millis(100));
            if event::poll(wait)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            return Ok(());
                        }
                        app.on_key(key);
                        dirty = true;
                    }
                    Event::Mouse(m) => {
                        app.on_mouse(m);
                        dirty = true;
                    }
                    _ => {}
                }
            }
            if let Some(block_id) = app.focus_requested.take() {
                match focus(&app, &block_id) {
                    Ok(()) if app.popup => return Ok(()),
                    Ok(()) => {}
                    Err(e) => {
                        app.error = Some(format!("focus {block_id}: {e:#}"));
                        dirty = true;
                    }
                }
            }
            if app.quit {
                return Ok(());
            }
            notify_blocked(&mut app, &mut notified);

            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Msg::Changed => {
                        backoff = RESUBSCRIBE_FIRST;
                        resnap_at.get_or_insert_with(|| Instant::now() + DEBOUNCE);
                    }
                    Msg::ActiveWindow { session_id, window_id } => {
                        // Follow from a snapshot that has the new window in it.
                        refresh(&mut app, &mut host);
                        if let Err(e) = follow(&app, &host, &session_id, &window_id) {
                            app.error = Some(format!("follow: {e:#}"));
                        }
                        dirty = true;
                    }
                    Msg::Lost(reason) => {
                        sub = None;
                        app.notice = Some((reason, now_ms() + NOTICE_MS));
                        resubscribe_at = Some(Instant::now() + backoff);
                        backoff = (backoff * 2).min(RESUBSCRIBE_MAX);
                        dirty = true;
                    }
                }
            }

            let now = Instant::now();
            if resubscribe_at.is_some_and(|t| now >= t) {
                resubscribe_at = None;
                sub = Some(Subscriber::start(tx.clone()));
            }
            if app.refresh_requested || now >= next_snapshot || resnap_at.is_some_and(|t| now >= t) {
                app.refresh_requested = false;
                resnap_at = None;
                let ok = refresh(&mut app, &mut host);
                next_snapshot = now + if ok { SNAPSHOT_EVERY } else { RETRY_EVERY };
                dirty = true;
            }
        }
    })();
    drop(sub);
    if mouse {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
    if config().remember {
        let _ = prefs::save(&prefs::prefs_path(), &app);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn parse_opts_reads_keys() {
        let mut a = args("--dump");
        a.extend(["--keys".to_string(), "j Tab ? Space".to_string()]);
        let o = parse_opts(&a).unwrap();
        let codes: Vec<KeyCode> = o.keys.iter().map(|k| k.code).collect();
        assert_eq!(codes, [KeyCode::Char('j'), KeyCode::Tab, KeyCode::Char('?'), KeyCode::Char(' ')]);
        assert!(parse_opts(&args("--dump --keys Nope")).is_err());
        assert!(parse_opts(&args("--keys j")).is_err(), "--keys needs --dump");
    }

    #[test]
    fn parse_opts_reads_view_orient_dump_and_keys() {
        let mut a = args("--dump --view heat --orient v --width 50 --height 30");
        a.extend(["--keys".to_string(), "j Tab".to_string()]);
        let o = parse_opts(&a).unwrap();
        assert!(o.dump && !o.popup);
        assert_eq!(o.view, preset_from_name("heat"));
        assert_eq!(o.orient, Some(Orient::Vertical));
        assert_eq!((o.width, o.height, o.keys.len()), (50, 30, 2));
        assert!(parse_opts(&args("--view nope")).is_err());
        assert!(parse_opts(&args("--orient x")).is_err());
    }

    #[test]
    fn parse_opts_rejects_width_without_dump() {
        assert!(parse_opts(&args("--width 50")).is_err());
        assert!(parse_opts(&args("--popup --height 10")).is_err());
        assert!(parse_opts(&args("--dump --width wide")).is_err());
    }

    #[test]
    fn version_flag_prints_name_and_version() {
        assert_eq!(version_line(), format!("rex-trail {}", env!("CARGO_PKG_VERSION")));
        assert!(version_line().starts_with("rex-trail 0."));
    }
}
