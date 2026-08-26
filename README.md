# tui-launcher

An original PSP-inspired XMB console hub for Linux terminals. `tui-launcher` combines desktop
applications, an installed Steam library, local media, quick system controls, and appearance/input
settings in one keyboard-, mouse-, and controller-native interface.

The normal command remains a quick floating launcher. Full-screen mode stays alive behind launched
content and refreshes when focus returns.

## Modes

- **Applications** discovers visible freedesktop `.desktop` entries and launches them with `gio` or
  `gtk-launch`.
- **Games** discovers installed Steam libraries and uses local Steam posters, heroes, and metadata.
- **Media** exposes MPRIS now-playing controls and browses the configured Videos, Pictures, and Music
  folders. Images render directly; video thumbnails are generated lazily when `ffmpegthumbnailer`
  is available.
- **System** provides capability-aware volume, brightness, power-profile, lock, logout, suspend,
  restart, and power-off actions. Session-ending actions require a one-second hold.
- **Settings** changes themes, XMB waves, transparency, sound, rumble, reduced motion, optional
  artwork downloads, panel width, and every controller binding.

Favorites, the last position in each mode, and the latest 20 launched items persist between runs.
Artwork and thumbnails load outside the render path, so missing or slow assets do not block input.

## Controls

| Keyboard | Controller default | Action |
| --- | --- | --- |
| Left/right or `h l` | D-pad left/right | Change mode |
| Up/down or `j k` | D-pad up/down | Browse items |
| `Enter` | South / Cross / A | Launch or change setting |
| `C` | North / Triangle / Y | Open or close details |
| `F` | West / Square / X | Toggle favorite |
| `S` | Start/Menu | Open Settings |
| `Escape` or `q` | East / Circle / B | Back or close |
| Mouse wheel | — | Browse items |

Controller prompts adapt to PlayStation- and Xbox-style devices. Bindings are remappable from the
Settings mode; assigning an occupied button swaps the two actions. Sound and force feedback fail
softly when no audio device or rumble-capable controller is present.

## Commands

```bash
# Quick floating overlay (default)
tui-launcher

# Persistent console hub
tui-launcher --fullscreen

# Open directly in a mode
tui-launcher --fullscreen --start games

# Backward-compatible desktop-entry list
tui-launcher --list

# Content counts and backend availability
tui-launcher --diagnose
```

`--overlay` explicitly selects the quick profile. `--overlay` and `--fullscreen` are mutually
exclusive.

## Configuration and state

- Configuration: `${XDG_CONFIG_HOME:-~/.config}/tui-launcher/config.toml`
- Persistent favorites/history: `${XDG_STATE_HOME:-~/.local/state}/tui-launcher/state.toml`
- Generated/downloaded artwork: `${XDG_CACHE_HOME:-~/.cache}/tui-launcher/`

Version 4 configuration files migrate to version 5 without losing the existing theme, accent,
transparency, border, icon dimensions, animation speed, or panel width. The original v4 file is
backed up once as `config.toml.bak-v4` when v5 is first saved. Writes use a same-directory temporary
file and atomic rename.

Network artwork is off by default. When enabled, only missing Steam posters are downloaded over
HTTPS, decoded before use, and stored in a cache capped at 256 MiB.

## Build

Rust 1.85 or newer is recommended. Linux controller/audio support requires the development packages
for `libudev` and ALSA. Optional runtime integrations include Steam, `ffmpegthumbnailer`, `ffprobe`,
`xdg-open`, `wpctl`/`pactl`, `brightnessctl`, `powerprofilesctl`, `loginctl`, and Hyprland tools.

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
./target/release/tui-launcher --diagnose
```

Install only after validating the build interactively:

```bash
cargo install --path . --locked --force
```

The interface and synthesized feedback are original. No PSP artwork, sound, or firmware assets are
included.
