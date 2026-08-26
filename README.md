# tui-launcher

A native PSP-inspired XMB desktop overlay for Linux. The interface is GPU-rendered and controller-first; it is not embedded in a terminal.

The category order follows the PSP home menu:

**Settings → Extras → Photo → Music → Video → Game → Network**

- **Settings** combines launcher appearance, controller mappings, audio/display shortcuts, power profiles, session actions, and guarded power controls.
- **Extras** discovers visible freedesktop desktop applications and resolves their native icons.
- **Photo**, **Music**, and **Video** read the configured media folders. Video thumbnails are generated asynchronously.
- **Game** discovers installed Steam libraries and uses local Steam artwork when available.
- **Network** exposes current connection state, graphical network settings, and the default browser.

## Interface

The native overlay opens as a centered, borderless 16:9 window. Categories move horizontally while the selected category remains at the visual anchor; its content forms the vertical part of the XMB. Selection motion and the background waves are time-based and independent of frame rate.

The default background color follows the current month. Themes offset that monthly palette, and reduced-motion mode disables animated transitions.

Launching an application, game, photo, video, or network tool hides the overlay. On Hyprland, the launcher watches for the new window and restores the overlay to the same category and item after that window closes.

Controller rumble is not used.

## Controls

| Action | Keyboard | Default controller |
| --- | --- | --- |
| Change category | Left / Right | D-pad or left stick |
| Change item | Up / Down | D-pad or left stick |
| Confirm | Enter / Space | South button |
| Back / close | Escape | East button |
| Options menu | T | North button |
| Favorite | F | West button |
| Jump to Settings | S | Start / Menu |

The on-screen prompts intentionally use the PSP-style ×, ○, and △ symbols while input remains mapped to the connected controller.

## Commands

```bash
# Native desktop overlay
tui-launcher

# Full-monitor presentation
tui-launcher --fullscreen

# Open on a specific category
tui-launcher --start game

# Non-graphical inventory and diagnostics
tui-launcher --list
tui-launcher --diagnose
```

## Data

- Configuration: `${XDG_CONFIG_HOME:-~/.config}/tui-launcher/config.toml`
- Favorites, recents, and selections: `${XDG_STATE_HOME:-~/.local/state}/tui-launcher/state.toml`
- Generated video thumbnails: `${XDG_CACHE_HOME:-~/.cache}/tui-launcher/thumbs/`

Version 4 terminal-launcher configuration is migrated to version 5 without discarding appearance preferences. A one-time `config.toml.bak-v4` backup is created when the migrated configuration is saved.

## Build

The native interface uses eframe/egui with wgpu and supports both Wayland and X11.

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
./target/release/tui-launcher --diagnose
```

This project uses original code and procedural visuals. It does not bundle Sony firmware, icons, sounds, or other PlayStation assets.
