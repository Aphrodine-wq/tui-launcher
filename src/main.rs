mod config;
mod feedback;
mod input;
mod model;
mod platform;
mod sources;
mod ui;

use std::{
    io::{self, IsTerminal},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use crossterm::{
    event::{
        self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyboardEnhancementFlags, MouseButton,
        MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use feedback::{AudioFeedback, Tone};
use input::{ControllerInput, InputAction, InputEvent};
use model::{Action, AppState, Mode, SettingAction};
use ratatui::{Terminal, backend::CrosstermBackend};
use ui::{ImageCache, Presentation};

use crate::config::{PersistentState, Settings};

#[derive(Parser)]
#[command(version, about = "A PSP-inspired Linux terminal launcher")]
struct Cli {
    #[arg(long, conflicts_with = "overlay")]
    fullscreen: bool,
    #[arg(long, conflicts_with = "fullscreen")]
    overlay: bool,
    #[arg(long, value_enum)]
    start: Option<StartMode>,
    #[arg(long)]
    list: bool,
    #[arg(long)]
    diagnose: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum StartMode {
    Applications,
    Games,
    Media,
    System,
    Settings,
}

impl From<StartMode> for Mode {
    fn from(value: StartMode) -> Self {
        match value {
            StartMode::Applications => Self::Applications,
            StartMode::Games => Self::Games,
            StartMode::Media => Self::Media,
            StartMode::System => Self::System,
            StartMode::Settings => Self::Settings,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.list {
        for application in sources::discover_applications() {
            if let Action::Desktop(path) = application.action {
                println!("{}\t{}", application.title, path.display());
            }
        }
        return Ok(());
    }
    if cli.diagnose {
        print_diagnostics();
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("tui-launcher must run inside a terminal (use --list or --diagnose otherwise)");
    }

    let (mut settings, migrated) = Settings::load();
    let mut persisted = PersistentState::load();
    let presentation = if cli.fullscreen || (!cli.overlay && settings.default_fullscreen) {
        Presentation::Fullscreen
    } else {
        Presentation::Overlay
    };
    let start_mode = cli.start.map(Mode::from).unwrap_or(persisted.last_mode);
    let mut state = AppState::new(start_mode, sources::discover_all(&settings, &persisted));
    restore_selections(&mut state, &persisted);
    refresh_now_playing(&mut state);

    let launch_after_restore = run_tui(&mut state, &mut settings, &mut persisted, presentation)?;
    save_runtime_state(&state, &mut persisted);
    settings.save(migrated)?;
    persisted.save()?;
    if let Some(action) = launch_after_restore {
        platform::execute(&action)?;
    }
    Ok(())
}

fn run_tui(
    state: &mut AppState,
    settings: &mut Settings,
    persisted: &mut PersistentState,
    presentation: Presentation,
) -> Result<Option<Action>> {
    let mut session = TerminalSession::new()?;
    let mut images = ImageCache::new();
    let audio = AudioFeedback::new();
    let mut controller = ControllerInput::new(settings.controller.clone());
    state.controller_name = controller.name.clone();
    let mut keyboard_confirm_down = false;
    let mut last_media_refresh = Instant::now();
    let (status_tx, status_rx) = mpsc::channel();
    let mut last_status_refresh = Instant::now() - Duration::from_secs(10);

    loop {
        state.tick(settings.reduced_motion);
        state.controller_name = controller.name.clone();
        if last_media_refresh.elapsed() >= Duration::from_secs(2) {
            refresh_now_playing(state);
            last_media_refresh = Instant::now();
        }
        while let Ok(status) = status_rx.try_recv() {
            state.system_status = status;
        }
        if last_status_refresh.elapsed() >= Duration::from_secs(5) {
            let sender = status_tx.clone();
            thread::spawn(move || {
                let _ = sender.send(sources::system_status());
            });
            last_status_refresh = Instant::now();
        }
        if hold_complete(state)
            && (controller.confirm_pressed()
                || (session.enhanced_keyboard && keyboard_confirm_down))
            && let Some(action) = state.selected_item().map(|item| item.action.clone())
        {
            state.hold_started = None;
            execute_in_place(state, settings, &action, audio.as_ref(), &mut controller);
        }

        let glyphs = controller.glyphs();
        session.terminal.draw(|frame| {
            ui::draw(
                frame,
                state,
                settings,
                persisted,
                presentation,
                &mut images,
                glyphs,
            )
        })?;

        for input in controller.poll() {
            if !input.pressed && input.action == InputAction::Confirm {
                state.hold_started = None;
                continue;
            }
            if input.pressed
                && let Some(outcome) = handle_input(
                    input,
                    state,
                    settings,
                    persisted,
                    presentation,
                    audio.as_ref(),
                    &mut controller,
                    &mut images,
                )
            {
                return Ok(outcome);
            }
        }

        let timeout = if state.motion != 0.0 || state.hold_started.is_some() {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(50)
        };
        if !event::poll(timeout)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if key.code == KeyCode::Enter {
                    match key.kind {
                        KeyEventKind::Press | KeyEventKind::Repeat => keyboard_confirm_down = true,
                        KeyEventKind::Release => {
                            keyboard_confirm_down = false;
                            state.hold_started = None;
                        }
                    }
                }
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                    && let Some(input) = map_key(key)
                    && let Some(outcome) = handle_input(
                        input,
                        state,
                        settings,
                        persisted,
                        presentation,
                        audio.as_ref(),
                        &mut controller,
                        &mut images,
                    )
                {
                    return Ok(outcome);
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    navigate_item(state, -1, settings, audio.as_ref(), &mut controller)
                }
                MouseEventKind::ScrollDown => {
                    navigate_item(state, 1, settings, audio.as_ref(), &mut controller)
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let input = InputEvent {
                        action: InputAction::Confirm,
                        pressed: true,
                    };
                    if let Some(outcome) = handle_input(
                        input,
                        state,
                        settings,
                        persisted,
                        presentation,
                        audio.as_ref(),
                        &mut controller,
                        &mut images,
                    ) {
                        return Ok(outcome);
                    }
                }
                _ => {}
            },
            Event::FocusGained => {
                state.focused = true;
                images.clear();
                refresh_now_playing(state);
            }
            Event::FocusLost => state.focused = false,
            Event::Resize(_, _) => images.clear(),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_input(
    input: InputEvent,
    state: &mut AppState,
    settings: &mut Settings,
    persisted: &mut PersistentState,
    presentation: Presentation,
    audio: Option<&AudioFeedback>,
    controller: &mut ControllerInput,
    images: &mut ImageCache,
) -> Option<Option<Action>> {
    match input.action {
        InputAction::PreviousMode => {
            state.switch_mode(-1);
            feedback(audio, controller, settings, Tone::Navigate, 25);
        }
        InputAction::NextMode => {
            state.switch_mode(1);
            feedback(audio, controller, settings, Tone::Navigate, 25);
        }
        InputAction::PreviousItem => navigate_item(state, -1, settings, audio, controller),
        InputAction::NextItem => navigate_item(state, 1, settings, audio, controller),
        InputAction::Settings => {
            state.mode = Mode::Settings;
            state.detail_open = false;
            feedback(audio, controller, settings, Tone::Navigate, 25);
        }
        InputAction::Context => {
            if !matches!(state.mode, Mode::System | Mode::Settings)
                && state.selected_item().is_some()
            {
                state.detail_open = !state.detail_open;
                feedback(audio, controller, settings, Tone::Confirm, 35);
            }
        }
        InputAction::Favorite => toggle_favorite(state, persisted, settings, audio, controller),
        InputAction::Back => {
            if state.detail_open {
                state.detail_open = false;
                feedback(audio, controller, settings, Tone::Back, 25);
            } else {
                feedback(audio, controller, settings, Tone::Back, 25);
                return Some(None);
            }
        }
        InputAction::Confirm => {
            let item = state.selected_item()?.clone();
            if !item.available {
                state.toast(
                    item.unavailable_reason
                        .unwrap_or_else(|| "This action is unavailable".to_owned()),
                );
                feedback(audio, controller, settings, Tone::Warning, 50);
                return None;
            }
            if let Action::Setting(action) = item.action {
                adjust_setting(action, settings);
                if !settings.rumble {
                    controller.stop_rumble();
                }
                controller.set_bindings(settings.controller.clone());
                state
                    .items
                    .insert(Mode::Settings, sources::setting_items(settings));
                images.clear();
                state.toast("Setting updated");
                feedback(audio, controller, settings, Tone::Confirm, 35);
                return None;
            }
            if ui::destructive(&item) {
                if state.hold_started.as_ref().is_some_and(|(id, started)| {
                    id == &item.id && started.elapsed() >= Duration::from_secs(1)
                }) {
                    state.hold_started = None;
                    execute_in_place(state, settings, &item.action, audio, controller);
                } else if state.hold_started.is_none() {
                    state.hold_started = Some((item.id, Instant::now()));
                    feedback(audio, controller, settings, Tone::Warning, 45);
                }
                return None;
            }
            persisted.record_recent(state.mode, &item.id);
            match (&item.action, presentation) {
                (
                    Action::Desktop(_) | Action::Steam(_) | Action::Open(_),
                    Presentation::Overlay,
                ) => {
                    feedback(audio, controller, settings, Tone::Confirm, 55);
                    return Some(Some(item.action));
                }
                _ => execute_in_place(state, settings, &item.action, audio, controller),
            }
        }
    }
    None
}

fn navigate_item(
    state: &mut AppState,
    delta: isize,
    settings: &Settings,
    audio: Option<&AudioFeedback>,
    controller: &mut ControllerInput,
) {
    state.select_delta(delta);
    state.detail_open = false;
    state.hold_started = None;
    feedback(audio, controller, settings, Tone::Navigate, 18);
}

fn toggle_favorite(
    state: &mut AppState,
    persisted: &mut PersistentState,
    settings: &Settings,
    audio: Option<&AudioFeedback>,
    controller: &mut ControllerInput,
) {
    if matches!(state.mode, Mode::System | Mode::Settings) {
        return;
    }
    let Some(item) = state.selected_item().cloned() else {
        return;
    };
    let favorite = persisted.toggle_favorite(&item.id);
    if let Some(items) = state.items.get_mut(&state.mode) {
        sources::order_library(items, persisted, state.mode);
        if let Some(index) = items.iter().position(|candidate| candidate.id == item.id) {
            state.selections.insert(state.mode, index);
        }
    }
    state.toast(if favorite {
        "Added to favorites"
    } else {
        "Removed from favorites"
    });
    feedback(audio, controller, settings, Tone::Confirm, 35);
}

fn execute_in_place(
    state: &mut AppState,
    settings: &Settings,
    action: &Action,
    audio: Option<&AudioFeedback>,
    controller: &mut ControllerInput,
) {
    match platform::execute(action) {
        Ok(message) => {
            if !message.is_empty() {
                state.toast(message);
            }
            feedback(audio, controller, settings, Tone::Confirm, 55);
        }
        Err(error) => {
            state.toast(error.to_string());
            feedback(audio, controller, settings, Tone::Warning, 60);
        }
    }
}

fn feedback(
    audio: Option<&AudioFeedback>,
    controller: &mut ControllerInput,
    settings: &Settings,
    tone: Tone,
    rumble_ms: u32,
) {
    if settings.sound
        && let Some(audio) = audio
    {
        audio.play(tone, settings.sound_volume);
    }
    if settings.rumble {
        controller.rumble(settings.rumble_strength, rumble_ms);
    }
}

fn adjust_setting(action: SettingAction, settings: &mut Settings) {
    match action {
        SettingAction::Theme => settings.theme = (settings.theme + 1) % 12,
        SettingAction::Accent => settings.accent = (settings.accent + 1) % 5,
        SettingAction::Transparent => settings.transparent = !settings.transparent,
        SettingAction::Waves => settings.waves = !settings.waves,
        SettingAction::Sound => settings.sound = !settings.sound,
        SettingAction::Rumble => settings.rumble = !settings.rumble,
        SettingAction::ReducedMotion => settings.reduced_motion = !settings.reduced_motion,
        SettingAction::NetworkArtwork => settings.network_artwork = !settings.network_artwork,
        SettingAction::PanelWidth => {
            settings.panel_width = if settings.panel_width >= 220 {
                80
            } else {
                settings.panel_width + 20
            }
        }
        SettingAction::ResetAppearance => {
            let defaults = Settings::default();
            settings.theme = defaults.theme;
            settings.accent = defaults.accent;
            settings.transparent = defaults.transparent;
            settings.waves = defaults.waves;
            settings.border_style = defaults.border_style;
            settings.animation_speed = defaults.animation_speed;
            settings.panel_width = defaults.panel_width;
        }
        SettingAction::Binding(target) => settings.controller.cycle(target),
    }
}

fn map_key(key: KeyEvent) -> Option<InputEvent> {
    let action = match key.code {
        KeyCode::Left | KeyCode::Char('h') => InputAction::PreviousMode,
        KeyCode::Right | KeyCode::Char('l') => InputAction::NextMode,
        KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp => InputAction::PreviousItem,
        KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => InputAction::NextItem,
        KeyCode::Enter => InputAction::Confirm,
        KeyCode::Esc | KeyCode::Char('q') => InputAction::Back,
        KeyCode::Char('c') | KeyCode::Char('C') => InputAction::Context,
        KeyCode::Char('f') | KeyCode::Char('F') => InputAction::Favorite,
        KeyCode::Char('s') | KeyCode::Char('S') => InputAction::Settings,
        _ => return None,
    };
    Some(InputEvent {
        action,
        pressed: true,
    })
}

fn refresh_now_playing(state: &mut AppState) {
    let now = sources::now_playing();
    state.now_playing = now.clone();
    let Some(items) = state.items.get_mut(&Mode::Media) else {
        return;
    };
    items.retain(|item| {
        !matches!(
            item.id.as_str(),
            "media:previous" | "media:play-pause" | "media:next"
        )
    });
    items.splice(0..0, sources::media_control_items(now.as_ref()));
}

fn restore_selections(state: &mut AppState, persisted: &PersistentState) {
    for mode in Mode::ALL {
        let Some(id) = persisted.selected.get(&mode.title().to_ascii_lowercase()) else {
            continue;
        };
        if let Some(index) = state
            .items
            .get(&mode)
            .and_then(|items| items.iter().position(|item| &item.id == id))
        {
            state.selections.insert(mode, index);
        }
    }
}

fn save_runtime_state(state: &AppState, persisted: &mut PersistentState) {
    persisted.last_mode = state.mode;
    for mode in Mode::ALL {
        if let Some(item) = state
            .items
            .get(&mode)
            .and_then(|items| items.get(state.selections.get(&mode).copied().unwrap_or(0)))
        {
            persisted
                .selected
                .insert(mode.title().to_ascii_lowercase(), item.id.clone());
        }
    }
}

fn hold_complete(state: &AppState) -> bool {
    state
        .hold_started
        .as_ref()
        .is_some_and(|(_, started)| started.elapsed() >= Duration::from_secs(1))
}

fn print_diagnostics() {
    let (settings, _) = Settings::load();
    let state = PersistentState::load();
    let modes = sources::discover_all(&settings, &state);
    println!("tui-launcher {}", env!("CARGO_PKG_VERSION"));
    for mode in Mode::ALL {
        println!(
            "{:<14} {} items",
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

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    enhanced_keyboard: bool,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode().context("failed to enter raw terminal mode")?;
        let mut rollback = TerminalInitRollback {
            active: true,
            enhanced_keyboard: false,
        };
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableFocusChange
        ) {
            let _ = disable_raw_mode();
            return Err(error).context("failed to initialize terminal screen");
        }
        let enhanced_keyboard = if std::env::var_os("TUI_LAUNCHER_NO_QUERIES").is_some() {
            false
        } else {
            supports_keyboard_enhancement().unwrap_or(false)
        };
        if enhanced_keyboard {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
            )?;
            rollback.enhanced_keyboard = true;
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).context("failed to create terminal")?;
        terminal.clear()?;
        rollback.active = false;
        Ok(Self {
            terminal,
            enhanced_keyboard,
        })
    }
}

struct TerminalInitRollback {
    active: bool,
    enhanced_keyboard: bool,
}

impl Drop for TerminalInitRollback {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        if self.enhanced_keyboard {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            stdout,
            DisableFocusChange,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if self.enhanced_keyboard {
            let _ = execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableFocusChange,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn start_modes_map() {
        assert_eq!(Mode::from(StartMode::Games), Mode::Games);
    }
    #[test]
    fn settings_actions_are_bounded() {
        let mut settings = Settings {
            theme: 11,
            accent: 4,
            panel_width: 220,
            ..Settings::default()
        };
        adjust_setting(SettingAction::Theme, &mut settings);
        adjust_setting(SettingAction::Accent, &mut settings);
        adjust_setting(SettingAction::PanelWidth, &mut settings);
        assert_eq!(
            (settings.theme, settings.accent, settings.panel_width),
            (0, 0, 80)
        );
    }
}
