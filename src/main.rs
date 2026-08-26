mod config;
mod feedback;
mod gui;
mod input;
mod model;
mod platform;
mod sources;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use eframe::egui;

use crate::{config::Settings, model::Mode};

#[derive(Parser)]
#[command(version, about = "A native PSP-inspired XMB desktop overlay")]
struct Cli {
    /// Open as a full monitor console shell instead of a desktop overlay.
    #[arg(long)]
    fullscreen: bool,
    #[arg(long, value_enum)]
    start: Option<StartMode>,
    /// Print discovered desktop applications without opening the interface.
    #[arg(long)]
    list: bool,
    /// Print content counts and launch capabilities without changing the system.
    #[arg(long)]
    diagnose: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum StartMode {
    Settings,
    Extras,
    Photo,
    Music,
    Video,
    Game,
    Network,
}

impl From<StartMode> for Mode {
    fn from(value: StartMode) -> Self {
        match value {
            StartMode::Settings => Self::Settings,
            StartMode::Extras => Self::Extras,
            StartMode::Photo => Self::Photo,
            StartMode::Music => Self::Music,
            StartMode::Video => Self::Video,
            StartMode::Game => Self::Game,
            StartMode::Network => Self::Network,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.list {
        for application in sources::discover_applications() {
            if let model::Action::Desktop(path) = application.action {
                println!("{}\t{}", application.title, path.display());
            }
        }
        return Ok(());
    }
    if cli.diagnose {
        print_diagnostics();
        return Ok(());
    }

    let (settings, migrated) = Settings::load();
    let persisted = config::PersistentState::load();
    let start = cli.start.map(Mode::from).unwrap_or(persisted.last_mode);
    let size = if cli.fullscreen {
        egui::vec2(1280.0, 720.0)
    } else {
        egui::vec2(1180.0, 664.0)
    };
    let viewport = egui::ViewportBuilder::default()
        .with_app_id("com.aphrodine.xmb-launcher")
        .with_title("XMB Launcher")
        .with_inner_size(size)
        .with_min_inner_size(egui::vec2(800.0, 450.0))
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(true)
        .with_fullscreen(cli.fullscreen)
        .with_window_level(egui::WindowLevel::AlwaysOnTop);
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        centered: !cli.fullscreen,
        persist_window: false,
        ..Default::default()
    };

    eframe::run_native(
        "com.aphrodine.xmb-launcher",
        options,
        Box::new(move |creation| {
            Ok(Box::new(gui::XmbApp::new(
                creation, settings, migrated, persisted, start,
            )))
        }),
    )?;
    Ok(())
}

fn print_diagnostics() {
    let (settings, _) = Settings::load();
    let state = config::PersistentState::load();
    let modes = sources::discover_all(&settings, &state);
    println!("tui-launcher {} (native XMB)", env!("CARGO_PKG_VERSION"));
    for mode in Mode::ALL {
        println!(
            "{:<10} {} items",
            mode.title().to_ascii_lowercase(),
            modes.get(&mode).map(Vec::len).unwrap_or(0)
        );
    }
    println!("capabilities:");
    for (name, available, backend) in platform::diagnose() {
        println!(
            "  {:<18} {:<3} ({backend})",
            name,
            if available { "yes" } else { "no" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentic_category_order_is_stable() {
        assert_eq!(Mode::ALL[0], Mode::Settings);
        assert_eq!(Mode::ALL[5], Mode::Game);
        assert_eq!(Mode::ALL[6], Mode::Network);
    }

    #[test]
    fn start_modes_map() {
        assert_eq!(Mode::from(StartMode::Extras), Mode::Extras);
        assert_eq!(Mode::from(StartMode::Music), Mode::Music);
    }
}
