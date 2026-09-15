use std::{
    env,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::{
    model::{Action, MediaControl, NetworkAction, SystemAction},
    sources::command_exists,
};

pub fn execute(action: &Action) -> Result<String> {
    match action {
        Action::Desktop(path) => launch_desktop(path),
        Action::Steam(app_id) => spawn("steam", &["-applaunch", &app_id.to_string()])
            .map(|_| format!("Launching Steam game {app_id}")),
        Action::Open(path) => {
            spawn_path("xdg-open", path).map(|_| format!("Opening {}", path.display()))
        }
        Action::Media(control) => control_media(control),
        Action::Network(action) => network_action(action),
        Action::System(action) => system_action(action),
        Action::Setting(_)
        | Action::Group(_)
        | Action::Wifi
        | Action::SystemInfo
        | Action::Folder(_)
        | Action::Clock
        | Action::AudioOutput
        | Action::Help
        | Action::Exit => Ok(String::new()),
    }
}

fn network_action(action: &NetworkAction) -> Result<String> {
    match action {
        NetworkAction::OpenBrowser => {
            spawn("xdg-open", &["https://www.google.com"])?;
            Ok("Opening the default browser".to_owned())
        }
        NetworkAction::OpenConnections => {
            if command_exists("nm-connection-editor") {
                spawn("nm-connection-editor", &[])?;
            } else if command_exists("gnome-control-center") {
                spawn("gnome-control-center", &["wifi"])?;
            } else {
                bail!("no graphical network settings tool was found");
            }
            Ok("Opening network settings".to_owned())
        }
    }
}

pub fn diagnose() -> Vec<(&'static str, bool, &'static str)> {
    vec![
        (
            "desktop launch",
            command_exists("gio") || command_exists("gtk-launch"),
            "gio or gtk-launch",
        ),
        ("Steam games", command_exists("steam"), "steam"),
        ("media open", command_exists("xdg-open"), "xdg-open"),
        (
            "video previews",
            command_exists("ffmpegthumbnailer") && command_exists("ffprobe"),
            "ffmpegthumbnailer + ffprobe",
        ),
        (
            "audio",
            command_exists("wpctl") || command_exists("pactl"),
            "wpctl or pactl",
        ),
        (
            "brightness",
            command_exists("brightnessctl"),
            "brightnessctl",
        ),
        (
            "power profiles",
            command_exists("powerprofilesctl"),
            "powerprofilesctl",
        ),
        (
            "session",
            command_exists("loginctl") || command_exists("hyprctl"),
            "loginctl or hyprctl",
        ),
    ]
}

fn launch_desktop(path: &Path) -> Result<String> {
    if command_exists("gio") {
        spawn_path_with_arg("gio", "launch", path)?;
    } else if command_exists("gtk-launch") {
        let id = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".desktop"))
            .context("desktop entry has no valid application ID")?;
        spawn("gtk-launch", &[id])?;
    } else {
        bail!("no desktop launcher backend is installed");
    }
    Ok(format!("Launching {}", path.display()))
}

fn control_media(control: &MediaControl) -> Result<String> {
    let finder =
        mpris::PlayerFinder::new().context("could not connect to the session media bus")?;
    let player = finder
        .find_active()
        .context("no active media player was found")?;
    match control {
        MediaControl::Previous => player.previous()?,
        MediaControl::PlayPause => player.play_pause()?,
        MediaControl::Next => player.next()?,
    }
    Ok(format!("Media: {control:?}"))
}

fn system_action(action: &SystemAction) -> Result<String> {
    match action {
        SystemAction::ScreenOff => {
            if !is_hyprland() {
                bail!("screen off needs Hyprland");
            }
            status("hyprctl", &["dispatch", "dpms", "off"])?;
        }
        SystemAction::VolumeDown => {
            if command_exists("wpctl") {
                status("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"])?;
            } else {
                status("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "-5%"])?;
            }
        }
        SystemAction::VolumeUp => {
            if command_exists("wpctl") {
                status(
                    "wpctl",
                    &["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@", "5%+"],
                )?;
            } else {
                status("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "+5%"])?;
            }
        }
        SystemAction::BrightnessDown => status("brightnessctl", &["set", "5%-"])?,
        SystemAction::BrightnessUp => status("brightnessctl", &["set", "+5%"])?,
        SystemAction::PowerSaver => status("powerprofilesctl", &["set", "power-saver"])?,
        SystemAction::Balanced => status("powerprofilesctl", &["set", "balanced"])?,
        SystemAction::Performance => status("powerprofilesctl", &["set", "performance"])?,
        SystemAction::Lock => {
            if command_exists("hyprlock") {
                spawn("hyprlock", &["--immediate"])?;
            } else {
                status("loginctl", &["lock-session"])?;
            }
        }
        SystemAction::Logout => {
            if is_hyprland() && command_exists("hyprctl") {
                status("hyprctl", &["dispatch", "exit"])?;
            } else if let Some(session) = env::var_os("XDG_SESSION_ID") {
                status_os(
                    "loginctl",
                    &["terminate-session".as_ref(), session.as_os_str()],
                )?;
            } else {
                bail!("XDG_SESSION_ID is unavailable");
            }
        }
        SystemAction::Suspend => status("systemctl", &["suspend"])?,
        SystemAction::Reboot => status("systemctl", &["reboot"])?,
        SystemAction::Shutdown => status("systemctl", &["poweroff"])?,
    }
    Ok(match action {
        SystemAction::VolumeDown | SystemAction::VolumeUp => "Volume adjusted",
        SystemAction::BrightnessDown | SystemAction::BrightnessUp => "Brightness adjusted",
        SystemAction::PowerSaver | SystemAction::Balanced | SystemAction::Performance => {
            "Power profile changed"
        }
        SystemAction::ScreenOff => "Displays off — press any button to wake",
        SystemAction::Lock => "Session locked",
        SystemAction::Logout => "Logging out",
        SystemAction::Suspend => "Suspending",
        SystemAction::Reboot => "Restarting",
        SystemAction::Shutdown => "Powering off",
    }
    .to_owned())
}

fn spawn(program: &str, args: &[&str]) -> Result<()> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch {program}"))?;
    Ok(())
}

fn spawn_path(program: &str, path: &Path) -> Result<()> {
    Command::new(program)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch {program}"))?;
    Ok(())
}

fn spawn_path_with_arg(program: &str, arg: &str, path: &Path) -> Result<()> {
    Command::new(program)
        .arg(arg)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch {program}"))?;
    Ok(())
}

fn status(program: &str, args: &[&str]) -> Result<()> {
    let exit = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !exit.success() {
        bail!("{program} returned {exit}");
    }
    Ok(())
}

fn status_os(program: &str, args: &[&std::ffi::OsStr]) -> Result<()> {
    let exit = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !exit.success() {
        bail!("{program} returned {exit}");
    }
    Ok(())
}

fn is_hyprland() -> bool {
    env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktop| desktop.to_ascii_lowercase().contains("hyprland"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_do_not_execute_capabilities() {
        let diagnostics = diagnose();
        assert!(diagnostics.iter().any(|(name, _, _)| *name == "session"));
    }

    #[test]
    fn destructive_actions_are_classified_outside_execution() {
        assert!(SystemAction::Shutdown.destructive());
        assert!(!SystemAction::VolumeUp.destructive());
    }
}
