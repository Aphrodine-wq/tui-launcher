# tui-launcher

A native PSP-inspired XMB desktop overlay for Linux. The interface is GPU-rendered and controller-first; it is not embedded in a terminal.

The category order follows the PSP home menu:

**Settings → Extras → Apps → Photo → Music → Video → Game → Network**

- **Settings** renders as a menu panel with Appearance, Controller, System, and Power groups; confirming a group opens it in place and Back returns.
- **Settings → System → Wi-Fi Networks** is a native Wi-Fi manager built on iwd (`iwctl`): scan, signal bars, connect to open or saved networks, disconnect, and a password prompt for new secured networks (typed with the keyboard). An external tool remains available as Advanced Network Settings.
- **Extras** holds the launcher's own features rather than applications: a full-screen **Clock**, **System Information**, and **Screen Off** (blanks the displays until a button is pressed).
- **Apps** discovers visible freedesktop desktop applications and resolves their native icons. Applications are homed by their desktop categories — game launchers under Game, music players under Music, video players and libraries under Video, image viewers under Photo — and everything else (including web apps) lands here. Steam-created shortcuts that duplicate installed library games are dropped. Hide an application with △ Options → Hide; restore them all under Settings → System → Hidden applications.
- **Photo**, **Music**, and **Video** list their applications first (an image viewer, Spotify, mpv), then browse the configured media folders one level at a time — folders that contain the right kind of media appear as entries you open, with ○ stepping back up. Video thumbnails are generated asynchronously. In **Music**, confirming a local track plays it inside the launcher: the queue is the rest of the column, △ Options gives Pause, Previous/Next Track, and Stop, and the current track shows in the status bar and beside the item. Playback continues while you browse and launch other things.
- **Game** shows a **Continue** row first (the game launched most recently from here, else the one played most recently on Steam), then the Steam client, then other game launchers (such as Prism Launcher), then installed Steam games with local artwork and playtime. Runtime tools, dedicated servers, and Wallpaper Engine are filtered out.
- **Network** opens the default browser and shows the connection state.

## Interface

The native overlay opens as a centered, borderless 16:9 window. Categories move horizontally while the selected category remains at the visual anchor; its content forms the vertical part of the XMB, and items above the selection jump over the category crossbar the way the original interface does. Category icons are original vector drawings rendered at any size. Selection motion and the background are time-based and independent of frame rate.

The background is a monthly gradient crossed by filled, glowing wave ribbons with drifting sparkles. Several animated effects layer on top, each an independent on/off under Settings → Appearance: **Waves**, **Wave sparkles**, a parallax **Starfield**, occasional **Comets**, and a perspective **Grid floor** — mix and match freely. All are procedural, time-based, and accent-tinted, and pace themselves: navigation renders at ~60 fps, slow ambient motion at ~30 fps, and a static interface stops repainting entirely so it costs nothing while idle. The wave accent setting tints the ribbons, sparkles, and boot wordmark (Classic, Aqua, Amber, Rose, Emerald). The transparent-overlay setting switches between a translucent and a fully opaque background. Reduced-motion mode freezes every effect, disables animated transitions, and skips the short boot-in animation. Sound volume, the boot animation, and the clock format (12/24h) are individually adjustable, and Settings → System → System Information shows an about screen for the machine and launcher.

**Background styles**: *Monthly gradient* or *Picture*.

**Default backgrounds**: Settings → Appearance → Default background cycles a set of original abstract wallpapers (Aurora, Nebula, Mesh, Dusk, Tide, Vanishing) generated in code on first run and cached under `${XDG_CACHE_HOME:-~/.cache}/tui-launcher/backgrounds/`. Selecting one uses it as the picture background; "None" returns to the monthly gradient.

**Idle clock**: after the configured idle time (Settings → Appearance → Idle clock; Off, 2, 5, 10, 15, or 30 minutes) the interface fades to a large clock and date over the waves. Any input wakes it; it never appears while a game or app launched from the launcher is in the foreground.

**Audio output**: Settings → System → Audio Output cycles the default sink through the outputs WirePlumber reports (`wpctl`), the way the Wi-Fi manager cycles networks.

**Theme packs** are the modding format: a folder in `${XDG_CONFIG_HOME:-~/.config}/tui-launcher/themes/<name>/` containing a `theme.toml` (accent, gradient, background picture, WAV interface sounds — all optional) and an `icons/` directory of per-category SVG/PNG icons that replace the built-in drawn icons. Share a pack by zipping the folder; install one by dropping it in. Select packs under Settings → Appearance → Theme pack. A documented starter pack ships in `examples/midnight-vapor/`.

The Network category identifies the actual default web browser (via `xdg-settings`) with its real name and icon.

Any photo can become the background: select it under **Photo**, open Options, and choose "Set as Background". The picture is drawn aspect-filled behind a legibility scrim with the waves on top, and Appearance → Background picture clears it back to the monthly gradient. `--background PATH` previews a picture for one run without saving it.

Every visible item shows its title; the selected one adds its subtitle and, when it has real artwork (a game cover, a photo, a video frame), a corner preview. Application icons are never blown up into a preview. Artwork is never stretched: item icons and the preview keep their source aspect ratio, icons are downscaled on the CPU for crispness at list size, and low-resolution art is not blown up.

When "Fetch missing Steam artwork" is enabled, boxart for installed Steam games that have no local artwork is downloaded once from Steam's public CDN and cached under `${XDG_CACHE_HOME:-~/.cache}/tui-launcher/steam-art/`. This is the only network access in the application and it stays off by default.

Launching an application, game, photo, video, or network tool hides the overlay. On Hyprland, the launcher watches for the new window and restores the overlay to the same category and item after that window closes.

**Controller settings** (Settings → Controller): every action is remapped by **press-to-bind** — select it, press the button you want, and any collision swaps. **Stick deadzone** and **Scroll repeat** (hold-to-repeat speed) are adjustable, **Rumble** gives a short buzz on confirm/back/warning at an adjustable strength (when the pad supports force feedback), **Test controller** shows the detected pad with live button lamps, and **Reset controller** restores the defaults.

## Controls

| Action | Keyboard | Default controller |
| --- | --- | --- |
| Change category | Left / Right | D-pad or left stick |
| Change item | Up / Down | D-pad or left stick |
| Confirm | Enter / Space | South button |
| Back | Escape | East button |
| Options menu | T | North button |
| Favorite | F | West button |
| Jump to Settings | S | Start / Menu |

The on-screen prompts intentionally use the PSP-style ×, ○, and △ symbols while input remains mapped to the connected controller.

Press **H** (or **Extras → Controls & Help**) at any time for a full-screen list of every control with its current keyboard key and controller button; a one-time hint points there on first run.

Back only steps out of panels and sub-lists; at the top level it does nothing, so a stray press cannot dismiss the launcher. The overlay is closed deliberately through **Settings → Power → Close overlay** (or the window manager). In **kiosk mode** (`--kiosk`) there is nothing behind the launcher, so Close overlay is not offered.

## Commands

```bash
# Native desktop overlay
tui-launcher

# Full-monitor presentation
tui-launcher --fullscreen

# Open on a specific category
tui-launcher --start game

# Kiosk: fullscreen, no "close overlay" — for running it as the desktop
tui-launcher --kiosk

# Non-graphical inventory and diagnostics
tui-launcher --list
tui-launcher --diagnose
```

## Desktop session

The launcher can be a whole desktop rather than a window. `tui-launcher --kiosk`
runs it fullscreen with no self-close. To make it a login session, install the
bundled entry, which launches the launcher inside the [`cage`](https://github.com/cage-kiosk/cage)
kiosk compositor:

```bash
scripts/install-session.sh      # copies dist/tui-launcher.desktop into the sessions dir (needs cage + sudo)
```

Your display manager will then offer **XMB Launcher** as a session. To try it
without logging out: `cage -- tui-launcher --kiosk`.

As a lighter alternative, launch-or-focus keeps it a keystroke away without a
resident process — cold start is ~0.3 s, so binding a key to
`omarchy-launch-or-focus xmb "tui-launcher"` (or your compositor's equivalent)
summons it effectively instantly.

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
