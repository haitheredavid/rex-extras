-- panoptes for Rex: the `panoptes_open` (overlay) and `panoptes_open_tab` actions, which open the
-- panoptes GitHub reviewer console for the focused pane's repository, and optional keybinds.
-- Load from init.lua:
--   require("rex_extras.panoptes").setup{ keys = { ["ctrl+a>alt+o"] = "panoptes_open" } }
local M = {}
local terminal = "com.superlogical.terminal"

-- Runs panoptes in $1 and, if it fails, keeps the pane open on its error until Enter.
local pane_script = [[
hold() { printf '\n%s\npress enter to close…' "$1"; read -r _; exit 1; }
command -v panoptes >/dev/null 2>&1 || hold "panoptes is not on PATH. Install it: brew install david-morgan-suffolk/tap/panoptes"
cd "$1" || hold "cannot cd into $1"
panoptes || hold "panoptes exited with status $? (run it from a GitHub checkout; see the error above)."
]]

local function call(method, args)
  local r, err = rex.call(method, args)
  if r == nil then error("panoptes: " .. method .. ": " .. tostring(err), 0) end
  return r
end

-- The working directory of the block's foreground process, which follows `cd`.
local function cwd_of(ctx)
  local p = call(terminal .. ".process", { session_id = ctx.session_id, block_id = ctx.block_id, args = {} })
  local cwd = p.foreground and p.foreground.cwd
  if not cwd or cwd == "" then error("panoptes: cannot read the working directory of the focused pane", 0) end
  return cwd
end

-- The window holding the block, so the overlay covers the pane it was opened from.
local function window_of(ctx)
  local view = call("session.view", { session_id = ctx.session_id })
  for _, w in ipairs(view.windows or {}) do
    for _, layer in ipairs(w.layers or {}) do
      for _, b in ipairs(layer.blocks or {}) do
        if b.block_id == ctx.block_id then return w.window_id end
      end
    end
  end
  return view.active_window_id
end

local function layout(cwd)
  return rex.layout.block{
    flavor = terminal .. ".shell",
    label = "panoptes",
    options = { command = { "sh", "-c", pane_script, "panoptes", cwd } },
  }
end

local function open(where)
  return function(ctx)
    if not ctx.session_id or not ctx.block_id then error("panoptes: no focused pane", 0) end
    local cwd = cwd_of(ctx)
    if where == "tab" then
      call("session.new_window", { session_id = ctx.session_id, window_label = "panoptes", layout = layout(cwd) })
    else
      call("session.new_layer", {
        session_id = ctx.session_id,
        window_id = window_of(ctx),
        bounds = { 0.05, 0.05, 0.9, 0.9 },
        focus = true,
        layout = layout(cwd),
      })
    end
  end
end

function M.setup(opts)
  opts = opts or {}
  rex.action{ name = "panoptes_open", title = "panoptes (overlay)", category = "panoptes", run = open("overlay") }
  rex.action{ name = "panoptes_open_tab", title = "panoptes (new tab)", category = "panoptes", run = open("tab") }
  for key, action in pairs(opts.keys or {}) do rex.bind(key, action) end
  return M
end

return M
