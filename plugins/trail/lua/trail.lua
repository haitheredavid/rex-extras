-- trail for Rex: actions trail_open (band), trail_popup, trail_toggle, trail_focus and
-- trail_next_blocked, optional keybinds, and the always-on recorder. Load from init.lua:
--   require("rex_extras.trail").setup{ keys = { ["ctrl+a>alt+g"] = "trail_toggle" } }
local M = {}
local terminal = "com.superlogical.terminal"
local BAND, POPUP = "Trail", "Trail popup"

local function check(r, err, what)
  if r == nil then error("trail: " .. what .. ": " .. tostring(err), 0) end
  return r
end

-- $REX_EXTRAS_HOME/trail/bin/rex-trail, as scripts/install.sh lays it out; nil without a root.
local function default_bin()
  local root = os.getenv("REX_EXTRAS_HOME")
  if not root then
    local home = os.getenv("HOME")
    if not home then return nil end
    root = home .. "/.local/share/rex-extras"
  end
  return root .. "/trail/bin/rex-trail"
end

-- Calls fn(window, block) for every block of view, in window, layer and layout order; stops at
-- the first truthy return and passes it on.
local function each_block(view, fn)
  for _, w in ipairs(view.windows or {}) do
    for _, l in ipairs(w.layers or {}) do
      for _, b in ipairs(l.blocks or {}) do
        local r = fn(w, b)
        if r then return r end
      end
    end
  end
end

local function view_of(session_id)
  local view, err = rex.session.view{ session_id = session_id }
  return check(view, err, "session.view")
end

-- The block labeled label in view, if any.
local function find(view, label)
  return each_block(view, function(_, b) if b.label == label then return b.block_id end end)
end

local function window_of(view, block_id)
  return each_block(view, function(w, b) if b.block_id == block_id then return w.window_id end end)
    or view.active_window_id
end

local function focus(view, block_id)
  local r, err = rex.session.focus_block{ session_id = view.session_id, block_id = block_id }
  check(r, err, "session.focus_block")
end

local function pane(argv, label)
  -- argv[1] is the absolute path of the installed binary, so no shell is needed to find it.
  return rex.layout.block{
    flavor = terminal .. ".shell", label = label,
    options = { command = argv, shell = "none" },
  }
end

-- `rex-trail config --json`: the resolved [band] and [popup] settings.
local function trail_config(bin)
  local r, err = rex.exec({ bin, "config", "--json" }, { timeout = 5 })
  check(r, err, "rex-trail config")
  if not r.ok then
    local msg = r.stderr ~= "" and r.stderr or ("exited with status " .. tostring(r.code))
    error("trail: rex-trail config: " .. msg, 0)
  end
  local c, derr = rex.json.decode(r.stdout)
  return check(c, derr, "rex-trail config output")
end

-- Splits the band off the focused block, sized to [band] rows or columns. new_split ignores its
-- ratio, so the band is placed again with move_block, which honors it; the ratio comes from the
-- anchor's size before the split.
local function open_band(ctx, bin, view)
  local anchor = ctx.block_id
  if not anchor then error("trail: no focused block", 0) end
  local band = trail_config(bin).band
  local direction, cells, key = "vertical", band.rows, "rows"
  if band.split == "right" then direction, cells, key = "horizontal", band.columns, "columns" end
  local size, err = rex.block.call(terminal, "size", { block_id = anchor })
  check(size, err, "terminal size")
  local total = size[key]
  if type(total) ~= "number" or total <= 0 then error("trail: terminal size: no " .. key, 0) end
  local ratio = math.min(math.max(1 - cells / total, 0.2), 0.9)
  local opened, serr = rex.session.new_split{
    session_id = view.session_id, anchor_id = anchor, direction = direction, side = "after", focus = true,
    layout = pane({ bin }, BAND),
  }
  check(opened, serr, "session.new_split")
  local id = opened.block_ids and opened.block_ids[1]
  if not id then error("trail: session.new_split: no block id in result", 0) end
  local moved, merr = rex.session.move_block{
    session_id = view.session_id, block_id = id, anchor_block_id = anchor,
    direction = direction, side = "after", ratio = ratio,
  }
  check(moved, merr, "session.move_block")
end

-- A centered floating layer, [popup] width x height, in the window holding the focused block.
local function open_popup(ctx, bin, view)
  local popup = trail_config(bin).popup
  local w, h = popup.width, popup.height
  local window = window_of(view, ctx.block_id)
  if not window then error("trail: session has no window", 0) end
  local r, err = rex.session.new_layer{
    session_id = view.session_id, window_id = window, focus = true,
    bounds = { (1 - w) / 2, (1 - h) / 2, w, h },
    layout = pane({ bin, "--popup" }, POPUP),
  }
  check(r, err, "session.new_layer")
end

-- A block that closed mid-scan, or answers nothing, counts as not blocked.
local function blocked(session_id, block_id)
  local r = rex.block.call(terminal, "program_status", { session_id = session_id, block_id = block_id })
  for _, rec in ipairs(r and r.records or {}) do
    if rec.state == "blocked" then return true end
  end
  return false
end

-- Another session's view needs an attach first; attaching returns it.
local function any_view(ctx, session_id)
  if session_id == ctx.session_id then return view_of(session_id) end
  local r, err = rex.call("session.attach", { session_id = session_id })
  return check(r, err, "session.attach").view
end

-- Focuses the next terminal block reporting `blocked`, across every session, after the focused
-- block in session, window and block order, wrapping around.
local function next_blocked(ctx)
  local list, err = rex.call("session.list")
  check(list, err, "session.list")
  local all, current = {}, 0
  for _, s in ipairs(list.sessions or {}) do
    each_block(any_view(ctx, s.session_id), function(w, b)
      if b.creator_name == terminal then
        all[#all + 1] = { session_id = s.session_id, window_id = w.window_id, block_id = b.block_id }
        if b.block_id == ctx.block_id then current = #all end
      end
    end)
  end
  for i = 1, #all do
    local t = all[(current + i - 1) % #all + 1]
    if blocked(t.session_id, t.block_id) then
      focus(t, t.block_id)
      if t.session_id ~= ctx.session_id then
        local ok, qerr = pcall(rex.client.queue, "session.select", { session_id = t.session_id, window_id = t.window_id })
        if not ok then error("trail: session.select: " .. tostring(qerr), 0) end
      end
      return
    end
  end
  error("trail: no blocked agents", 0)
end

function M.setup(opts)
  opts = opts or {}
  local bin = opts.bin or default_bin()
  local function need_bin()
    if not bin then error("trail: no rex-trail path: pass setup{ bin = ... } or set HOME", 0) end
    return bin
  end
  local function action(name, title, description, run)
    rex.action{ name = name, title = title, category = "Trail", description = description, keywords = "agents trail", run = run }
  end

  action("trail_open", "Trail: Open Band", "Open the trail as a band split beside the focused block, or focus the open one.",
    function(ctx)
      local view = view_of(ctx.session_id)
      local band = find(view, BAND)
      if band then return focus(view, band) end
      open_band(ctx, need_bin(), view)
    end)
  action("trail_popup", "Trail: Open Popup", "Open the trail as a centered floating popup, or focus the open one.",
    function(ctx)
      local view = view_of(ctx.session_id)
      local popup = find(view, POPUP)
      if popup then return focus(view, popup) end
      open_popup(ctx, need_bin(), view)
    end)
  action("trail_toggle", "Trail: Toggle Band", "Close this session's trail band, or open it beside the focused block.",
    function(ctx)
      local view = view_of(ctx.session_id)
      local band = find(view, BAND)
      if not band then return open_band(ctx, need_bin(), view) end
      local r, err = rex.call("block.close", { session_id = view.session_id, block_id = band })
      check(r, err, "block.close")
    end)
  action("trail_focus", "Trail: Focus", "Focus this session's trail band or popup, or open the band.",
    function(ctx)
      local view = view_of(ctx.session_id)
      local open = find(view, BAND) or find(view, POPUP)
      if open then return focus(view, open) end
      open_band(ctx, need_bin(), view)
    end)
  action("trail_next_blocked", "Trail: Next Blocked Agent", "Focus the next agent waiting on you, in any session.",
    next_blocked)

  for key, name in pairs(opts.keys or {}) do rex.bind(key, name) end
  if opts.record ~= false then
    if not bin then
      rex.log("error", "trail: cannot start recorder: no rex-trail path (pass setup{ bin = ... } or set HOME)")
    else
      local proc, err = rex.exec.start({ bin, "record" }, { detach = true, stdout = false, stderr = false })
      if not proc then rex.log("error", "trail: cannot start recorder: " .. tostring(err)) end
    end
  end
  return M
end

return M
