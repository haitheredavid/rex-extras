# Engineering Guide

Generated: 2026-11-18

## Commands

Run in `plugins/<name>/`:

- `cargo fmt --check` - formatting.
- `cargo clippy --locked --all-targets -- -D warnings` - lints.
- `cargo test --locked` - unit tests for that crate.
- `cargo build --release --locked` - release binary.

Command scope rule: cite exact scope only. Each command covers one extra; there is no repo-wide build.

## Rust

- Each extra is its own crate with its own `Cargo.lock`; no Cargo workspace.
- `Cargo.toml` is the only version source. `--version` prints `<crate> <CARGO_PKG_VERSION>`.
- Keep all Rex wire-format knowledge in the host module (`src/rex.rs` in trail); the model and views stay host-neutral.
- Shell out to the `rex` CLI with a `--timeout`; never block the UI thread on it without one.
- Keep runtime validation at external boundaries: CLI args, env, files, `rex` output.

## Testing Patterns

- `cargo test --locked` in the extra's directory.
- Prefer real local behavior in tests; mock only network, filesystem, time, and other process boundaries.
- Reset env vars, singletons, and temp dirs in test cleanup.

## Boundaries

- Keep external boundary parsing close to HTTP handlers, CLI args, env readers, file readers, queue consumers, and provider SDK wrappers.
- Keep business logic out of transport glue when practical.
- Keep generated API/types/codegen artifacts out of manual edits unless repo docs say otherwise.
- For monorepos, treat shared schema, public API, worker, runtime, and client changes as contract work. Update all consumers together.

## Search Scope

When grepping, finding, or reading within the repo, exclude build output (`target/`). It pollutes results and holds no source-of-truth content.

## Settings

Environment variables and CLI args enter an extra through its config module (`config.rs` in trail) or `main.rs` argument dispatch. Domain code takes values as parameters rather than reading env directly, except for Rex's pane env (`REX_*`) read at the host boundary.

## Safety

- Do not read or copy `.env`, `.secrets`, `*.pem`, `*.key`, `*.p12`.
- Avoid editing generated/local artifacts: `target/`.
- Never write to the user's init.lua; installers print the snippet.
- Release asset names are a contract with `install.sh`; change both together.

- Use `.env.example` only for variable names when needed.
- Do not commit real tokens, bearer headers, database URLs, DSNs, AWS secrets, or local credential values.
- Preserve unrelated dirty work. Never revert files you did not intentionally change.

## Commits

- **Conventional Commits with the extra as scope**: `feat(trail): …`, `fix(trail): …`, `docs(trail): …`, `chore(trail): release 0.1.0`.
- **One concern per commit.** Do not bundle a refactor with a feature with a dep bump.
- **Subject <= 72 chars, imperative mood.**
- **Body explains *why*, not *what*.** The diff shows what.
- **Generated artifacts ride with their source change.** Lockfile updates, codegen outputs, migration SQL - commit with the source edit that triggered them.
- **Never commit secrets.** Real tokens, DSNs, bearer headers, cloud credentials. `.env.example` is for variable names only.
- **Preserve unrelated dirty work.** Never restage or revert files you did not intentionally touch.

## Local Agent Scratch

Agents drop transient working files at repo root while in the middle of a task: `PLAN.md`, `TODO.md`, `NOTES.md`, `SCRATCH.md`. These reflect one session's in-flight reasoning. They are not durable design docs and should not enter git.

Add to `.gitignore`:

```
PLAN.md
TODO.md
NOTES.md
SCRATCH.md
```

- `AGENTS.md` is **durable** and stays committed. It is the canonical entrypoint, not scratch - do not add it to `.gitignore`.
- Durable architecture, decisions, and caveats belong in `.context/` (committed). Scratch belongs at root (ignored).
- A plan worth keeping graduates into an in-flight doc (see `.context/guides/doc-lifecycle.md`) or into the PR description before the scratch file is discarded. Do not park it in a root scratch file.

## Context Maintenance

- Keep `AGENTS.md` compact and operational.
- Put durable architecture, decisions, and caveats in `.context/` guides. Guides never hold progress or status - see `.context/README.md`.
- In-flight briefs, designs, and specs follow `.context/guides/doc-lifecycle.md`.
- Protected-context changes follow `.context/guides/context-maintenance.md`.
