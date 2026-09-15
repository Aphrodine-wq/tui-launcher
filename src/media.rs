//! Media folders: the Photo, Music, and Video columns browse the configured
//! roots one level at a time, the way the original console listed folders
//! on the Memory Stick. Folders that contain no media of the column's kind
//! (at any depth) are not shown at all.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::model::{Action, LibraryItem};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind {
    Photo,
    Music,
    Video,
}

impl MediaKind {
    fn matches(self, extension: &str) -> bool {
        match self {
            Self::Photo => image_extension(extension),
            Self::Music => audio_extension(extension),
            Self::Video => video_extension(extension),
        }
    }

    fn file_label(self) -> &'static str {
        match self {
            Self::Photo => "Picture",
            Self::Music => "Music",
            Self::Video => "Video",
        }
    }

    fn plural(self, count: usize) -> String {
        let noun = match (self, count) {
            (Self::Photo, 1) => "picture",
            (Self::Photo, _) => "pictures",
            (Self::Music, 1) => "track",
            (Self::Music, _) => "tracks",
            (Self::Video, 1) => "video",
            (Self::Video, _) => "videos",
        };
        format!("{count} {noun}")
    }
}

/// One level of the media tree. At the root (`folder == None`) every
/// configured root contributes its folders and loose files; inside a
/// folder, that folder's own subfolders and files. Folders sort by name,
/// files newest first, and folders always come before files.
pub fn list(roots: &[PathBuf], kind: MediaKind, folder: Option<&Path>) -> Vec<LibraryItem> {
    let dirs: Vec<PathBuf> = match folder {
        Some(folder) => vec![folder.to_path_buf()],
        None => roots.to_vec(),
    };
    let mut folders = Vec::new();
    let mut files = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || is_hidden(&path) {
                continue;
            }
            if metadata.is_dir() {
                let count = count_media(&path, kind, 0);
                if count > 0 {
                    folders.push((path, count));
                }
            } else if metadata.is_file() && kind.matches(&extension_of(&path)) {
                files.push(path);
            }
        }
    }
    folders.sort_by(|left, right| {
        file_name_lower(&left.0)
            .cmp(&file_name_lower(&right.0))
            .then_with(|| left.0.cmp(&right.0))
    });
    files.sort_by(|left, right| {
        modified(right)
            .cmp(&modified(left))
            .then_with(|| left.cmp(right))
    });
    folders
        .into_iter()
        .map(|(path, count)| folder_item(path, count, kind))
        .chain(files.into_iter().map(|path| file_item(path, kind)))
        .collect()
}

pub fn is_audio(path: &Path) -> bool {
    audio_extension(&extension_of(path))
}

fn folder_item(path: PathBuf, count: usize, kind: MediaKind) -> LibraryItem {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Folder")
        .to_owned();
    let mut item = LibraryItem::simple(
        format!("folder:{}", canonical.display()),
        title,
        Action::Folder(path.clone()),
    );
    item.subtitle = kind.plural(count);
    item.details = vec![path.display().to_string()];
    if kind == MediaKind::Photo {
        // A picture folder previews its newest picture.
        item.art = first_media(&path, kind, 0);
    }
    item
}

fn file_item(path: PathBuf, kind: MediaKind) -> LibraryItem {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    let title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Media")
        .replace(['_', '-'], " ");
    let mut item = LibraryItem::simple(
        format!("media:{}", canonical.display()),
        title,
        Action::Open(path.clone()),
    );
    item.subtitle = kind.file_label().to_owned();
    item.details = vec![path.display().to_string()];
    if kind == MediaKind::Photo {
        item.art = Some(path);
    }
    item
}

/// Media files of `kind` below `dir`, at any depth (bounded).
fn count_media(dir: &Path, kind: MediaKind, depth: usize) -> usize {
    if depth > 12 {
        return 0;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || is_hidden(&path) {
            continue;
        }
        if metadata.is_dir() {
            count += count_media(&path, kind, depth + 1);
        } else if metadata.is_file() && kind.matches(&extension_of(&path)) {
            count += 1;
        }
    }
    count
}

/// The newest media file of `kind` below `dir`, searching subfolders when
/// the folder itself holds none.
fn first_media(dir: &Path, kind: MediaKind, depth: usize) -> Option<PathBuf> {
    if depth > 12 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    let mut files = Vec::new();
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || is_hidden(&path) {
            continue;
        }
        if metadata.is_dir() {
            subdirs.push(path);
        } else if metadata.is_file() && kind.matches(&extension_of(&path)) {
            files.push(path);
        }
    }
    if let Some(newest) = files.into_iter().max_by_key(|path| modified(path)) {
        return Some(newest);
    }
    subdirs.sort();
    subdirs
        .iter()
        .find_map(|subdir| first_media(subdir, kind, depth + 1))
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase()
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tl-media-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn root_lists_folders_before_files_and_skips_empty_folders() {
        let root = scratch("root");
        fs::write(root.join("loose.png"), b"x").unwrap();
        fs::create_dir_all(root.join("trip")).unwrap();
        fs::write(root.join("trip/beach.jpg"), b"x").unwrap();
        fs::create_dir_all(root.join("nested/deep")).unwrap();
        fs::write(root.join("nested/deep/cave.webp"), b"x").unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/notes.txt"), b"x").unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join(".hidden/secret.png"), b"x").unwrap();

        let items = list(std::slice::from_ref(&root), MediaKind::Photo, None);
        let titles: Vec<&str> = items.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(titles, ["nested", "trip", "loose"]);
        assert!(matches!(items[0].action, Action::Folder(_)));
        assert_eq!(items[0].subtitle, "1 picture");
        assert!(items[0].art.is_some(), "photo folders preview a picture");
        assert!(matches!(items[2].action, Action::Open(_)));

        let inside = list(
            std::slice::from_ref(&root),
            MediaKind::Photo,
            Some(&root.join("trip")),
        );
        assert_eq!(inside.len(), 1);
        assert_eq!(inside[0].title, "beach");

        assert!(list(std::slice::from_ref(&root), MediaKind::Music, None).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn audio_detection_is_by_extension() {
        assert!(is_audio(Path::new("/x/song.FLAC")));
        assert!(!is_audio(Path::new("/x/clip.mp4")));
    }
}
