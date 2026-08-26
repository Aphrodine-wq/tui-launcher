//! Distributable theme packs — the launcher's mod format.
//!
//! A pack is a plain folder under `~/.config/tui-launcher/themes/<id>/`:
//! `theme.toml` plus optional `icons/` (per-category SVG/PNG) and sound
//! files. Everything is optional; anything missing falls back to the
//! built-in look. Sharing a pack is sharing the folder.

use std::{fs, path::PathBuf};

use serde::Deserialize;

use crate::{config, feedback::Tone, model::Mode};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawTheme {
    name: String,
    author: String,
    accent: String,
    background_top: String,
    background_bottom: String,
    background_image: String,
    sound_navigate: String,
    sound_confirm: String,
    sound_back: String,
    sound_warning: String,
    sound_boot: String,
}

#[derive(Debug, Default)]
pub struct ThemePack {
    pub name: String,
    pub author: String,
    pub accent: Option<[u8; 3]>,
    pub gradient: Option<([u8; 3], [u8; 3])>,
    pub background: Option<PathBuf>,
    icons: [Option<PathBuf>; Mode::ALL.len()],
    sounds: [Option<Vec<u8>>; 5],
}

impl ThemePack {
    pub fn icon(&self, mode: Mode) -> Option<&PathBuf> {
        self.icons[mode.index()].as_ref()
    }

    pub fn sound(&self, tone: Tone) -> Option<&[u8]> {
        self.sounds[tone_index(tone)].as_deref()
    }
}

fn tone_index(tone: Tone) -> usize {
    match tone {
        Tone::Navigate => 0,
        Tone::Confirm => 1,
        Tone::Back => 2,
        Tone::Warning => 3,
        Tone::Boot => 4,
    }
}

pub fn themes_dir() -> Option<PathBuf> {
    Some(config::paths()?.config.join("themes"))
}

/// Installed pack ids (directory names containing a theme.toml), sorted.
pub fn list_packs() -> Vec<String> {
    let Some(dir) = themes_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut packs: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().join("theme.toml").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    packs.sort();
    packs
}

pub fn load(id: &str) -> Option<ThemePack> {
    load_from_dir(&themes_dir()?.join(id), id)
}

fn load_from_dir(dir: &std::path::Path, id: &str) -> Option<ThemePack> {
    let raw: RawTheme = toml::from_str(&fs::read_to_string(dir.join("theme.toml")).ok()?).ok()?;
    let file = |name: &str| -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        let path = dir.join(name);
        path.is_file().then_some(path)
    };
    let mut icons: [Option<PathBuf>; Mode::ALL.len()] = Default::default();
    for mode in Mode::ALL {
        let stem = mode.title().to_ascii_lowercase();
        icons[mode.index()] = ["svg", "png", "webp", "jpg"]
            .iter()
            .map(|extension| dir.join("icons").join(format!("{stem}.{extension}")))
            .find(|path| path.is_file());
    }
    let sound = |name: &str| file(name).and_then(|path| fs::read(path).ok());
    Some(ThemePack {
        name: if raw.name.is_empty() {
            id.to_owned()
        } else {
            raw.name
        },
        author: raw.author,
        accent: parse_hex(&raw.accent),
        gradient: parse_hex(&raw.background_top).zip(parse_hex(&raw.background_bottom)),
        background: file(&raw.background_image),
        icons,
        sounds: [
            sound(&raw.sound_navigate),
            sound(&raw.sound_confirm),
            sound(&raw.sound_back),
            sound(&raw.sound_warning),
            sound(&raw.sound_boot),
        ],
    })
}

/// "#rrggbb" or "rrggbb" → RGB. Anything else is None.
pub fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors_parse_leniently() {
        assert_eq!(parse_hex("#96e2ff"), Some([0x96, 0xe2, 0xff]));
        assert_eq!(parse_hex("96E2FF"), Some([0x96, 0xe2, 0xff]));
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("not a color"), None);
    }

    #[test]
    fn partial_packs_degrade_to_builtins() {
        let dir = std::env::temp_dir().join(format!("tp-theme-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("icons")).unwrap();
        fs::write(
            dir.join("theme.toml"),
            "name = \"Test Pack\"\naccent = \"#ff0080\"\nbackground_top = \"#101020\"\n",
        )
        .unwrap();
        fs::write(dir.join("icons").join("game.svg"), "<svg/>").unwrap();
        let pack = load_from_dir(&dir, "test").unwrap();
        assert_eq!(pack.name, "Test Pack");
        assert_eq!(pack.accent, Some([0xff, 0x00, 0x80]));
        // top without bottom -> no gradient override
        assert!(pack.gradient.is_none());
        assert!(pack.icon(Mode::Game).is_some());
        assert!(pack.icon(Mode::Music).is_none());
        assert!(pack.sound(Tone::Boot).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
