# Agent Guide

## Project Context

rex-extras: personal extras for the [Rex](https://www.superlogical.com/rex) terminal, one self-contained subdirectory per extra, each installable on its own.

Additional durable context lives in `.context/` (conventions in `.context/README.md`):

- `.context/project-context.md` - product purpose, architecture, ownership, and durable decisions.
- `.context/reference/rex-extension-model.md` - how Rex is extended: Lua module, CLI, API methods, events, environment.
- `.context/guides/engineering.md` - commands, Rust style, testing patterns, boundaries, and safety rules.
- `.context/guides/writing-briefs.md` - the brief -> design -> spec flow and how to write a brief.
- `.context/guides/writing-designs.md` - how to write a design that splits a brief into specs.
- `.context/guides/writing-specs.md` - how to write a spec for one shippable unit; shared language rules.
- `.context/guides/doc-lifecycle.md` - where briefs, designs, specs, and other in-flight docs are drafted, reviewed, and closed.
- `.context/guides/pull-requests.md` - PR branch, title, body, size, linear vs. stacked.
- `.context/guides/context-maintenance.md` - altitude rule, protected paths, context PR procedure.
- `.context/README.md` - `.context/` layout: guides, reference, archive; other files in `guides/` come from the team profile.

## Repo Shape

Each `plugins/<name>/` is self-contained. A Rust extra is its own crate with `Cargo.lock`, `README.md` and `CHANGELOG.md`; there is no Cargo workspace. Its Rex side is `lua/<name>.lua`, installed to `~/.local/share/rex-extras/lua/rex_extras/<name>.lua` and loaded from init.lua. Tags are per extra (`trail-v0.1.0`).

| Path | Package | Purpose |
|------|---------|---------|
| `plugins/trail/` | `rex-trail` | Agent activity TUI: a lane per agent block with state, context and cost, plus a session timeline. |
| `plugins/trail/lua/trail.lua` | Lua module | Registers the `trail_open` / `trail_popup` actions and starts the recorder. |
| `.github/workflows/trail*.yml` | CI | Per-extra checks and tag-driven releases. |

## Rex Extension Model

Rex has no plugin manager and no manifest. An extra is a Lua module that init.lua `require`s, plus an external program that drives the `rex` CLI (`rex api call`, `rex do`, `rex events`). `rex.on` only works in `rex do` scripts, not in config, so event-driven work lives in a long-running process. Details: `.context/reference/rex-extension-model.md`.

## Toolchain

- Rust stable; `rex-trail` MSRV is 1.89 (`rust-version` in `Cargo.toml`, checked in CI).

## Commands

Run in `plugins/<name>/`:

- `cargo fmt --check` - formatting.
- `cargo clippy --locked --all-targets -- -D warnings` - lints.
- `cargo test --locked` - unit tests for that crate only.
- `cargo build --release --locked` - release binary.

Do not overstate command scope. Each command covers one extra; there is no repo-wide build.

## Hard Rules

- Release asset names are a contract with `install.sh`. Change both together.
- `Cargo.toml` is the only version source.
- Never write to the user's init.lua. Installers print the snippet instead.

## Testing

- `cargo test --locked` in the extra's directory.
- Prefer real local behavior in tests; mock only network, filesystem, time, and other process boundaries.
- Reset env vars, singletons, and temp dirs in test cleanup.

## Safety

- Do not read or copy `.env`, `.secrets`, `*.pem`, `*.key`, `*.p12`.
- Avoid editing generated/local artifacts: `target/`.
- Do not commit real tokens or local credential values.
- Preserve unrelated dirty work. Never revert files you did not intentionally change.

Search scope, settings discipline, and commit style live in [`.context/guides/engineering.md`](.context/guides/engineering.md).

## MCP Servers

- None configured for this repo.

## Skills

- `rex` (user-level skill) for driving the Rex CLI while testing.

## Where To Edit

- Start with `.context/project-context.md` for architecture and ownership.
- Start with `.context/guides/engineering.md` for commands, tests, style, and safety.
- Keep root guidance compact. Move durable detail into `.context/`.

## Maintenance

- Update this file when commands, package ownership, or safety boundaries change.
- Record durable decisions in `.context/project-context.md`.
- Do not use this file as a task log; track in-flight work per `.context/guides/doc-lifecycle.md`.
