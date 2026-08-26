use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::model::{BindingTarget, Mode};

pub const CONFIG_VERSION: u8 = 5;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub version: u8,
    pub neighbor_count: usize,
    pub icon_width: u16,
    pub icon_height: u16,
    pub show_index: bool,
    pub show_footer: bool,
    pub show_header: bool,
    pub transparent: bool,
    pub theme: usize,
    pub accent: usize,
    pub border_style: usize,
    pub animation_speed: usize,
    pub panel_width: u16,
    pub waves: bool,
    pub sound: bool,
    pub sound_volume: f32,
    pub rumble: bool,
    pub rumble_strength: f32,
    pub reduced_motion: bool,
    pub network_artwork: bool,
    pub default_fullscreen: bool,
    pub background_image: Option<PathBuf>,
    pub background_mode: String,
    pub theme_pack: Option<String>,
    pub sparkles: bool,
    pub boot_animation: bool,
    pub clock_24h: bool,
    pub media_paths: Vec<PathBuf>,
    pub controller: ControllerBindings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            neighbor_count: 2,
            icon_width: 22,
            icon_height: 11,
            show_index: true,
            show_footer: true,
            show_header: true,
            transparent: true,
            theme: 0,
            accent: 0,
            border_style: 0,
            animation_speed: 1,
            panel_width: 110,
            waves: true,
            sound: true,
            sound_volume: 0.22,
            rumble: false,
            rumble_strength: 0.55,
            reduced_motion: false,
            network_artwork: false,
            default_fullscreen: false,
            background_image: None,
            background_mode: "gradient".to_owned(),
            theme_pack: None,
            sparkles: true,
            boot_animation: true,
            clock_24h: false,
            media_paths: default_media_paths(),
            controller: ControllerBindings::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ControllerBindings {
    pub confirm: String,
    pub back: String,
    pub context: String,
    pub favorite: String,
    pub previous_mode: String,
    pub next_mode: String,
    pub previous_item: String,
    pub next_item: String,
    pub settings: String,
}

impl Default for ControllerBindings {
    fn default() -> Self {
        Self {
            confirm: "south".into(),
            back: "east".into(),
            context: "north".into(),
            favorite: "west".into(),
            previous_mode: "dpad-left".into(),
            next_mode: "dpad-right".into(),
            previous_item: "dpad-up".into(),
            next_item: "dpad-down".into(),
            settings: "start".into(),
        }
    }
}

impl ControllerBindings {
    const BUTTONS: [&'static str; 11] = [
        "south",
        "east",
        "north",
        "west",
        "dpad-left",
        "dpad-right",
        "dpad-up",
        "dpad-down",
        "left-shoulder",
        "right-shoulder",
        "start",
    ];

    pub fn get(&self, target: BindingTarget) -> &str {
        match target {
            BindingTarget::Confirm => &self.confirm,
            BindingTarget::Back => &self.back,
            BindingTarget::Context => &self.context,
            BindingTarget::Favorite => &self.favorite,
            BindingTarget::PreviousMode => &self.previous_mode,
            BindingTarget::NextMode => &self.next_mode,
            BindingTarget::PreviousItem => &self.previous_item,
            BindingTarget::NextItem => &self.next_item,
            BindingTarget::Settings => &self.settings,
        }
    }

    pub fn cycle(&mut self, target: BindingTarget) {
        let old = self.get(target).to_owned();
        let index = Self::BUTTONS
            .iter()
            .position(|button| *button == old)
            .unwrap_or(0);
        let next = Self::BUTTONS[(index + 1) % Self::BUTTONS.len()].to_owned();
        if let Some(other) = BindingTarget::ALL
            .into_iter()
            .find(|other| *other != target && self.get(*other) == next)
        {
            self.set(other, old);
        }
        self.set(target, next);
    }

    fn normalize(&mut self) {
        let values = BindingTarget::ALL
            .into_iter()
            .map(|target| self.get(target))
            .collect::<Vec<_>>();
        let valid = values.iter().all(|value| Self::BUTTONS.contains(value))
            && values
                .iter()
                .enumerate()
                .all(|(index, value)| !values[..index].contains(value));
        if !valid {
            *self = Self::default();
        }
    }

    fn set(&mut self, target: BindingTarget, value: String) {
        match target {
            BindingTarget::Confirm => self.confirm = value,
            BindingTarget::Back => self.back = value,
            BindingTarget::Context => self.context = value,
            BindingTarget::Favorite => self.favorite = value,
            BindingTarget::PreviousMode => self.previous_mode = value,
            BindingTarget::NextMode => self.next_mode = value,
            BindingTarget::PreviousItem => self.previous_item = value,
            BindingTarget::NextItem => self.next_item = value,
            BindingTarget::Settings => self.settings = value,
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        self.version = CONFIG_VERSION;
        self.neighbor_count = self.neighbor_count.clamp(1, 3);
        self.icon_width = self.icon_width.clamp(12, 40);
        self.icon_height = self.icon_height.clamp(6, 20);
        self.theme = self.theme.min(11);
        self.accent = self.accent.min(4);
        self.border_style = self.border_style.min(4);
        self.animation_speed = self.animation_speed.min(2);
        self.panel_width = self.panel_width.clamp(50, 600);
        self.sound_volume = self.sound_volume.clamp(0.0, 1.0);
        self.rumble_strength = self.rumble_strength.clamp(0.0, 1.0);
        if !matches!(
            self.background_mode.as_str(),
            "gradient" | "picture" | "wallpaper"
        ) {
            self.background_mode = "gradient".to_owned();
        }
        if self.media_paths.is_empty() {
            self.media_paths = default_media_paths();
        }
        self.controller.normalize();
    }

    pub fn load() -> (Self, bool) {
        let Some(path) = paths().map(|paths| paths.config.join("config.toml")) else {
            return (Self::default(), false);
        };
        let Some(contents) = fs::read_to_string(&path).ok() else {
            return (Self::default(), false);
        };
        let Ok(mut settings) = toml::from_str::<Self>(&contents) else {
            return (Self::default(), false);
        };
        // Config written before background modes existed: a stored picture
        // was always shown, so keep showing it.
        if !contents.contains("background_mode") && settings.background_image.is_some() {
            settings.background_mode = "picture".to_owned();
        }
        let migrated = settings.version != CONFIG_VERSION;
        settings.normalize();
        (settings, migrated)
    }

    pub fn save(&self, backup_v4: bool) -> Result<()> {
        let paths = paths().context("could not determine XDG directories")?;
        let path = paths.config.join("config.toml");
        fs::create_dir_all(&paths.config)?;
        if backup_v4 && path.is_file() {
            let backup = paths.config.join("config.toml.bak-v4");
            if !backup.exists() {
                fs::copy(&path, &backup).with_context(|| {
                    format!("failed to back up configuration to {}", backup.display())
                })?;
            }
        }
        atomic_write(&path, toml::to_string_pretty(self)?.as_bytes())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PersistentState {
    pub favorites: HashSet<String>,
    pub recents: HashMap<String, Vec<String>>,
    pub last_mode: Mode,
    pub selected: HashMap<String, String>,
}

impl PersistentState {
    pub fn load() -> Self {
        let Some(path) = paths().map(|paths| paths.state.join("state.toml")) else {
            return Self::default();
        };
        fs::read_to_string(path)
            .ok()
            .and_then(|contents| toml::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let paths = paths().context("could not determine XDG directories")?;
        fs::create_dir_all(&paths.state)?;
        atomic_write(
            &paths.state.join("state.toml"),
            toml::to_string_pretty(self)?.as_bytes(),
        )
    }

    pub fn record_recent(&mut self, mode: Mode, id: &str) {
        let recents = self
            .recents
            .entry(mode.title().to_ascii_lowercase())
            .or_default();
        recents.retain(|recent| recent != id);
        recents.insert(0, id.to_owned());
        recents.truncate(20);
    }

    pub fn recent_position(&self, mode: Mode, id: &str) -> Option<usize> {
        self.recents
            .get(&mode.title().to_ascii_lowercase())?
            .iter()
            .position(|recent| recent == id)
    }

    pub fn toggle_favorite(&mut self, id: &str) -> bool {
        if self.favorites.remove(id) {
            false
        } else {
            self.favorites.insert(id.to_owned());
            true
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config: PathBuf,
    pub cache: PathBuf,
    pub state: PathBuf,
}

pub fn paths() -> Option<AppPaths> {
    let base = BaseDirs::new()?;
    Some(AppPaths {
        config: base.config_dir().join("tui-launcher"),
        cache: base.cache_dir().join("tui-launcher"),
        state: base.state_dir()?.join("tui-launcher"),
    })
}

fn default_media_paths() -> Vec<PathBuf> {
    let Some(base) = BaseDirs::new() else {
        return Vec::new();
    };
    let home = base.home_dir();
    ["Videos", "Pictures", "Music"]
        .into_iter()
        .map(|directory| home.join(directory))
        .collect()
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("failed to save {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_v4_without_losing_appearance() {
        let mut settings: Settings = toml::from_str(
            "version=4\ntheme=5\nborder_style=4\ntransparent=true\npanel_width=180\n",
        )
        .unwrap();
        settings.normalize();
        assert_eq!(settings.version, 5);
        assert_eq!(settings.theme, 5);
        assert_eq!(settings.border_style, 4);
        assert_eq!(settings.panel_width, 180);
        assert!(settings.waves);
    }

    #[test]
    fn recent_history_is_unique_and_bounded() {
        let mut state = PersistentState::default();
        for id in 0..30 {
            state.record_recent(Mode::Game, &id.to_string());
        }
        state.record_recent(Mode::Game, "25");
        let recents = &state.recents["game"];
        assert_eq!(recents.len(), 20);
        assert_eq!(recents[0], "25");
    }

    #[test]
    fn controller_binding_cycle_swaps_collisions() {
        let mut bindings = ControllerBindings::default();
        bindings.cycle(BindingTarget::Confirm);
        assert_eq!(bindings.confirm, "east");
        assert_eq!(bindings.back, "south");
    }

    #[test]
    fn malformed_controller_bindings_reset_safely() {
        let mut bindings = ControllerBindings {
            confirm: "not-a-button".into(),
            ..ControllerBindings::default()
        };
        bindings.normalize();
        assert_eq!(bindings.confirm, "south");
    }
}
