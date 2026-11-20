# Project Context

Generated: 2026-11-18

## What This Repo Is

rex-extras: personal extras for the [Rex](https://www.superlogical.com/rex) terminal, one self-contained subdirectory per extra, each installable on its own.

The repo is used by its author on their own Rex setup and published so others can install individual extras. Each extra owns its binary, its Lua glue and its release tags. What it should preserve: installing one extra never requires another, and nothing edits the user's init.lua.

## Architecture

```text
init.lua -> require("rex_extras.<name>").setup{} -> rex.action / rex.bind / rex.exec.start
action -> external binary -> rex CLI (api call, do, events) -> Rex server
external binary (long-running) -> rex events --all --json -> snapshot via rex do -> TUI / history files
```

- `plugins/<name>/` is a standalone Rust crate (no Cargo workspace) plus `lua/<name>.lua`.
- The Lua module only registers actions and keybinds and spawns the binary; all logic lives in the binary.
- Host access is one module per extra (`src/rex.rs` in trail) that shells out to the `rex` CLI. It is the only code that knows Rex's wire format.

How Rex is extended, with the exact methods and payloads relied on: [`reference/rex-extension-model.md`](reference/rex-extension-model.md).

## Ownership Map

| Path | Package | Purpose |
|------|---------|---------|
| `plugins/trail/` | `rex-trail` | Agent activity TUI and recorder. |
| `plugins/trail/lua/trail.lua` | Lua module | Actions `trail_open` (band) and `trail_popup`, optional keybinds, recorder start. |
| `plugins/trail/scripts/install.sh` | installer | Downloads (or builds) the binary and copies the Lua module. |
| `.github/workflows/` | CI | `trail.yml` checks, `trail-release.yml` tag-driven release. |

## External Integrations

- Rex server via the `rex` CLI (`REX_BIN_DIR`, `REX_SESSION`, `REX_BLOCK`, `REX_SERVER`).
- Agent session files on disk: omp (`~/.omp/agent/sessions`), Claude Code (`CLAUDE_CONFIG_DIR` or `~/.claude`), Codex (`CODEX_HOME` or `~/.codex`).
- GitHub Releases for prebuilt binaries.

## Durable Decisions

- One crate per extra, no Cargo workspace: each extra builds, versions and releases independently.
- `Cargo.toml` is the only version source; tags are `<name>-v<version>` and must match it.
- Release asset names are a contract with `install.sh`.
- Installers never write to init.lua; they print the snippet.
- Event-driven work runs in an external process, because `rex.on` is unavailable at config load.
- Commits use Conventional Commits with the extra as scope (`feat(trail): …`).

## Non-Goals

- A plugin manager or manifest format for Rex.
- Shared runtime code between extras.
