-- rex-trail for Rex: the `trail_open` (band) and `trail_popup` actions, optional keybinds, and
-- the always-on recorder. Load from init.lua:
--   require("rex_extras.trail").setup{ keys = { ["ctrl+a>alt+g"] = "trail_open" } }
local M = {}
local home = os.getenv("HOME")

function M.setup(opts)
  opts = opts or {}
  local bin = opts.bin or (home .. "/.local/share/rex-extras/trail/bin/rex-trail")
  local function open(where)
    return function(ctx)
      if not ctx.session_id or not ctx.block_id then
        rex.log("error", "trail: no focused block")
        return
      end
      local r, err = rex.exec({ bin, "open", where, "--session", ctx.session_id, "--block", ctx.block_id }, { timeout = 10 })
      if not r or not r.ok then rex.log("error", "trail: " .. tostring(r and r.stderr or err)) end
    end
  end
  rex.action{ name = "trail_open", title = "Agent trail (band)", category = "Trail", run = open("split") }
  rex.action{ name = "trail_popup", title = "Agent trail (popup)", category = "Trail", run = open("popup") }
  for key, action in pairs(opts.keys or {}) do rex.bind(key, action) end
  if opts.record ~= false then
    rex.exec.start({ bin, "record" }, { detach = true, stdout = false, stderr = false })
  end
  return M
end

return M
