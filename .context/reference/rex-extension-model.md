# Rex extension model

Verified on rex 0.1.0-preview.20, API revision 16.

**No plugin manager, no manifest.** An extra is a Lua module that init.lua `require`s, plus external programs that drive the `rex` CLI.
- The Lua runtime is 5.1. `require` and `dofile` exist. `package.path` is `./?.lua;/usr/local/share/lua/5.1/?.lua;…`.
- `rex.action`, `rex.bind` and `rex.exec`/`rex.exec.start` work at config load.
- `rex.on` does not work in config, only in `rex do` scripts. Event-driven work therefore lives in a long-running process.

**`rex api call METHOD '<json>'`** prints the result JSON on stdout.

**`rex do - --json`** runs a Lua script read from stdin and prints its return value as JSON.
- `rex.call("session.list")` returns `{sessions:[{session_id,label}]}`.
- `rex.call("session.view",{session_id})` returns `{session_id,label,active_window_id,windows:[{window_id,label,focused_block_id,layers:[{layer_id,kind,bounds,blocks:[{block_id,label,creator_name,flavor}]}]}],clients:[…]}`.
- `rex.call("com.superlogical.terminal.<method>",{session_id,block_id,args={}})` calls a block method. It works for any session.

**Terminal `process`** returns `{child:{pid,name,…}, foreground:{pid,name,path,argv0,cwd}|nil, ancestors:[…], last_exit}`.

**Terminal `program_status`** returns `{records:[{state,app,effective_app,owner:{pid,name},updated_at, id?}]}`.
- These are OSC 7501 records. omp emits them natively: `state=working, app=omp`.
- States are `idle|working|done|blocked|error`.

**Terminal `size`** returns `{rows,columns,…}`.

**`client.list`** returns `{clients:[{client_id, info:{kind:"app"|"cli",…}, session_ids:[…]}]}`. The app client's `session_ids[0]` is the session it shows.

**`rex events --all --json`**: one JSON line per event.
- Session events are wrapped as `{"type":"session_event","payload":{"session_id","event":{"type", "payload"}}}`.
- Terminal events are nested once more as `event.type=="block_event"` with `payload.{block_id,creator_name,name,payload}`.
- Names that matter: `window_created|window_closed|window_label_changed|active_window_changed|block_created|block_closed|session_label_changed`, server-level `session_created|session_destroyed`, and terminal `program_status_changed|program_status_removed|process_changed|child_exited|pwd_changed|title_changed`.
- omp's spinner fires `title_changed` several times a second.

**Write methods** (schemas read with `rex api describe`):
- `session.new_split {session_id, anchor_id, direction:"horizontal"|"vertical", side:"before"|"after", ratio, layout, focus}`
- `session.new_layer {session_id, window_id, bounds:[x,y,w,h] fractions, layout, focus}`
- `session.move_block {session_id, block_id, anchor_block_id, direction, side, ratio}`
- `session.focus_block {session_id, block_id}`
- The client action `rex do session.select session_id=S`. With exactly one app client connected, `rex do` uses it.

**Layout node JSON** (from `rex.layout.block`): `{"block":{"flavor":"com.superlogical.terminal.shell","options":{"command":[…],"shell":"none"},"label":"Trail"}}`.

**Agent session files:**
- omp keeps its root session `.jsonl` open: `lsof -p <omp pid>` shows `~/.omp/agent/sessions/<slug>/<file>.jsonl`, plus subagent files one level deeper.
- Claude Code writes `<CLAUDE_CONFIG_DIR|~/.claude>/sessions/<n>.json` containing `{"pid", "sessionId", "status":"busy"|…}`. The file name is **not** always the pid; match on the `pid` field.

**Environment:**
- Panes get `REX_SESSION`, `REX_BLOCK`, `REX_SERVER` and `REX_BIN_DIR` (the directory holding `rex`).
- `REX_BIN_DIR` is also set for `rex do` scripts.
