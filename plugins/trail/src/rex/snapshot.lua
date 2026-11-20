-- Run by `rex do - --json`: every session's windows and terminal blocks, with each block's
-- process and program status, plus the session the app client shows. A script may only read
-- sessions it is attached to, so it attaches to each one; attaching returns the view.
local TERMINAL = "com.superlogical.terminal"

local function block_call(session_id, block_id, method)
  local ok, result = pcall(rex.call, TERMINAL .. "." .. method, { session_id = session_id, block_id = block_id, args = {} })
  if ok then return result end
  return nil
end

local sessions = {}
for _, s in ipairs(rex.call("session.list").sessions or {}) do
  local view = rex.call("session.attach", { session_id = s.session_id }).view
  local windows = {}
  for _, w in ipairs(view.windows or {}) do
    local blocks = {}
    for _, layer in ipairs(w.layers or {}) do
      for _, b in ipairs(layer.blocks or {}) do
        if b.creator_name == TERMINAL then
          blocks[#blocks + 1] = {
            block_id = b.block_id,
            label = b.label,
            process = block_call(s.session_id, b.block_id, "process"),
            status = block_call(s.session_id, b.block_id, "program_status"),
          }
        end
      end
    end
    windows[#windows + 1] = {
      window_id = w.window_id,
      label = w.label,
      focused_block_id = w.focused_block_id,
      blocks = blocks,
    }
  end
  sessions[#sessions + 1] = {
    session_id = s.session_id,
    label = view.label or s.label,
    active_window_id = view.active_window_id,
    windows = windows,
  }
end

local focused_session_id
for _, c in ipairs(rex.call("client.list").clients or {}) do
  if c.info and c.info.kind == "app" and c.session_ids and c.session_ids[1] then
    focused_session_id = c.session_ids[1]
    break
  end
end

return { sessions = sessions, focused_session_id = focused_session_id }
