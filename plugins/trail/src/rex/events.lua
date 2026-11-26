-- Run by `rex-trail` as `rex do --all - [band=BLOCK split=down|right rows=N columns=N]`. Prints one
-- JSON line per change a snapshot would show: `{"msg":"changed"}`, coalesced over a short burst,
-- and `{"msg":"active_window","session_id":…,"window_id":…}`. With `band`, it also moves that
-- block beside the focused block of whichever window of its session becomes active.
io.stdout:setvbuf("no")

local TERMINAL = "com.superlogical.terminal"
local DEBOUNCE = 0.1

local function emit(t)
  print(rex.json.encode(t))
end

local pending = false
local function changed()
  if pending then return end
  pending = true
  rex.after(DEBOUNCE, function()
    pending = false
    emit({ msg = "changed" })
  end)
end

-- Session events, then terminal events, that change what a snapshot shows. Others, like
-- `terminal.title_changed` (omp's spinner, several a second) and `client_connected` (every `rex`
-- call, ours included), would only cost snapshots.
for _, name in ipairs({
  "session_created",
  "session_destroyed",
  "window_created",
  "window_closed",
  "window_label_changed",
  "block_created",
  "block_closed",
  "session_label_changed",
  "session_view_changed",
  "terminal.program_status_changed",
  "terminal.program_status_removed",
  "terminal.process_changed",
  "terminal.child_exited",
  "terminal.pwd_changed",
}) do
  rex.on(name, changed)
end

local args = rex.args or {}
local band = args.band

local function must(what, result, err)
  if result == nil then error(what .. ": " .. tostring(err), 0) end
  return result
end

-- The session's view; a script reads only sessions it is attached to, and attaching returns it.
local function view_of(session_id)
  local view = rex.call("session.view", { session_id = session_id })
  if view then return view end
  return must("session.attach", rex.call("session.attach", { session_id = session_id })).view
end

local function blocks_of(window)
  local blocks = {}
  for _, layer in ipairs(window.layers or {}) do
    for _, b in ipairs(layer.blocks or {}) do
      blocks[#blocks + 1] = b.block_id
    end
  end
  return blocks
end

local function contains(list, id)
  for _, v in ipairs(list) do
    if v == id then return true end
  end
  return false
end

-- The block to place the band beside when `window_id` becomes active: that window's focused block,
-- else its first. None when the band is not in this session, already in that window, or alone in
-- its own (moving it would leave that window empty).
local function follow_anchor(view, window_id)
  local target, home
  for _, w in ipairs(view.windows or {}) do
    if w.window_id == window_id then target = w end
    if contains(blocks_of(w), band) then home = w end
  end
  if not target or not home or home == target or #blocks_of(home) < 2 then return nil end
  return target.focused_block_id or blocks_of(target)[1]
end

local function follow(session_id, window_id)
  local anchor = follow_anchor(view_of(session_id), window_id)
  if not anchor then return end
  local direction, cells, key = "vertical", args.rows, "rows"
  if args.split == "right" then direction, cells, key = "horizontal", args.columns, "columns" end
  local size = must("terminal size", rex.call(TERMINAL .. ".size", { session_id = session_id, block_id = anchor, args = {} }))
  local total = tonumber(size[key])
  if not total or total <= 0 then error("terminal size: no " .. key, 0) end
  -- Share the anchor keeps so the band after it gets its cells.
  local ratio = math.min(math.max(1 - cells / total, 0.2), 0.9)
  must("session.move_block", rex.session.move_block({
    session_id = session_id,
    block_id = band,
    anchor_block_id = anchor,
    direction = direction,
    side = "after",
    ratio = ratio,
  }))
end

rex.on("active_window_changed", function(ctx, ev)
  local session_id = ctx.session_id or ev.session_id
  emit({ msg = "active_window", session_id = session_id, window_id = ev.window_id })
  if band then
    local ok, err = pcall(follow, session_id, ev.window_id)
    if not ok then emit({ msg = "error", text = "follow: " .. tostring(err) }) end
  end
end)
