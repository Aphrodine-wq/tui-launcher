use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Settings,
    Extras,
    Photo,
    Music,
    Video,
    Game,
    Network,
}

impl Mode {
    pub const ALL: [Self; 7] = [
        Self::Settings,
        Self::Extras,
        Self::Photo,
        Self::Music,
        Self::Video,
        Self::Game,
        Self::Network,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Settings => "SETTINGS",
            Self::Extras => "EXTRAS",
            Self::Photo => "PHOTO",
            Self::Music => "MUSIC",
            Self::Video => "VIDEO",
            Self::Game => "GAME",
            Self::Network => "NETWORK",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Settings => "⚙",
            Self::Extras => "✦",
            Self::Photo => "▧",
            Self::Music => "♪",
            Self::Video => "▶",
            Self::Game => "◆",
            Self::Network => "◎",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaControl {
    Previous,
    PlayPause,
    Next,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkAction {
    OpenBrowser,
    OpenConnections,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemAction {
    VolumeDown,
    VolumeUp,
    BrightnessDown,
    BrightnessUp,
    PowerSaver,
    Balanced,
    Performance,
    Lock,
    Logout,
    Suspend,
    Reboot,
    Shutdown,
}

impl SystemAction {
    pub fn destructive(&self) -> bool {
        matches!(
            self,
            Self::Logout | Self::Suspend | Self::Reboot | Self::Shutdown
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Desktop(PathBuf),
    Steam(u32),
    Open(PathBuf),
    Media(MediaControl),
    Network(NetworkAction),
    System(SystemAction),
    Setting(SettingAction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingAction {
    Theme,
    Accent,
    Transparent,
    Waves,
    Sound,
    ReducedMotion,
    NetworkArtwork,
    PanelWidth,
    ResetAppearance,
    Binding(BindingTarget),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingTarget {
    Confirm,
    Back,
    Context,
    Favorite,
    PreviousMode,
    NextMode,
    PreviousItem,
    NextItem,
    Settings,
}

impl BindingTarget {
    pub const ALL: [Self; 9] = [
        Self::Confirm,
        Self::Back,
        Self::Context,
        Self::Favorite,
        Self::PreviousMode,
        Self::NextMode,
        Self::PreviousItem,
        Self::NextItem,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Confirm => "Confirm",
            Self::Back => "Back",
            Self::Context => "Details",
            Self::Favorite => "Favorite",
            Self::PreviousMode => "Previous mode",
            Self::NextMode => "Next mode",
            Self::PreviousItem => "Previous item",
            Self::NextItem => "Next item",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LibraryItem {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub details: Vec<String>,
    pub art: Option<PathBuf>,
    pub hero: Option<PathBuf>,
    pub action: Action,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

impl LibraryItem {
    pub fn simple(id: impl Into<String>, title: impl Into<String>, action: Action) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: String::new(),
            details: Vec::new(),
            art: None,
            hero: None,
            action,
            available: true,
            unavailable_reason: None,
        }
    }

    pub fn unavailable(mut self, reason: impl Into<String>) -> Self {
        self.available = false;
        self.unavailable_reason = Some(reason.into());
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct NowPlaying {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub status: String,
    pub art: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemStatus {
    pub battery: Option<String>,
    pub network: Option<String>,
    pub volume: Option<String>,
}

pub struct AppState {
    pub mode: Mode,
    pub items: HashMap<Mode, Vec<LibraryItem>>,
    pub selections: HashMap<Mode, usize>,
    pub now_playing: Option<NowPlaying>,
    pub system_status: SystemStatus,
    pub controller_name: Option<String>,
}

impl AppState {
    pub fn new(mode: Mode, items: HashMap<Mode, Vec<LibraryItem>>) -> Self {
        let selections = Mode::ALL.into_iter().map(|mode| (mode, 0)).collect();
        Self {
            mode,
            items,
            selections,
            now_playing: None,
            system_status: SystemStatus::default(),
            controller_name: None,
        }
    }

    pub fn mode_items(&self) -> &[LibraryItem] {
        self.items.get(&self.mode).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn selected_index(&self) -> usize {
        self.selections.get(&self.mode).copied().unwrap_or(0)
    }

    pub fn selected_item(&self) -> Option<&LibraryItem> {
        self.mode_items().get(self.selected_index())
    }

    pub fn select_delta(&mut self, delta: isize) {
        let len = self.mode_items().len();
        if len == 0 {
            return;
        }
        let current = self.selected_index();
        let next = (current as isize + delta).rem_euclid(len as isize) as usize;
        self.selections.insert(self.mode, next);
    }

    pub fn switch_mode(&mut self, delta: isize) {
        let next = (self.mode.index() as isize + delta).rem_euclid(Mode::ALL.len() as isize);
        self.mode = Mode::ALL[next as usize];
    }
}
