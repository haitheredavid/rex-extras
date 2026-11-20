# panoptes for Rex

Opens the [panoptes](https://github.com/david-morgan-suffolk/panoptes) GitHub reviewer console for the repository of the pane you are working in, either as a floating overlay or in a new tab. The Rex port of `herdr-panoptes`; a Lua module only, with no binary.

## Prerequisites

- `panoptes` on `PATH` (`brew install david-morgan-suffolk/tap/panoptes`)
- `gh`, authenticated with `gh auth login`

## Install

```
sh plugins/panoptes/scripts/install.sh
```

It copies `lua/panoptes.lua` to `~/.local/share/rex-extras/lua/rex_extras/panoptes.lua` (`REX_EXTRAS_HOME` changes the root) and prints the lines to add to init.lua:

```lua
package.path = os.getenv("HOME") .. "/.local/share/rex-extras/lua/?.lua;" .. package.path
require("rex_extras.panoptes").setup{ keys = { ["ctrl+a>alt+o"] = "panoptes_open", ["ctrl+a>alt+shift+o"] = "panoptes_open_tab" } }
```

Then `rex config reload`. Skip the `package.path` line if another extra already adds it.

## Actions

| Action | Effect |
| --- | --- |
| `panoptes_open` | Open panoptes in a floating layer covering 90% of the focused pane's window |
| `panoptes_open_tab` | Open panoptes in a new tab labelled `panoptes` |

The repository is taken from the focused pane's foreground process's working directory, which follows `cd`. Quitting panoptes with `q` closes the overlay or tab. If panoptes fails, for example because the directory is not a GitHub checkout, the pane stays open showing the error until you press Enter.

Run one by hand with `rex do panoptes_open`, or pick it from the command palette under **panoptes**.

Image charts in the issue reader need panoptes built with Rex in its kitty-graphics terminal list (after v0.2.0); older releases show tables without charts inside Rex.
