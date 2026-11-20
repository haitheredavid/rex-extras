# Changelog

All notable changes to trail are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-11-18

Port of herdr-trail 0.1.1 to Rex, as `rex-trail`. Views, keys, session adapters and `config.toml` keys are unchanged. What differs from herdr-trail:

- Rex is reached through the `rex` CLI (`$REX_BIN_DIR`, then the macOS app bundle, then `PATH`): one `rex do` script snapshots every session's windows and terminal blocks, and `rex events --all --json` triggers a debounced snapshot when something relevant changes. A Rex session reads as a workspace, a window as a tab and a block as a pane.
- Agents are recognized from OSC 7501 program status (`effective_app`), else from the foreground process name: omp, Claude Code and Codex. State comes from OSC 7501, with `error` reading as done; a Claude Code block without it takes `busy`/`idle` from its `~/.claude/sessions` file.
- Sessions are found from the files the agent process holds open (omp's root session file, Codex's rollout), or from the `~/.claude/sessions/*.json` entry naming the agent's pid. Before, herdr reported them.
- A Rex Lua module (`lua/trail.lua`, installed as `rex_extras.trail`) replaces the herdr manifest. It registers the `trail_open` (band) and `trail_popup` actions, binds optional keys, and starts the recorder.
- `rex-trail open split|popup --session S --block B` opens the band as a split below block B, sized to `[band] rows`/`columns`, or the popup as a centered floating layer sized to `[popup] width`/`height`.
- The band follows the active window within its session from inside the trail process; there is no `follow` subcommand or pin file. It doesn't follow into other sessions.
- `rex-trail record` is a long-running recorder, kept to one per state directory by `recorder.lock`, instead of a per-event hook. On start it logs running agents as detected, so a restart doesn't split their lanes.
- Blocked reminders are OSC 777 desktop notifications.
- Paths: config in `$REX_TRAIL_CONFIG_DIR`, else `$XDG_CONFIG_HOME/rex-extras/trail`, else `~/.config/rex-extras/trail`; state in `$REX_TRAIL_STATE_DIR`, else `$XDG_STATE_HOME/rex-extras/trail`, else `~/.local/state/rex-extras/trail`. History lines are tagged with the Rex server address.
- `scripts/install.sh` installs to `~/.local/share/rex-extras` (`REX_EXTRAS_HOME`) with release assets `rex-trail-<os>-<arch>`, and prints the init.lua lines instead of editing init.lua. The variables are `REX_TRAIL_BUILD` and `REX_TRAIL_DOWNLOAD_BASE`.
- The herdr protocol warning is gone. Status-only lanes say `status only` instead of `herdr status only`.
- Needs Rust 1.89 or newer to build from source.
