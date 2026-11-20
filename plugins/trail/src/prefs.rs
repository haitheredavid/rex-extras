//! Remembered view settings: `prefs.toml` in the state dir, written on quit and read on the
//! next start, between `config.toml [start]` and the command line. Anything unreadable is ignored.

use std::path::{Path, PathBuf};

use toml::Table;

use crate::model::{Group, Scope, Sort};
use crate::view::{App, Axis, Density, Lens, Mode, Orient};

pub fn prefs_path() -> PathBuf {
    crate::history::log_dir().join("prefs.toml")
}

/// The settings as a TOML table.
fn table(app: &App) -> Table {
    let mut t = Table::new();
    let mut put = |k: &str, v: &str| {
        t.insert(k.into(), v.into());
    };
    put("mode", if app.mode == Mode::Usage { "usage" } else { "lanes" });
    put(
        "lens",
        match app.lens {
            Lens::Status => "status",
            Lens::Heat => "heat",
            Lens::Ctx => "ctx",
        },
    );
    put("axis", if app.axis == Axis::Steps { "steps" } else { "time" });
    put(
        "density",
        match app.density {
            Density::Auto => "auto",
            Density::Compact => "compact",
            Density::Roomy => "roomy",
        },
    );
    put("orient", if app.orient == Orient::Vertical { "v" } else { "h" });
    let q = &app.query;
    put(
        "sort",
        match q.sort {
            Sort::Stable => "stable",
            Sort::Attention => "attention",
            Sort::Recent => "recent",
            Sort::Name => "name",
        },
    );
    put(
        "group",
        match q.group {
            Group::Workspace => "workspace",
            Group::Tab => "tab",
            Group::None => "none",
        },
    );
    put(
        "scope",
        match q.scope {
            Scope::All => "all",
            Scope::Workspace => "workspace",
            Scope::Tab => "tab",
        },
    );
    t.insert("active_only".into(), q.active_only.into());
    t.insert("show_closed".into(), q.show_closed.into());
    t
}

pub fn save(path: &Path, app: &App) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, table(app).to_string())
}

/// Applies whatever `path` holds that reads as a setting; everything else is skipped.
pub fn load(path: &Path, app: &mut App) {
    let Some(t) = std::fs::read_to_string(path).ok().and_then(|s| s.parse::<Table>().ok()) else { return };
    let text = |k: &str| t.get(k).and_then(|v| v.as_str());
    let flag = |k: &str| t.get(k).and_then(|v| v.as_bool());
    match text("mode") {
        Some("lanes") => app.mode = Mode::Lanes,
        Some("usage") => app.mode = Mode::Usage,
        _ => {}
    }
    match text("lens") {
        Some("status") => app.lens = Lens::Status,
        Some("heat") => app.lens = Lens::Heat,
        Some("ctx") => app.lens = Lens::Ctx,
        _ => {}
    }
    match text("axis") {
        Some("time") => app.axis = Axis::Time,
        Some("steps") => app.axis = Axis::Steps,
        _ => {}
    }
    match text("density") {
        Some("auto") => app.density = Density::Auto,
        Some("compact") => app.density = Density::Compact,
        Some("roomy") => app.density = Density::Roomy,
        _ => {}
    }
    match text("orient") {
        Some("h") => app.orient = Orient::Horizontal,
        Some("v") => app.orient = Orient::Vertical,
        _ => {}
    }
    let q = &mut app.query;
    match text("sort") {
        Some("stable") => q.sort = Sort::Stable,
        Some("attention") => q.sort = Sort::Attention,
        Some("recent") => q.sort = Sort::Recent,
        Some("name") => q.sort = Sort::Name,
        _ => {}
    }
    match text("group") {
        Some("workspace") => q.group = Group::Workspace,
        Some("tab") => q.group = Group::Tab,
        Some("none") => q.group = Group::None,
        _ => {}
    }
    match text("scope") {
        Some("all") => q.scope = Scope::All,
        Some("workspace") => q.scope = Scope::Workspace,
        Some("tab") => q.scope = Scope::Tab,
        _ => {}
    }
    if let Some(b) = flag("active_only") {
        q.active_only = b;
    }
    if let Some(b) = flag("show_closed") {
        q.show_closed = b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::State;

    #[test]
    fn prefs_round_trip() {
        let dir = std::env::temp_dir().join(format!("rex-trail-prefs-{}", std::process::id()));
        let path = dir.join("prefs.toml");
        let mut app = App::new(State::default(), 4, Orient::Vertical, 0);
        app.density = Density::Roomy;
        app.query.sort = Sort::Attention;
        app.query.group = Group::Tab;
        app.query.active_only = true;
        save(&path, &app).unwrap();

        let mut back = App::new(State::default(), 1, Orient::Horizontal, 0);
        load(&path, &mut back);
        assert_eq!(
            (back.mode, back.lens, back.axis, back.density, back.orient),
            (Mode::Lanes, Lens::Heat, Axis::Time, Density::Roomy, Orient::Vertical)
        );
        assert_eq!(back.query, app.query);

        std::fs::write(&path, "mode = 3\nlens = \"sideways\"\nnot toml [").unwrap();
        let mut untouched = App::new(State::default(), 1, Orient::Horizontal, 0);
        load(&path, &mut untouched);
        assert_eq!(untouched.lens, Lens::Status, "an unreadable file changes nothing");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
