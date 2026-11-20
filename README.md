# rex-extras

Personal [Rex](https://www.superlogical.com/rex) extras, one subdirectory per extra, each installable on its own.

| Extra | What it does |
| --- | --- |
| [`trail`](plugins/trail/README.md) | Agent activity for Rex: a lane per agent block with state, context and cost, and a step-through session timeline |
| [`panoptes`](plugins/panoptes/README.md) | Opens the panoptes GitHub reviewer console for the focused pane's repository, as an overlay or a tab |

## Install

Each extra's README has its install command and options. For trail:

```
git clone https://github.com/haitheredavid/rex-extras && sh rex-extras/plugins/trail/scripts/install.sh
```

## Layout

Each `plugins/<name>/` is self-contained. A Rust extra is its own crate with `Cargo.lock`, `README.md` and `CHANGELOG.md`; there is no Cargo workspace. Its Rex side is `lua/<name>.lua`, installed to `~/.local/share/rex-extras/lua/rex_extras/<name>.lua` and loaded from init.lua. Releases are tagged per extra (`trail-v0.1.0`).

## trail

See [plugins/trail/README.md](plugins/trail/README.md) for views, keys, configuration, data handling and development.

## panoptes

See [plugins/panoptes/README.md](plugins/panoptes/README.md) for install, actions and keys.
