use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use freedesktop_desktop_entry::{
    DesktopEntry, current_desktop, default_paths, get_languages_from_env,
};

use crate::{
    config::{PersistentState, Settings},
    media::{self, MediaKind},
    model::{
        Action, LibraryItem, MediaControl, Mode, NetworkAction, NowPlaying, SettingAction,
        SystemAction, SystemStatus,
    },
};

pub fn discover_all(
    settings: &Settings,
    persisted: &PersistentState,
) -> HashMap<Mode, Vec<LibraryItem>> {
    let mut modes = HashMap::new();
    modes.insert(Mode::Settings, setting_root_items());
    let library = discover_games();
    let library_ids: HashSet<u32> = library
        .iter()
        .filter_map(|item| match item.action {
            Action::Steam(app_id) => Some(app_id),
            _ => None,
        })
        .collect();
    // Steam and Steam-classified applications live in Game (dropping the
    // desktop shortcuts that duplicate installed library games). Other
    // applications are homed by their desktop categories: game launchers
    // in Game, players and viewers in Music, Video, and Photo, and the
    // rest in Apps. Hidden applications are skipped everywhere.
    let mut apps = Vec::new();
    let mut steam_apps = Vec::new();
    let mut homed: HashMap<Mode, Vec<LibraryItem>> = HashMap::new();
    for application in discover_applications() {
        if persisted.hidden.contains(&application.item.id)
            || application
                .steam_shortcut
                .is_some_and(|app_id| library_ids.contains(&app_id))
        {
            continue;
        }
        if application.steam {
            if !steam_tool(&application.item.title, 0) {
                steam_apps.push(application.item);
            }
        } else if application.home == Mode::Apps {
            apps.push(application.item);
        } else {
            homed
                .entry(application.home)
                .or_default()
                .push(application.item);
        }
    }
    modes.insert(Mode::Extras, extras_items());
    modes.insert(Mode::Apps, apps);
    for (mode, kind) in [
        (Mode::Photo, MediaKind::Photo),
        (Mode::Music, MediaKind::Music),
        (Mode::Video, MediaKind::Video),
    ] {
        let mut items = homed.remove(&mode).unwrap_or_default();
        items.extend(media::list(&settings.media_paths, kind, None));
        modes.insert(mode, items);
    }
    let mut games = steam_apps;
    games.extend(homed.remove(&Mode::Game).unwrap_or_default());
    games.extend(library);
    if let Some(resume) = continue_item(&games, persisted) {
        games.insert(0, resume);
    }
    modes.insert(Mode::Game, games);
    modes.insert(Mode::Network, network_items());
    for (mode, items) in &mut modes {
        // Settings and Extras keep their curated order; favorites and
        // recents would scramble them.
        if !matches!(mode, Mode::Settings | Mode::Extras) {
            order_library(items, persisted, *mode);
        }
    }
    modes
}

pub fn setting_root_items() -> Vec<LibraryItem> {
    crate::model::SettingsGroup::ALL
        .into_iter()
        .map(|group| {
            let mut item = LibraryItem::simple(
                format!("group:{}", group.title().to_ascii_lowercase()),
                group.title(),
                Action::Group(group),
            );
            item.subtitle = group.subtitle().to_owned();
            item
        })
        .collect()
}

pub fn settings_group_items(
    group: crate::model::SettingsGroup,
    settings: &Settings,
    persisted: &PersistentState,
) -> Vec<LibraryItem> {
    use crate::model::SettingsGroup;
    match group {
        SettingsGroup::Appearance => setting_items(settings),
        SettingsGroup::Controller => controller_items(settings),
        SettingsGroup::System => system_control_items(settings, persisted),
        SettingsGroup::Power => power_items(),
    }
}

pub struct DiscoveredApp {
    pub item: LibraryItem,
    pub steam: bool,
    pub steam_shortcut: Option<u32>,
    /// The tab this application belongs in; Apps when nothing fits.
    pub home: Mode,
}

/// Where a desktop application lives, from its freedesktop categories.
/// Anything without a clear game or media role stays in Apps: a game
/// *utility* (mod manager, server monitor) is a tool, and an office
/// document viewer is not a photo viewer.
pub fn classify_application(categories: &[&str]) -> Mode {
    let has = |name: &str| categories.contains(&name);
    if has("Game") && !has("Utility") {
        Mode::Game
    } else if has("Music") || (has("Audio") && !has("Video")) {
        Mode::Music
    } else if has("Video") || has("AudioVideo") {
        Mode::Video
    } else if has("Graphics") && !has("Office") && (has("Viewer") || has("Photography")) {
        Mode::Photo
    } else {
        Mode::Apps
    }
}

pub fn discover_applications() -> Vec<DiscoveredApp> {
    let locales = get_languages_from_env();
    let desktops = current_desktop().unwrap_or_default();
    let mut seen = HashSet::new();
    let mut applications = Vec::new();

    for path in freedesktop_desktop_entry::Iter::new(default_paths()) {
        let Ok(entry) = DesktopEntry::from_path(&path, Some(&locales)) else {
            continue;
        };
        if !seen.insert(entry.id().to_ascii_lowercase()) || !entry_is_visible(&entry, &desktops) {
            continue;
        }
        // The launcher's own desktop entry exists so Walker can start it;
        // it has no business listing itself.
        if entry.id().eq_ignore_ascii_case("xmb-launcher") {
            continue;
        }
        let Some(name) = entry.name(&locales).map(|name| name.into_owned()) else {
            continue;
        };
        let subtitle = entry
            .generic_name(&locales)
            .or_else(|| entry.comment(&locales))
            .map(|value| value.into_owned())
            .unwrap_or_else(|| "Desktop application".to_owned());
        let art = entry.icon().and_then(resolve_icon);
        let exec = entry.exec().unwrap_or_default().to_owned();
        let steam_shortcut = steam_shortcut_app_id(&exec);
        let steam = steam_shortcut.is_some()
            || entry.id().to_ascii_lowercase().contains("steam")
            || name.to_ascii_lowercase().contains("steam")
            || exec.to_ascii_lowercase().contains("steam");
        let home = classify_application(&entry.categories().unwrap_or_default());
        let mut item = LibraryItem::simple(
            format!("application:{}", entry.id().to_ascii_lowercase()),
            name,
            Action::Desktop(path.clone()),
        );
        item.subtitle = subtitle;
        item.details = vec![entry.id().to_owned(), path.display().to_string()];
        item.art = art;
        applications.push(DiscoveredApp {
            item,
            steam,
            steam_shortcut,
            home,
        });
    }

    applications.sort_by(|left, right| natural_title_cmp(&left.item.title, &right.item.title));
    applications
}

/// Steam-created game shortcuts run `steam steam://rungameid/<app id>`.
pub fn steam_shortcut_app_id(exec: &str) -> Option<u32> {
    let start = exec.find("steam://rungameid/")? + "steam://rungameid/".len();
    let digits: String = exec[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

pub fn discover_games() -> Vec<LibraryItem> {
    let Some(steam_root) = steam_root() else {
        return Vec::new();
    };
    let mut libraries = vec![steam_root.clone()];
    let library_file = steam_root.join("steamapps/libraryfolders.vdf");
    if let Ok(contents) = fs::read_to_string(library_file)
        && let Ok(vdf) = steam_vdf_parser::parse_text(&contents)
        && let Some(folders) = vdf.as_obj()
    {
        for value in folders.values() {
            if let Some(path) = value.get_str(&["path"]) {
                libraries.push(PathBuf::from(path));
            }
        }
    }
    libraries.sort();
    libraries.dedup();

    let artwork_root = steam_root.join("appcache/librarycache");
    let playtimes = steam_playtimes(&steam_root);
    let mut seen = HashSet::new();
    let mut games = Vec::new();
    for library in libraries {
        let Ok(entries) = fs::read_dir(library.join("steamapps")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("appmanifest_")
                || path.extension().and_then(|e| e.to_str()) != Some("acf")
            {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(vdf) = steam_vdf_parser::parse_text(&contents) else {
                continue;
            };
            let Some(app_id) = vdf
                .get_str(&["appid"])
                .and_then(|id| id.parse::<u32>().ok())
            else {
                continue;
            };
            if !seen.insert(app_id) {
                continue;
            }
            let title = vdf.get_str(&["name"]).unwrap_or("Unknown Steam title");
            if steam_tool(title, app_id) {
                continue;
            }
            let art_dir = artwork_root.join(app_id.to_string());
            let cached_cover = steam_cover_cache_path(app_id).filter(|path| path.is_file());
            let art = first_file(&[
                art_dir.join("library_600x900.jpg"),
                art_dir.join("header.jpg"),
                art_dir.join("logo.png"),
            ])
            .or_else(|| cached_cover.clone());
            let hero = first_file(&[
                art_dir.join("library_hero.jpg"),
                art_dir.join("library_hero_blur.jpg"),
                art_dir.join("header.jpg"),
            ])
            .or(cached_cover);
            let (minutes, recorded) = playtimes.get(&app_id).copied().unwrap_or((0, 0));
            let last_played = vdf
                .get_str(&["LastPlayed"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default()
                .max(recorded);
            let size = vdf
                .get_str(&["SizeOnDisk"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default();
            let mut item =
                LibraryItem::simple(format!("steam:{app_id}"), title, Action::Steam(app_id));
            item.subtitle = play_summary(minutes, last_played);
            item.last_played = last_played;
            item.details = vec![
                format!("Steam App ID {app_id}"),
                format!("{} installed", human_size(size)),
                path.display().to_string(),
            ];
            if minutes > 0 {
                item.details
                    .insert(1, format!("{} played", human_minutes(minutes)));
            }
            item.art = art;
            item.hero = hero;
            games.push(item);
        }
    }
    games.sort_by(|left, right| natural_title_cmp(&left.title, &right.title));
    games
}

pub fn network_items() -> Vec<LibraryItem> {
    // Network configuration lives under Settings → System; the category
    // keeps the browser, like the original home menu. The row identifies
    // the user's actual default browser with its real name and icon.
    if let Some(browser) = resolved_default_browser() {
        return vec![browser];
    }
    let mut browser = LibraryItem::simple(
        "network:browser",
        "Internet Browser",
        Action::Network(NetworkAction::OpenBrowser),
    );
    browser.subtitle = system_status()
        .network
        .map(|status| format!("{status} — open the default browser"))
        .unwrap_or_else(|| "Open the default browser".to_owned());
    if !command_exists("xdg-open") {
        browser = browser.unavailable("xdg-open is unavailable");
    }
    vec![browser]
}

fn resolved_default_browser() -> Option<LibraryItem> {
    let output = Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let id = id.trim_end_matches(".desktop");
    if id.is_empty() {
        return None;
    }
    let locales = get_languages_from_env();
    for path in freedesktop_desktop_entry::Iter::new(default_paths()) {
        let Ok(entry) = DesktopEntry::from_path(&path, Some(&locales)) else {
            continue;
        };
        if !entry.id().eq_ignore_ascii_case(id) {
            continue;
        }
        let name = entry.name(&locales)?.into_owned();
        let mut item = LibraryItem::simple("network:browser", name, Action::Desktop(path.clone()));
        item.subtitle = system_status()
            .network
            .map(|status| format!("{status} — default browser"))
            .unwrap_or_else(|| "Default browser".to_owned());
        item.art = entry.icon().and_then(resolve_icon);
        item.details = vec![entry.id().to_owned(), path.display().to_string()];
        return Some(item);
    }
    None
}

fn wifi_item() -> LibraryItem {
    let status = system_status()
        .network
        .unwrap_or_else(|| "Offline".to_owned());
    let mut wifi = LibraryItem::simple("network:wifi", "Wi-Fi Networks", Action::Wifi);
    wifi.subtitle = status;
    if !crate::wifi::available() {
        wifi = wifi.unavailable("iwd (iwctl) was not found");
    }
    wifi
}

fn network_settings_item() -> LibraryItem {
    let mut connection = LibraryItem::simple(
        "network:connections",
        "Advanced Network Settings",
        Action::Network(NetworkAction::OpenConnections),
    );
    connection.subtitle = "External network tool".to_owned();
    if !command_exists("nm-connection-editor") && !command_exists("gnome-control-center") {
        connection = connection.unavailable("no graphical network settings tool was found");
    }
    connection
}

pub fn media_control_items(now: Option<&NowPlaying>) -> Vec<LibraryItem> {
    let Some(now) = now else {
        return Vec::new();
    };
    [
        ("media:previous", "Previous", MediaControl::Previous),
        (
            "media:play-pause",
            now.status.as_str(),
            MediaControl::PlayPause,
        ),
        ("media:next", "Next", MediaControl::Next),
    ]
    .into_iter()
    .map(|(id, title, control)| {
        let mut item = LibraryItem::simple(id, title, Action::Media(control));
        item.subtitle = format!("{} — {} · {}", now.artist, now.title, now.player);
        item.art = now.art.clone();
        item
    })
    .collect()
}

pub fn now_playing() -> Option<NowPlaying> {
    let finder = mpris::PlayerFinder::new().ok()?;
    let player = finder.find_active().ok()?;
    let metadata = player.get_metadata().ok()?;
    let art = metadata
        .art_url()
        .and_then(|url| url.strip_prefix("file://"))
        .map(percent_decode_path)
        .filter(|path| path.is_file());
    Some(NowPlaying {
        player: player.identity().to_owned(),
        title: metadata.title().unwrap_or("Unknown track").to_owned(),
        artist: metadata
            .artists()
            .map(|artists| artists.join(", "))
            .unwrap_or_else(|| "Unknown artist".to_owned()),
        status: format!("{:?}", player.get_playback_status().ok()?),
        art,
    })
}

pub fn system_status() -> SystemStatus {
    SystemStatus {
        battery: battery_status(),
        network: network_status(),
        volume: volume_status(),
    }
}

fn battery_status() -> Option<String> {
    let supplies = fs::read_dir("/sys/class/power_supply").ok()?;
    for supply in supplies.flatten() {
        let path = supply.path();
        let kind = fs::read_to_string(path.join("type")).ok()?;
        if kind.trim() != "Battery" {
            continue;
        }
        let capacity = fs::read_to_string(path.join("capacity"))
            .ok()?
            .trim()
            .parse::<u8>()
            .ok()?;
        let charging =
            fs::read_to_string(path.join("status")).is_ok_and(|status| status.trim() == "Charging");
        return Some(format!(
            "{} {capacity}%",
            if charging { "BAT+" } else { "BAT" }
        ));
    }
    None
}

fn network_status() -> Option<String> {
    let interfaces = fs::read_dir("/sys/class/net").ok()?;
    let mut wired = false;
    for interface in interfaces.flatten() {
        let path = interface.path();
        if interface.file_name() == "lo"
            || fs::read_to_string(path.join("operstate")).ok()?.trim() != "up"
        {
            continue;
        }
        if path.join("wireless").exists() {
            return Some("WI-FI".to_owned());
        }
        wired = true;
    }
    wired.then(|| "ETH".to_owned())
}

fn volume_status() -> Option<String> {
    if command_exists("wpctl") {
        let output = Command::new("wpctl")
            .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
            .output()
            .ok()?;
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if text.contains("MUTED") {
                return Some("MUTE".to_owned());
            }
            let level = text
                .split_whitespace()
                .find_map(|part| part.parse::<f32>().ok())?;
            return Some(format!("VOL {:.0}%", level * 100.0));
        }
    }
    None
}

pub fn system_control_items(settings: &Settings, persisted: &PersistentState) -> Vec<LibraryItem> {
    let mut items = vec![wifi_item(), network_settings_item(), audio_output_item()];
    add_control(
        &mut items,
        "volume-down",
        "Volume −",
        "Audio output",
        SystemAction::VolumeDown,
        command_exists("wpctl") || command_exists("pactl"),
        "wpctl or pactl is unavailable",
    );
    add_control(
        &mut items,
        "volume-up",
        "Volume +",
        "Audio output",
        SystemAction::VolumeUp,
        command_exists("wpctl") || command_exists("pactl"),
        "wpctl or pactl is unavailable",
    );
    let brightness = command_exists("brightnessctl") && backlight_available();
    add_control(
        &mut items,
        "brightness-down",
        "Brightness −",
        "Display",
        SystemAction::BrightnessDown,
        brightness,
        "no controllable backlight was found",
    );
    add_control(
        &mut items,
        "brightness-up",
        "Brightness +",
        "Display",
        SystemAction::BrightnessUp,
        brightness,
        "no controllable backlight was found",
    );
    let profiles = command_exists("powerprofilesctl");
    add_control(
        &mut items,
        "power-saver",
        "Power saver",
        "Power profile",
        SystemAction::PowerSaver,
        profiles,
        "powerprofilesctl is unavailable",
    );
    add_control(
        &mut items,
        "balanced",
        "Balanced",
        "Power profile",
        SystemAction::Balanced,
        profiles,
        "powerprofilesctl is unavailable",
    );
    add_control(
        &mut items,
        "performance",
        "Performance",
        "Power profile",
        SystemAction::Performance,
        profiles,
        "powerprofilesctl is unavailable",
    );
    let mut clock = LibraryItem::simple(
        "setting:clock",
        "Clock format",
        Action::Setting(SettingAction::Clock24h),
    );
    clock.subtitle = if settings.clock_24h {
        "24-hour"
    } else {
        "12-hour"
    }
    .to_owned();
    items.push(clock);
    let hidden = persisted.hidden.len();
    let mut restore = LibraryItem::simple(
        "setting:restore-hidden",
        "Hidden applications",
        Action::Setting(SettingAction::RestoreHidden),
    );
    restore.subtitle = match hidden {
        0 => "None hidden".to_owned(),
        1 => "1 hidden — restore".to_owned(),
        count => format!("{count} hidden — restore all"),
    };
    if hidden == 0 {
        restore = restore.unavailable("Hide an application from its △ Options menu");
    }
    items.push(restore);
    let mut info = LibraryItem::simple(
        "system:information",
        "System Information",
        Action::SystemInfo,
    );
    info.subtitle = "About this machine and launcher".to_owned();
    items.push(info);
    items
}

pub fn power_items() -> Vec<LibraryItem> {
    let mut items = Vec::new();
    add_control(
        &mut items,
        "lock",
        "Lock",
        "Session",
        SystemAction::Lock,
        command_exists("loginctl") || command_exists("hyprlock"),
        "no lock backend was found",
    );
    add_control(
        &mut items,
        "logout",
        "Log out",
        "Hold to confirm",
        SystemAction::Logout,
        command_exists("loginctl") || command_exists("hyprctl"),
        "no session backend was found",
    );
    add_control(
        &mut items,
        "suspend",
        "Suspend",
        "Hold to confirm",
        SystemAction::Suspend,
        command_exists("systemctl"),
        "systemctl is unavailable",
    );
    add_control(
        &mut items,
        "reboot",
        "Restart",
        "Hold to confirm",
        SystemAction::Reboot,
        command_exists("systemctl"),
        "systemctl is unavailable",
    );
    add_control(
        &mut items,
        "shutdown",
        "Power off",
        "Hold to confirm",
        SystemAction::Shutdown,
        command_exists("systemctl"),
        "systemctl is unavailable",
    );
    // Last on purpose: an accidental double-confirm entering this group
    // must not dismiss the launcher. In kiosk mode there is nothing behind
    // the launcher, so closing it is not offered at all.
    if !crate::config::is_kiosk() {
        let mut close = LibraryItem::simple("system:exit", "Close overlay", Action::Exit);
        close.subtitle = "Exit this launcher".to_owned();
        items.push(close);
    }
    items
}

pub fn setting_items(settings: &Settings) -> Vec<LibraryItem> {
    let definitions = [
        (
            "theme",
            "Theme",
            format!("{} / 12", settings.theme + 1),
            SettingAction::Theme,
        ),
        (
            "accent",
            "Wave accent",
            crate::model::ACCENT_NAMES[settings.accent.min(crate::model::ACCENT_NAMES.len() - 1)]
                .to_owned(),
            SettingAction::Accent,
        ),
        (
            "theme-pack",
            "Theme pack",
            settings
                .theme_pack
                .clone()
                .unwrap_or_else(|| "Built-in".to_owned()),
            SettingAction::ThemePack,
        ),
        (
            "background-mode",
            "Background style",
            match settings.background_mode.as_str() {
                "picture" => "Picture",
                _ => "Monthly gradient",
            }
            .to_owned(),
            SettingAction::BackgroundMode,
        ),
        (
            "background",
            "Background picture",
            settings
                .background_image
                .as_deref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Monthly gradient".to_owned()),
            SettingAction::Background,
        ),
        (
            "default-background",
            "Default background",
            settings
                .background_image
                .as_deref()
                .and_then(default_background_name)
                .unwrap_or_else(|| "None".to_owned()),
            SettingAction::DefaultBackground,
        ),
        (
            "transparent",
            "Transparent overlay",
            on_off(settings.transparent),
            SettingAction::Transparent,
        ),
        (
            "waves",
            "Animated waves",
            on_off(settings.waves),
            SettingAction::Waves,
        ),
        (
            "sparkles",
            "Wave sparkles",
            on_off(settings.sparkles),
            SettingAction::Sparkles,
        ),
        (
            "starfield",
            "Starfield",
            on_off(settings.starfield),
            SettingAction::Starfield,
        ),
        (
            "comets",
            "Comets",
            on_off(settings.comets),
            SettingAction::Comets,
        ),
        (
            "grid-floor",
            "Grid floor",
            on_off(settings.grid_floor),
            SettingAction::GridFloor,
        ),
        (
            "sound",
            "Interface sound",
            on_off(settings.sound),
            SettingAction::Sound,
        ),
        (
            "sound-volume",
            "Sound volume",
            format!("{:.0}%", settings.sound_volume * 100.0),
            SettingAction::SoundVolume,
        ),
        (
            "boot-animation",
            "Boot animation",
            on_off(settings.boot_animation),
            SettingAction::BootAnimation,
        ),
        (
            "idle-clock",
            "Idle clock",
            idle_clock_label(settings.idle_clock_minutes),
            SettingAction::IdleClock,
        ),
        (
            "reduced-motion",
            "Reduced motion",
            on_off(settings.reduced_motion),
            SettingAction::ReducedMotion,
        ),
        (
            "network-art",
            "Fetch missing Steam artwork",
            on_off(settings.network_artwork),
            SettingAction::NetworkArtwork,
        ),
        (
            "reset",
            "Reset appearance",
            "Restore visual defaults".to_owned(),
            SettingAction::ResetAppearance,
        ),
    ];
    definitions
        .into_iter()
        .map(|(id, title, subtitle, action)| {
            let mut item =
                LibraryItem::simple(format!("setting:{id}"), title, Action::Setting(action));
            item.subtitle = subtitle;
            item
        })
        .collect()
}

/// If `path` is one of the generated default wallpapers, its display name.
fn default_background_name(path: &std::path::Path) -> Option<String> {
    let file = path.file_name()?.to_str()?;
    crate::backgrounds::list()
        .into_iter()
        .find(|wallpaper| wallpaper.path.file_name().and_then(|n| n.to_str()) == Some(file))
        .map(|wallpaper| wallpaper.name)
}

pub fn controller_items(settings: &Settings) -> Vec<LibraryItem> {
    let has_controller = settings_controller_present();
    let mut items: Vec<LibraryItem> = crate::model::BindingTarget::ALL
        .into_iter()
        .map(|target| {
            let mut item = LibraryItem::simple(
                format!(
                    "setting:binding-{}",
                    target.label().to_ascii_lowercase().replace(' ', "-")
                ),
                target.label(),
                Action::Setting(SettingAction::Binding(target)),
            );
            item.subtitle = format!(
                "{} — press to rebind",
                pretty_button(settings.controller.get(target))
            );
            item
        })
        .collect();

    let tuning: [(&str, &str, String, SettingAction); 4] = [
        (
            "stick-deadzone",
            "Stick deadzone",
            format!("{:.0}%", settings.stick_deadzone * 100.0),
            SettingAction::StickDeadzone,
        ),
        (
            "nav-repeat",
            "Scroll repeat",
            nav_repeat_label(settings.nav_repeat_ms),
            SettingAction::NavRepeat,
        ),
        (
            "rumble",
            "Rumble",
            on_off(settings.rumble),
            SettingAction::Rumble,
        ),
        (
            "rumble-strength",
            "Rumble strength",
            format!("{:.0}%", settings.rumble_strength * 100.0),
            SettingAction::RumbleStrength,
        ),
    ];
    for (id, title, subtitle, action) in tuning {
        let mut item = LibraryItem::simple(format!("setting:{id}"), title, Action::Setting(action));
        item.subtitle = subtitle;
        items.push(item);
    }

    let mut test = LibraryItem::simple(
        "setting:test-controller",
        "Test controller",
        Action::Setting(SettingAction::TestController),
    );
    test.subtitle = if has_controller {
        settings_controller_name().unwrap_or_else(|| "Show inputs".to_owned())
    } else {
        "No controller connected".to_owned()
    };
    if !has_controller {
        test = test.unavailable("Connect a controller to test it");
    }
    items.push(test);

    let mut reset = LibraryItem::simple(
        "setting:reset-controller",
        "Reset controller",
        Action::Setting(SettingAction::ResetController),
    );
    reset.subtitle = "Restore default mappings".to_owned();
    items.push(reset);
    items
}

pub fn nav_repeat_label(ms: u16) -> String {
    match ms {
        0 => "Off".to_owned(),
        m if m >= 300 => "Slow".to_owned(),
        m if m >= 180 => "Medium".to_owned(),
        _ => "Fast".to_owned(),
    }
}

/// Human-friendly button name for the controller rows.
pub fn pretty_button(button: &str) -> String {
    match button {
        "south" => "A / ✕",
        "east" => "B / ○",
        "north" => "Y / △",
        "west" => "X / □",
        "dpad-left" => "D-pad Left",
        "dpad-right" => "D-pad Right",
        "dpad-up" => "D-pad Up",
        "dpad-down" => "D-pad Down",
        "left-shoulder" => "L1 / LB",
        "right-shoulder" => "R1 / RB",
        "start" => "Start",
        other => other,
    }
    .to_owned()
}

fn settings_controller_present() -> bool {
    settings_controller_name().is_some()
}

/// The connected controller's name, if any (best-effort, cheap gilrs probe).
fn settings_controller_name() -> Option<String> {
    let gilrs = gilrs::Gilrs::new().ok()?;
    gilrs
        .gamepads()
        .find(|(_, pad)| pad.is_connected())
        .map(|(_, pad)| pad.name().to_owned())
}

pub fn order_library(items: &mut [LibraryItem], persisted: &PersistentState, mode: Mode) {
    items.sort_by(|left, right| {
        // Game: Continue row, then the Steam client, then other launchers;
        // media tabs: applications, then folders, then files — all above
        // favorites and recents.
        let pinned = |item: &LibraryItem| -> u8 {
            match mode {
                Mode::Game if item.id == "game:continue" => 4,
                Mode::Game if item.id == "application:steam" => 3,
                Mode::Game | Mode::Music | Mode::Video | Mode::Photo
                    if item.id.starts_with("application:") =>
                {
                    2
                }
                Mode::Music | Mode::Video | Mode::Photo
                    if matches!(item.action, Action::Folder(_)) =>
                {
                    1
                }
                _ => 0,
            }
        };
        let left_pinned = pinned(left);
        let right_pinned = pinned(right);
        let left_favorite = persisted.favorites.contains(&left.id);
        let right_favorite = persisted.favorites.contains(&right.id);
        let left_recent = persisted.recent_position(mode, &left.id);
        let right_recent = persisted.recent_position(mode, &right.id);
        right_pinned
            .cmp(&left_pinned)
            .then_with(|| right_favorite.cmp(&left_favorite))
            .then_with(|| match (left_recent, right_recent) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => natural_title_cmp(&left.title, &right.title),
            })
    });
}

pub fn entry_is_visible(entry: &DesktopEntry, desktops: &[String]) -> bool {
    if entry.hidden()
        || entry.no_display()
        || entry.type_() != Some("Application")
        || (entry.exec().is_none() && !entry.dbus_activatable())
    {
        return false;
    }
    if let Some(only) = entry.only_show_in()
        && !only
            .iter()
            .any(|allowed| desktop_matches(allowed, desktops))
    {
        return false;
    }
    if let Some(excluded) = entry.not_show_in()
        && excluded
            .iter()
            .any(|denied| desktop_matches(denied, desktops))
    {
        return false;
    }
    entry.try_exec().is_none_or(command_exists)
}

pub fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file();
    }
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|directory| directory.join(command).is_file())
    })
}

fn desktop_matches(candidate: &str, desktops: &[String]) -> bool {
    desktops
        .iter()
        .any(|desktop| candidate.eq_ignore_ascii_case(desktop))
}

fn resolve_icon(icon: &str) -> Option<PathBuf> {
    let expanded = if let Some(rest) = icon.strip_prefix("~/") {
        env::var_os("HOME").map(PathBuf::from)?.join(rest)
    } else {
        PathBuf::from(icon)
    };
    if expanded.is_absolute() && expanded.is_file() {
        return Some(expanded);
    }
    freedesktop_icons::lookup(icon)
        .with_size(128)
        .with_cache()
        .find()
}

/// The launcher's own extras: things that are not applications.
pub fn extras_items() -> Vec<LibraryItem> {
    let mut clock = LibraryItem::simple("extras:clock", "Clock", Action::Clock);
    clock.subtitle = "Full-screen clock and date".to_owned();
    let mut info = LibraryItem::simple(
        "system:information",
        "System Information",
        Action::SystemInfo,
    );
    info.subtitle = "About this machine and launcher".to_owned();
    let mut screen = LibraryItem::simple(
        "extras:screen-off",
        "Screen Off",
        Action::System(SystemAction::ScreenOff),
    );
    screen.subtitle = "Displays off until a button is pressed".to_owned();
    if !command_exists("hyprctl") {
        screen = screen.unavailable("needs Hyprland");
    }
    let mut help = LibraryItem::simple("extras:help", "Controls & Help", Action::Help);
    help.subtitle = "Show every control (or press H)".to_owned();
    vec![help, clock, info, screen]
}

/// The quick-resume row: the game launched most recently from here, else
/// the Steam game with the newest last-played time.
pub fn continue_item(games: &[LibraryItem], persisted: &PersistentState) -> Option<LibraryItem> {
    let playable = |item: &&LibraryItem| {
        item.id != "game:continue" && item.id != "application:steam" && item.available
    };
    let recent = persisted.recents.get("game").and_then(|ids| {
        ids.iter()
            .find_map(|id| games.iter().filter(playable).find(|game| &game.id == id))
    });
    let target = recent.or_else(|| {
        games
            .iter()
            .filter(playable)
            .filter(|game| game.last_played > 0)
            .max_by_key(|game| game.last_played)
    })?;
    let mut item = LibraryItem::simple(
        "game:continue",
        format!("Continue: {}", target.title),
        target.action.clone(),
    );
    item.subtitle = target.subtitle.clone();
    item.details = target.details.clone();
    item.art = target.art.clone();
    item.hero = target.hero.clone();
    item.alias_of = Some(target.id.clone());
    Some(item)
}

/// Minutes played and last-played time per app id, merged across every
/// Steam account's localconfig.vdf.
fn steam_playtimes(steam_root: &Path) -> HashMap<u32, (u64, u64)> {
    let mut playtimes = HashMap::new();
    let Ok(users) = fs::read_dir(steam_root.join("userdata")) else {
        return playtimes;
    };
    for user in users.flatten() {
        let Ok(contents) = fs::read_to_string(user.path().join("config/localconfig.vdf")) else {
            continue;
        };
        let Ok(vdf) = steam_vdf_parser::parse_text(&contents) else {
            continue;
        };
        let Some(apps) = vdf.as_obj().and_then(|root| {
            ["Software", "Valve", "Steam", "apps"]
                .iter()
                .try_fold(root, |obj, key| vdf_child(obj, key))
        }) else {
            continue;
        };
        for (id, value) in apps.keys().zip(apps.values()) {
            let (Ok(app_id), Some(app)) = (id.parse::<u32>(), value.as_obj()) else {
                continue;
            };
            let number = |key: &str| {
                app.get(key)
                    .and_then(|value| value.as_str())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            let entry = playtimes.entry(app_id).or_insert((0, 0));
            entry.0 = entry.0.max(number("Playtime"));
            entry.1 = entry.1.max(number("LastPlayed"));
        }
    }
    playtimes
}

/// Case-insensitive child object lookup (Steam mixes key casing).
fn vdf_child<'a, 't>(
    obj: &'a steam_vdf_parser::Obj<'t>,
    key: &str,
) -> Option<&'a steam_vdf_parser::Obj<'t>> {
    obj.keys()
        .zip(obj.values())
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .and_then(|(_, value)| value.as_obj())
}

fn play_summary(minutes: u64, last_played: u64) -> String {
    match (minutes, last_played) {
        (0, 0) => "Installed Steam game".to_owned(),
        (0, when) => format!("Last played {}", format_timestamp(when)),
        (played, 0) => format!("{} played", human_minutes(played)),
        (played, when) => format!(
            "{} played · Last {}",
            human_minutes(played),
            format_timestamp(when)
        ),
    }
}

fn human_minutes(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

fn idle_clock_label(minutes: u8) -> String {
    if minutes == 0 {
        "Off".to_owned()
    } else {
        format!("After {minutes} min")
    }
}

pub struct AudioSink {
    pub id: u32,
    pub name: String,
    pub default: bool,
}

fn audio_output_item() -> LibraryItem {
    let mut item = LibraryItem::simple("audio:output", "Audio Output", Action::AudioOutput);
    if !command_exists("wpctl") {
        item.subtitle = "—".to_owned();
        return item.unavailable("wpctl (WirePlumber) is unavailable");
    }
    item.subtitle = audio_sinks()
        .into_iter()
        .find(|sink| sink.default)
        .map(|sink| sink.name)
        .unwrap_or_else(|| "No output".to_owned());
    item
}

pub fn audio_sinks() -> Vec<AudioSink> {
    let Ok(output) = Command::new("wpctl").arg("status").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_wpctl_sinks(&String::from_utf8_lossy(&output.stdout))
}

/// The "Sinks:" block of `wpctl status`: `*` marks the default, then
/// `<id>. <name> [vol: x]`.
fn parse_wpctl_sinks(text: &str) -> Vec<AudioSink> {
    let mut sinks = Vec::new();
    let mut in_sinks = false;
    for line in text.lines() {
        let trimmed = line.trim_matches(|c: char| c.is_whitespace() || "│├└─".contains(c));
        if trimmed.starts_with("Sinks:") {
            in_sinks = true;
            continue;
        }
        if !in_sinks {
            continue;
        }
        if trimmed.ends_with(':') || trimmed.starts_with("Video") {
            break;
        }
        let default = trimmed.starts_with('*');
        let rest = trimmed.trim_start_matches('*').trim();
        let Some((id, name)) = rest.split_once(". ") else {
            continue;
        };
        let Ok(id) = id.trim().parse::<u32>() else {
            continue;
        };
        let name = name
            .split(" [vol:")
            .next()
            .unwrap_or(name)
            .trim()
            .to_owned();
        sinks.push(AudioSink { id, name, default });
    }
    sinks
}

pub fn set_audio_sink(id: u32) -> bool {
    Command::new("wpctl")
        .args(["set-default", &id.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn steam_cover_cache_path(app_id: u32) -> Option<PathBuf> {
    Some(
        crate::config::paths()?
            .cache
            .join("steam-art")
            .join(format!("{app_id}.jpg")),
    )
}

/// Download boxart for a Steam title that has no local artwork yet. Results
/// are cached; the overlay only calls this when "Fetch missing Steam
/// artwork" is enabled.
pub fn fetch_steam_cover(app_id: u32) -> Option<PathBuf> {
    let output = steam_cover_cache_path(app_id)?;
    if output.is_file() {
        return Some(output);
    }
    fs::create_dir_all(output.parent()?).ok()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build()
        .into();
    for variant in ["library_600x900.jpg", "header.jpg"] {
        let url = format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/{variant}");
        let Ok(mut response) = agent.get(&url).call() else {
            continue;
        };
        let Ok(bytes) = response.body_mut().read_to_vec() else {
            continue;
        };
        if bytes.len() < 512 {
            continue;
        }
        if fs::write(&output, &bytes).is_ok() {
            return Some(output);
        }
    }
    None
}

fn steam_root() -> Option<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME")?);
    [home.join(".local/share/Steam"), home.join(".steam/steam")]
        .into_iter()
        .find(|path| path.join("steamapps").is_dir())
}

/// Steam entries that are not games: runtimes, redistributables, servers,
/// and Wallpaper Engine (431960), which is software sold through Steam.
fn steam_tool(title: &str, app_id: u32) -> bool {
    let lower = title.to_ascii_lowercase();
    app_id == 228980
        || app_id == 431960
        || lower == "wallpaper engine"
        || lower.contains("steam linux runtime")
        || lower.starts_with("proton ")
        || lower.contains("steamworks common redistributables")
        || lower.contains("pressure-vessel")
        || lower.contains("dedicated server")
}

fn first_file(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.is_file()).cloned()
}

fn add_control(
    items: &mut Vec<LibraryItem>,
    id: &str,
    title: &str,
    subtitle: &str,
    action: SystemAction,
    available: bool,
    reason: &str,
) {
    let mut item = LibraryItem::simple(format!("system:{id}"), title, Action::System(action));
    item.subtitle = subtitle.to_owned();
    if !available {
        item = item.unavailable(reason);
    }
    items.push(item);
}

fn backlight_available() -> bool {
    fs::read_dir("/sys/class/backlight")
        .ok()
        .is_some_and(|mut entries| entries.next().is_some())
}

fn on_off(value: bool) -> String {
    if value { "On" } else { "Off" }.to_owned()
}

fn natural_title_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

fn format_timestamp(timestamp: u64) -> String {
    chrono::DateTime::from_timestamp(timestamp as i64, 0)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "recently".to_owned())
}

fn human_size(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB)
    }
}

fn percent_decode_path(value: &str) -> PathBuf {
    let mut bytes = Vec::with_capacity(value.len());
    let input = value.as_bytes();
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%'
            && index + 2 < input.len()
            && let Ok(hex) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            bytes.push(hex);
            index += 3;
        } else {
            bytes.push(input[index]);
            index += 1;
        }
    }
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(contents: &str) -> DesktopEntry {
        DesktopEntry::from_str("test.desktop", contents, None::<&[&str]>).unwrap()
    }

    #[test]
    fn desktop_visibility_respects_restrictions() {
        let allowed =
            entry("[Desktop Entry]\nType=Application\nName=A\nExec=true\nOnlyShowIn=Hyprland;");
        let denied =
            entry("[Desktop Entry]\nType=Application\nName=A\nExec=true\nNotShowIn=HYPRLAND;");
        assert!(entry_is_visible(&allowed, &["hyprland".into()]));
        assert!(!entry_is_visible(&denied, &["hyprland".into()]));
    }

    #[test]
    fn filters_steam_runtime_entries() {
        assert!(steam_tool("Steam Linux Runtime 3.0", 1));
        assert!(steam_tool("Steamworks Common Redistributables", 228980));
        assert!(steam_tool("Unturned Dedicated Server", 1110390));
        assert!(!steam_tool("Skyrim Special Edition", 489830));
    }

    #[test]
    fn settings_are_grouped() {
        use crate::model::SettingsGroup;
        let settings = Settings::default();
        let persisted = PersistentState::default();
        let root = setting_root_items();
        assert_eq!(root.len(), SettingsGroup::ALL.len());
        assert!(
            root.iter()
                .all(|item| matches!(item.action, Action::Group(_)))
        );
        let appearance = settings_group_items(SettingsGroup::Appearance, &settings, &persisted);
        assert!(
            appearance
                .iter()
                .any(|item| item.id == "setting:background")
        );
        let controller = settings_group_items(SettingsGroup::Controller, &settings, &persisted);
        // Binding rows come first, then the tuning/test/reset rows.
        assert!(controller[0].id.starts_with("setting:binding-"));
        assert!(controller.iter().any(|item| item.id == "setting:rumble"));
        assert!(
            controller
                .iter()
                .any(|item| item.id == "setting:reset-controller")
        );
        let power = settings_group_items(SettingsGroup::Power, &settings, &persisted);
        assert!(power.iter().any(|item| item.id == "system:shutdown"));
        assert!(!power.iter().any(|item| item.id == "system:volume-up"));
        assert_eq!(
            power.last().map(|item| item.id.as_str()),
            Some("system:exit")
        );
        let system = settings_group_items(SettingsGroup::System, &settings, &persisted);
        assert_eq!(
            system.first().map(|item| item.id.as_str()),
            Some("network:wifi")
        );
        assert_eq!(
            system.get(1).map(|item| item.id.as_str()),
            Some("network:connections")
        );
    }

    #[test]
    fn steam_shortcut_app_ids_parse_from_exec() {
        assert_eq!(
            steam_shortcut_app_id("steam steam://rungameid/377160"),
            Some(377160)
        );
        assert_eq!(steam_shortcut_app_id("/usr/bin/spotify %U"), None);
    }

    #[test]
    fn network_category_keeps_only_the_browser() {
        let items = network_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "network:browser");
    }

    #[test]
    fn steam_client_is_pinned_above_favorites() {
        let mut items = vec![
            LibraryItem::simple("steam:100", "Alpha Game", Action::Steam(100)),
            LibraryItem::simple(
                "application:steam",
                "Steam",
                Action::Setting(SettingAction::Theme),
            ),
            LibraryItem::simple("steam:200", "Beta Game", Action::Steam(200)),
        ];
        let mut state = PersistentState::default();
        state.favorites.insert("steam:200".into());
        order_library(&mut items, &state, Mode::Game);
        assert_eq!(items[0].id, "application:steam");
        assert_eq!(items[1].id, "steam:200");
    }

    #[test]
    fn background_subtitle_names_the_picture() {
        let settings = Settings {
            background_image: Some(PathBuf::from("/home/user/Pictures/sunset.jpg")),
            ..Settings::default()
        };
        let items = setting_items(&settings);
        let background = items
            .iter()
            .find(|item| item.id == "setting:background")
            .unwrap();
        assert_eq!(background.subtitle, "sunset.jpg");
    }

    #[test]
    fn applications_are_homed_by_category() {
        assert_eq!(classify_application(&["Game", "ActionGame"]), Mode::Game);
        assert_eq!(
            classify_application(&["Audio", "Music", "Player", "AudioVideo"]),
            Mode::Music
        );
        assert_eq!(
            classify_application(&["AudioVideo", "Audio", "Video", "Player", "TV"]),
            Mode::Video
        );
        assert_eq!(classify_application(&["AudioVideo", "Video"]), Mode::Video);
        assert_eq!(classify_application(&["Game", "Utility"]), Mode::Apps);
        assert_eq!(classify_application(&["Graphics", "Viewer"]), Mode::Photo);
        assert_eq!(
            classify_application(&["Graphics", "2DGraphics"]),
            Mode::Apps
        );
        assert_eq!(
            classify_application(&["Office", "Viewer", "Graphics", "2DGraphics"]),
            Mode::Apps
        );
        assert_eq!(classify_application(&["Utility"]), Mode::Apps);
        assert_eq!(classify_application(&[]), Mode::Apps);
        assert!(steam_tool("Wallpaper Engine", 0));
        assert!(steam_tool("Anything", 431960));
        assert!(!steam_tool("Unturned", 304930));
    }

    #[test]
    fn playtime_summaries_read_naturally() {
        assert_eq!(human_minutes(42), "42 min");
        assert_eq!(human_minutes(120), "2h");
        assert_eq!(human_minutes(19308), "321h 48m");
        assert_eq!(play_summary(0, 0), "Installed Steam game");
        assert!(play_summary(90, 0).starts_with("1h 30m played"));
        assert_eq!(idle_clock_label(0), "Off");
        assert_eq!(idle_clock_label(5), "After 5 min");
    }

    #[test]
    fn wpctl_sinks_parse_with_default_marker() {
        let text = "PipeWire 'pipewire-0' [1.4.7]\n\nAudio\n ├─ Devices:\n │      44. Navi 21/23 HDMI/DP Audio Controller [alsa]\n │  \n ├─ Sinks:\n │  *   52. Navi 21/23 HDMI/DP Audio Controller Digital Stereo (HDMI 4) [vol: 0.40]\n │      61. Starship/Matisse HD Audio Controller Analog Stereo [vol: 1.00 MUTED]\n │  \n ├─ Sources:\n │      70. Mic [vol: 1.00]\n";
        let sinks = parse_wpctl_sinks(text);
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[0].id, 52);
        assert!(sinks[0].default);
        assert_eq!(
            sinks[1].name,
            "Starship/Matisse HD Audio Controller Analog Stereo"
        );
        assert!(!sinks[1].default);
    }

    #[test]
    fn continue_row_prefers_launcher_recents_then_steam_history() {
        let mut older = LibraryItem::simple("steam:1", "Older", Action::Steam(1));
        older.last_played = 100;
        let mut newer = LibraryItem::simple("steam:2", "Newer", Action::Steam(2));
        newer.last_played = 200;
        let steam = LibraryItem::simple(
            "application:steam",
            "Steam",
            Action::Desktop(PathBuf::from("/s.desktop")),
        );
        let games = vec![steam, older, newer];
        let mut persisted = PersistentState::default();
        let resume = continue_item(&games, &persisted).unwrap();
        assert_eq!(resume.alias_of.as_deref(), Some("steam:2"));
        assert_eq!(resume.title, "Continue: Newer");
        persisted.record_recent(Mode::Game, "steam:1");
        let resume = continue_item(&games, &persisted).unwrap();
        assert_eq!(resume.alias_of.as_deref(), Some("steam:1"));
        let mut ordered = games.clone();
        ordered.insert(0, resume);
        order_library(&mut ordered, &persisted, Mode::Game);
        assert_eq!(ordered[0].id, "game:continue");
        assert_eq!(ordered[1].id, "application:steam");
    }

    #[test]
    fn favorites_then_recents_then_titles() {
        let mut items = vec![
            LibraryItem::simple("c", "Charlie", Action::Setting(SettingAction::Theme)),
            LibraryItem::simple("a", "Alpha", Action::Setting(SettingAction::Theme)),
            LibraryItem::simple("b", "Beta", Action::Setting(SettingAction::Theme)),
        ];
        let mut state = PersistentState::default();
        state.favorites.insert("c".into());
        state.record_recent(Mode::Extras, "b");
        order_library(&mut items, &state, Mode::Extras);
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
    }
}
