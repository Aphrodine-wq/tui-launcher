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
    modes.insert(Mode::Extras, discover_applications());
    modes.insert(
        Mode::Photo,
        discover_media(&settings.media_paths, MediaKind::Photo),
    );
    modes.insert(
        Mode::Music,
        discover_media(&settings.media_paths, MediaKind::Music),
    );
    modes.insert(
        Mode::Video,
        discover_media(&settings.media_paths, MediaKind::Video),
    );
    modes.insert(Mode::Game, discover_games());
    modes.insert(Mode::Network, network_items());
    for (mode, items) in &mut modes {
        // Settings keeps its curated order; favorites and recents would
        // scramble it every time a volume button is used.
        if *mode != Mode::Settings {
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
) -> Vec<LibraryItem> {
    use crate::model::SettingsGroup;
    match group {
        SettingsGroup::Appearance => setting_items(settings),
        SettingsGroup::Controller => controller_items(settings),
        SettingsGroup::System => system_control_items(),
        SettingsGroup::Power => power_items(),
    }
}

pub fn discover_applications() -> Vec<LibraryItem> {
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
        let Some(name) = entry.name(&locales).map(|name| name.into_owned()) else {
            continue;
        };
        let subtitle = entry
            .generic_name(&locales)
            .or_else(|| entry.comment(&locales))
            .map(|value| value.into_owned())
            .unwrap_or_else(|| "Desktop application".to_owned());
        let art = entry.icon().and_then(resolve_icon);
        let mut item = LibraryItem::simple(
            format!("application:{}", entry.id().to_ascii_lowercase()),
            name,
            Action::Desktop(path.clone()),
        );
        item.subtitle = subtitle;
        item.details = vec![entry.id().to_owned(), path.display().to_string()];
        item.art = art;
        applications.push(item);
    }

    applications.sort_by(|left, right| natural_title_cmp(&left.title, &right.title));
    applications
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
            let last_played = vdf
                .get_str(&["LastPlayed"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default();
            let size = vdf
                .get_str(&["SizeOnDisk"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default();
            let mut item =
                LibraryItem::simple(format!("steam:{app_id}"), title, Action::Steam(app_id));
            item.subtitle = if last_played == 0 {
                "Installed Steam game".to_owned()
            } else {
                format!("Last played {}", format_timestamp(last_played))
            };
            item.details = vec![
                format!("Steam App ID {app_id}"),
                format!("{} installed", human_size(size)),
                path.display().to_string(),
            ];
            item.art = art;
            item.hero = hero;
            games.push(item);
        }
    }
    games.sort_by(|left, right| natural_title_cmp(&left.title, &right.title));
    games
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind {
    Photo,
    Music,
    Video,
}

pub fn discover_media(roots: &[PathBuf], requested: MediaKind) -> Vec<LibraryItem> {
    let mut paths = Vec::new();
    for root in roots {
        walk_media(root, 0, &mut paths);
    }
    paths.sort_by(|left, right| {
        right
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .cmp(
                &left
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok(),
            )
            .then_with(|| left.cmp(right))
    });
    paths
        .into_iter()
        .filter_map(|path| {
            let title = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Media")
                .replace(['_', '-'], " ");
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let kind = if image_extension(&extension) {
                MediaKind::Photo
            } else if audio_extension(&extension) {
                MediaKind::Music
            } else {
                MediaKind::Video
            };
            if kind != requested {
                return None;
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            let mut item = LibraryItem::simple(
                format!("media:{}", canonical.display()),
                title,
                Action::Open(path.clone()),
            );
            item.subtitle = match kind {
                MediaKind::Photo => "Picture",
                MediaKind::Music => "Music",
                MediaKind::Video => "Video",
            }
            .to_owned();
            item.details = vec![path.display().to_string()];
            if kind == MediaKind::Photo {
                item.art = Some(path);
            }
            Some(item)
        })
        .collect()
}

pub fn network_items() -> Vec<LibraryItem> {
    let status = system_status()
        .network
        .unwrap_or_else(|| "Offline".to_owned());
    let mut connection = LibraryItem::simple(
        "network:connections",
        "Network Settings",
        Action::Network(NetworkAction::OpenConnections),
    );
    connection.subtitle = status;
    if !command_exists("nm-connection-editor") && !command_exists("gnome-control-center") {
        connection = connection.unavailable("no graphical network settings tool was found");
    }
    let mut browser = LibraryItem::simple(
        "network:browser",
        "Internet Browser",
        Action::Network(NetworkAction::OpenBrowser),
    );
    browser.subtitle = "Open the default browser".to_owned();
    if !command_exists("xdg-open") {
        browser = browser.unavailable("xdg-open is unavailable");
    }
    vec![connection, browser]
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

pub fn system_control_items() -> Vec<LibraryItem> {
    let mut items = Vec::new();
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
    items
}

pub fn power_items() -> Vec<LibraryItem> {
    let mut items = Vec::new();
    let mut close = LibraryItem::simple("system:exit", "Close overlay", Action::Exit);
    close.subtitle = "Exit this launcher".to_owned();
    items.push(close);
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
            "sound",
            "Interface sound",
            on_off(settings.sound),
            SettingAction::Sound,
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

pub fn controller_items(settings: &Settings) -> Vec<LibraryItem> {
    crate::model::BindingTarget::ALL
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
            item.subtitle = settings.controller.get(target).to_owned();
            item
        })
        .collect()
}

pub fn order_library(items: &mut [LibraryItem], persisted: &PersistentState, mode: Mode) {
    items.sort_by(|left, right| {
        let left_favorite = persisted.favorites.contains(&left.id);
        let right_favorite = persisted.favorites.contains(&right.id);
        let left_recent = persisted.recent_position(mode, &left.id);
        let right_recent = persisted.recent_position(mode, &right.id);
        right_favorite
            .cmp(&left_favorite)
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
        let url =
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/{variant}");
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

fn steam_tool(title: &str, app_id: u32) -> bool {
    let lower = title.to_ascii_lowercase();
    app_id == 228980
        || lower.contains("steam linux runtime")
        || lower.starts_with("proton ")
        || lower.contains("steamworks common redistributables")
        || lower.contains("pressure-vessel")
        || lower.contains("dedicated server")
}

fn first_file(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.is_file()).cloned()
}

fn walk_media(path: &Path, depth: usize, output: &mut Vec<PathBuf>) {
    if depth > 12 {
        return;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_file() {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if image_extension(&extension) || video_extension(&extension) || audio_extension(&extension)
        {
            output.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        walk_media(&entry.path(), depth + 1, output);
    }
}

fn image_extension(extension: &str) -> bool {
    matches!(extension, "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif")
}

fn video_extension(extension: &str) -> bool {
    matches!(extension, "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v")
}

fn audio_extension(extension: &str) -> bool {
    matches!(extension, "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus")
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
        let root = setting_root_items();
        assert_eq!(root.len(), SettingsGroup::ALL.len());
        assert!(
            root.iter()
                .all(|item| matches!(item.action, Action::Group(_)))
        );
        let appearance = settings_group_items(SettingsGroup::Appearance, &settings);
        assert!(appearance.iter().any(|item| item.id == "setting:background"));
        assert!(
            settings_group_items(SettingsGroup::Controller, &settings)
                .iter()
                .all(|item| item.id.starts_with("setting:binding-"))
        );
        let power = settings_group_items(SettingsGroup::Power, &settings);
        assert!(power.iter().any(|item| item.id == "system:shutdown"));
        assert!(!power.iter().any(|item| item.id == "system:volume-up"));
        assert_eq!(power.first().map(|item| item.id.as_str()), Some("system:exit"));
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
