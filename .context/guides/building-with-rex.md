# Building With Rex

How an extra uses Rex's own surfaces (Lua actions, blocks, events) instead of terminal workarounds. Exact methods and payloads: [`../reference/rex-extension-model.md`](../reference/rex-extension-model.md). Rex docs: https://www.superlogical.com/rex/docs (also the `superlogical` MCP server).

## The three layers

Every extra is built from at most three layers. Each has one job; do not let one do another's.

|Layer|Runs as|Owns|Never|
|---|---|---|---|
|**Lua module** `lua/<name>.lua`|init.lua `require`, server-side|The public interface: `rex.action`s, optional keybinds, starting the event consumer.|Long work (actions have 30 s), state, parsing agent or tool output.|
|**Event consumer** (optional)|Detached `rex do <script>.lua --all`, or a binary reading `rex events --all --json`|Reacting to Rex events; writing the extra's own state files.|UI. Owning layout the Lua module can do.|
|**Program in a block** (optional)|A terminal block the Lua module opens|Rendering and interaction: TUI, heavy parsing, domain logic.|Deciding where it is placed. Guessing which session or block it is in.|

```text
key / palette / rex do / agent -> rex.action (Lua) -> rex.session.* + rex.layout.block -> block running the program
config load -> rex.exec.start{detach} -> event consumer -> rex.on / rex events -> state files
program in block -> REX_BLOCK -> rex api call / OSC 7501 -> Rex
```

## Where a capability goes

Take the first rung that holds:

1. **A Rex built-in or client action does it.** Bind it or `rex.invoke` / `rex.client.queue` it; write nothing.
2. **One action can do it in Lua within 30 s.** `rex.action` with `rex.session.*`, `rex.block.call`, `rex.layout.*`, `rex.exec`.
3. **It needs a choice or a line of text from the person.** `rex.ui.pick` / `rex.ui.input` inside the action's `run`, not a TUI prompt.
4. **It must react to events.** An event consumer. Prefer a `rex do` script with `rex.on`, because handlers get unwrapped payloads and `rex.session.*`; use a binary on `rex events` only when the reaction needs the binary's parsing or state.
5. **It needs a screen of its own.** A program in a block, opened by an action.
6. **Rex cannot answer it at all** (another tool's session files, pids). Read it in the binary's host module, behind one function, and say in a comment which Rex surface would replace it.

`rex.on` does not work in init.lua (it warns and never fires), so rung 4 is always a separate process. `rex.call`, `rex.session.*` and `rex.block.call` fail at load and work inside actions. Check a function's row in [Where each function works](https://www.superlogical.com/rex/docs/reference/lua#where-each-function-works) before using it.

## Lua module

The module is a template, not a shared library: extras share no runtime code.

```lua
-- <name> for Rex: actions <name>_<verb>[_<where>], optional keybinds. Load from init.lua:
--   require("rex_extras.<name>").setup{ keys = { ["ctrl+a>alt+x"] = "<name>_open" } }
local M = {}
local terminal = "com.superlogical.terminal"

local function check(r, err, what)
  if r == nil then error("<name>: " .. what .. ": " .. tostring(err), 0) end
  return r
end

local function window_of(block_id)
  local view = check(rex.session.view{}, nil, "session.view")
  for _, w in ipairs(view.windows or {}) do
    for _, l in ipairs(w.layers or {}) do
      for _, b in ipairs(l.blocks or {}) do
        if b.block_id == block_id then return w.window_id end
      end
    end
  end
  return view.active_window_id
end
local function pane(argv, cwd, label)
  -- argv[1] is absolute (the extra's own binary), so no shell is needed to find it.
  return rex.layout.block{
    flavor = terminal .. ".shell", label = label,
    options = { command = argv, shell = "none", cwd = cwd },
  }
end

function M.setup(opts)
  opts = opts or {}
  rex.action{
    name = "<name>_open", title = "<Name>: Open Overlay", category = "<Name>",
    run = function(ctx)
      if not ctx.block_id then error("<name>: no focused pane", 0) end
      local r, err = rex.session.new_layer{
        window_id = window_of(ctx.block_id), bounds = { 0.05, 0.05, 0.9, 0.9 }, focus = true,
        layout = pane({ "<program>" }, nil, "<name>"),
      }
      check(r, err, "session.new_layer")
    end,
  }
  for key, action in pairs(opts.keys or {}) do rex.bind(key, action) end
  return M
end

return M
```

- **Actions are the interface.** Everything a person, key, agent or script can do is a `rex.action`; keys, the palette, `rex do <action> k=v` and agents then share one entry point. Give actions typed `args` so they run headless, plus `description`/`keywords` for palette search.
- **Naming.** Actions `<name>_<verb>[_<where>]`, lowercase and underscores (no dots: dotted names belong to clients and blocks). Placement words: `overlay`, `split`, `tab`. `category` is the extra's display name; `title` starts with it.
- **Errors.** Fail an action with `error("<name>: …", 0)` so whoever ran it sees the message and `rex do` exits non-zero. `rex.log` is for background noise only. Every `rex.call` / `rex.session.*` / `rex.exec` result is checked: they return `nil, err` instead of raising.
- **Options.** `setup(opts)` takes `keys`, plus `bin` when there is a binary and `<feature> = false` to disable background work. Defaults never index a possibly-nil env var (`os.getenv("HOME") ..` at load breaks the whole init.lua).
- **Load-time safety.** At load the module only defines actions, binds keys and starts the consumer. Nothing at load calls the API or blocks.

## Blocks and placement

- **Open UI as a block from Lua.** `rex.layout.block` + `rex.session.new_layer` (overlay), `new_split` (split), `new_window` (tab), `float_block` / `zoom_block` for existing blocks. Do not build layout JSON by hand in a binary or shell out to a binary just to place a pane; delegate to the binary only when placement needs the binary's own config.
- **Choose the shell deliberately and pass `cwd`.** `shell = "none"` starts `command` directly with Rex's own environment: use it when `argv[1]` is an absolute path the extra installed. Leave `shell` unset when the program is found on the user's `PATH` (Homebrew, `~/.local/bin`): the user's shell reads its startup files first, and the arguments still reach the program unchanged. Use a wrapper `sh -c` only to keep an error on screen, with every value passed as a positional parameter, never spliced into the script.
- **Pass identity explicitly.** The action knows `ctx.session_id` and `ctx.block_id`; pass them to the program as arguments. A program that must find itself uses `REX_BLOCK` (any `-s` accepts a block ID). Never rely on `REX_SESSION`: Rex no longer sets it.
- **Ask blocks, do not scrape.** Use terminal block methods (`process` for foreground cwd and argv, `program_status`, `size`, `format`) through `rex.block.call`, not process-name matching, title parsing, or screen reading.
- **Label what you open** (`label` on the block, `window_label` on a tab) so people, agents and your own code can find it in `session.view`.
- **Discover, do not guess.** `rex api describe <method>` and `rex api list` give the live schema; `block_creator.list` gives flavors and their options.

## Design system

Rex has a small native vocabulary. Extras use it instead of inventing their own, so they look and behave like Rex. Live schemas win over this list: `rex api call -s "$REX_BLOCK" block_creator.list '{}'` and `rex api describe <method>`.

**Surfaces: pick by role.**

|Role|Surface|Call|
|---|---|---|
|Primary tool the person switches to|Window (tab)|`rex.session.new_window{ window_label, layout }`|
|Companion beside the work it describes|Split|`rex.session.new_split{ anchor_id, direction, side, ratio, layout }`|
|Transient overlay, dismissed after use|Floating layer|`rex.session.new_layer{ window_id, bounds = {x, y, w, h}, focus = true, layout }`; fractions from the top-left, inside the window|
|Temporary focus on an existing block|Zoom|`rex.session.zoom_block`|
|Runs unseen, placed later|Detached block|`rex.session.new_block`, then `move_block`|
|One question or choice|Command palette|`rex.ui.pick` / `rex.ui.input` inside an action's `run`|

Panes are never shorter than 5 rows; a band or status strip that needs fewer is a floating layer.

**Block types.** Build UI from the terminal block (`com.superlogical.terminal`, flavor `shell`). Its options: `command`, `shell`, `cwd`, `path_prepend`, `initial_input`, `exit`, `size`, `theme`, `login`, `interactive` (`args` is deprecated). Other creators (onboarding, tunnel) belong to Rex; extras do not create them. There are no user-defined block types: an extra's UI is a program in a terminal block.

**Icons.** The only icon vocabulary is `rex.ui.pick` items, 16 names: `document`, `folder`, `branch`, `terminal`, `code`, `server`, `database`, `package`, `globe`, `link`, `person`, `tag`, `clock`, `star`, `gear`, `plus`. Match the name to what the row is (`folder` for a directory, `branch` for a git branch, `server` for a host). An unknown name shows no icon. Actions, labels and status have no icon field; tab icons come from Rex's built-in program list.

**Status.** A program reports state with OSC 7501 (`state=idle|working|done|blocked|error|clear`, with `app`, `kind=permission|question|auth` for `blocked`, `progress` 0-100, base64 `title`/`msg`, `id` path). Rex turns it into tab spinners, attention dots, the session picker, the Dock badge and notifications. Repeat `app` in every report; report `blocked` with a `kind` when waiting on the person; report `done` or `error` just before exit and `idle` on cancel. Spec: https://www.superlogical.com/rex/docs/build/program-status

**Words.**
- Action `title` is Title Case, prefixed with the extra's display name (`Trail: Open Band`), and ends in `…` when the action asks a question. `category` is the display name; add `description` and `keywords`.
- Label every block, window and session the extra creates (128 characters max) with the extra's lowercase name or what the block shows.
- `rex.ui.pick` rows: `title` for the thing, `subtitle` for its path or location, `detail` for a short status at the row's end. `rex.ui.input` takes `title`, `placeholder`, `value`, `confirm`.
- No answer: return quietly on `cancelled`; raise or log any other reason (`timeout`, `no_client`, `unsupported`, `busy`, `failed`).

**Color.** Extras cannot read or set the app theme, and `set_theme` changes only one terminal's defaults. TUIs draw with the default foreground/background and ANSI colors 0-15, which follow the person's theme; truecolor may be remapped for contrast. Do not pass `theme` or call `set_theme` to style an extra.

**Sidebar and tab bar.** Not extensible: no custom rows, fields, icons or status line. An extra shows up there only through what it already feeds: labels, OSC 7501 status, and `rex.client.queue("client.tab.unread")`. Show/hide and sizing are the person's settings (`client.sidebar.toggle`); an extra does not change them.

## Event consumers

- **Prefer a `rex do --all` script** registering `rex.on` handlers at load. Handlers run one at a time; debounce bursts with `rex.after`, then re-read `rex.session.view` rather than folding many events into a cached copy.
- **A binary reading `rex events --all --json`** keeps all envelope parsing in its host module and fails loudly when a line does not parse, instead of silently falling back to polling.
- **Start once, from Lua:** `rex.exec.start({ … }, { detach = true })` in `setup`, with its `nil, err` checked and reported through `rex.log`. Config reloads re-run `setup`, so the consumer takes a lock and exits quietly if another copy holds it.
- **Detached processes outlive Rex.** The consumer exits on its own once the server has been unreachable for a while; the next config load restarts it. One lock per server, not per machine, when state is tagged by server.
- **Prefer events to polling.** Poll only as a slow safety net, never as the main loop.

## Programs in blocks

- **Report state with OSC 7501** (`idle|working|done|blocked|error`, with `app` and `msg`). It drives tab spinners, the session picker and `terminal.program_status_changed`, which other extras can consume. Do not encode state in the title.
- **Report resumability with OSC 7577** plus a `resume.trust` rule the user adds, not a custom restore mechanism.
- **Notify** through the terminal (OSC 9/777) or `rex.exec` of an OS notifier from Lua. Rex has no notification API; do not invent one in a status file.
- **Host module.** All Rex wire format (CLI argv, JSON shapes, event envelopes, env vars) lives in one module per binary; the model and views never see it. See [engineering.md](engineering.md#rust).

## Testing against Rex

- `rex config check` validates init.lua with the extra loaded.
- `rex do lua/<name>.lua --action <action>` runs one action from the file without reloading config.
- `rex do <script>.lua` runs a consumer in the foreground; `rex events --all --json` shows what it would receive.
- `rex actions` lists the loaded actions and their arguments.

## Anti-patterns

- An action that only `rex.exec`s a binary which then calls `rex api call` to open a pane.
- Status, session or agent identity from process names, `lsof`, window titles or screen text when a block method or OSC record carries it.
- A long-running process with no lock, no exit condition, or an unchecked start.
- Logging an action's failure and returning success.
- A TUI prompt for a one-off choice that `rex.ui.pick` can ask.
- Reading `REX_SESSION`, hardcoding the Rex app bundle path, or guessing `rex` on `PATH` when `REX_BIN_DIR` or the action's `ctx` gives the answer.
