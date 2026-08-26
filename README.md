# tui-launcher

A minimal Unix-style Linux application carousel built with Rust and Ratatui. It discovers
applications from freedesktop `.desktop` entries and renders every visible icon at the same size.
Kitty graphics are preferred, with guaranteed half-block fallbacks and
compact pre-rendered thumbnails for small windows. There is deliberately no search or filtering.

## Controls

| Input | Action |
| --- | --- |
| Left/right or `h l` | Browse one application |
| Up/down or `j k` | Jump five applications |
| Left click or `Enter` | Launch |
| Mouse wheel | Browse applications |
| `Home` / `End` | Select first or last application |
| `S` | Open settings |
| `Escape` or `q` | Close |

## Settings

Press `S` to change the neighbor count, selected icon dimensions, index, footer, accent color, and
panel width. It also includes full theme presets, borderless mode, border styles, and animation speed. Use the arrow
keys to select and change values; press `R` to restore defaults. Changes are saved automatically to
`~/.config/tui-launcher/config.toml` when the launcher closes.

The same file can be edited directly:

```toml
version = 4
neighbor_count = 2
icon_width = 22
icon_height = 11
show_index = true
show_footer = true
show_header = true
transparent = true
theme = 0
accent = 0
border_style = 0
animation_speed = 1
panel_width = 110
```

Themes: Tokyo Night, Nord, Catppuccin, Gruvbox, Everforest, and Mono. Borders: plain, rounded,
double, thick, or none. Carousel movement uses
time-based easing and remains responsive to repeated navigation input.

Transparent mode leaves terminal cells unpainted so the terminal or compositor background can show through.
The `TUI-LAUNCHER` header can be toggled independently.

Narrow terminals automatically show fewer neighboring names and reduce the selected icon height.

## Build and run

```bash
cargo build --release
kitty --class tui-launcher --title Applications ./target/release/tui-launcher
```

List discovered applications without opening the TUI:

```bash
./target/release/tui-launcher --list
```

The project does not install the binary or change desktop keybindings. A later integration can
launch it with `xdg-terminal-exec` and assign that command to a Hyprland binding.
