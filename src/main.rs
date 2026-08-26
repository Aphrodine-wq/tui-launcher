use std::{
    collections::HashSet,
    env,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use freedesktop_desktop_entry::{
    DesktopEntry, current_desktop, default_paths, get_languages_from_env,
};
use image::{DynamicImage, ImageBuffer, Rgba, imageops::FilterType};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use ratatui_image::{Image, Resize, picker::Picker, protocol::Protocol};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

const CONFIG_VERSION: u8 = 4;
const SETTINGS_COUNT: usize = 13;

#[derive(Clone, Copy)]
struct Theme {
    name: &'static str,
    background: Color,
    panel: Color,
    text: Color,
    muted: Color,
    border: Color,
    accents: [Color; 5],
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
struct Settings {
    #[serde(default = "legacy_config_version")]
    version: u8,
    neighbor_count: usize,
    icon_width: u16,
    icon_height: u16,
    show_index: bool,
    show_footer: bool,
    show_header: bool,
    transparent: bool,
    theme: usize,
    accent: usize,
    border_style: usize,
    animation_speed: usize,
    panel_width: u16,
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
        }
    }
}

impl Settings {
    fn normalize(&mut self) {
        self.version = CONFIG_VERSION;
        self.neighbor_count = self.neighbor_count.clamp(1, 3);
        self.icon_width = self.icon_width.clamp(12, 32);
        self.icon_height = self.icon_height.clamp(6, 16);
        self.theme = self.theme.min(theme_count() - 1);
        self.accent = self.accent.min(4);
        self.border_style = self.border_style.min(4);
        self.animation_speed = self.animation_speed.min(2);
        self.panel_width = self.panel_width.clamp(50, 600);
    }

    fn path() -> Option<PathBuf> {
        let base = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("tui-launcher/config.toml"))
    }

    fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let settings: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| toml::from_str(&contents).ok())
            .unwrap_or_default();
        if settings.version != CONFIG_VERSION {
            let settings = Self::default();
            let _ = settings.save();
            return settings;
        }
        let mut settings = settings;
        settings.normalize();
        settings
    }

    fn save(&self) -> Result<()> {
        let path = Self::path().context("could not determine the settings path")?;
        let parent = path.parent().context("settings path has no parent")?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("failed to save {}", path.display()))
    }
}

struct Application {
    id: String,
    name: String,
    desktop_file: PathBuf,
    icon_path: Option<PathBuf>,
    icon_source: Option<DynamicImage>,
    focus_image: Option<Protocol>,
    thumbnail: Option<Protocol>,
    compact_thumbnail: Option<Protocol>,
}

struct Launcher {
    applications: Vec<Application>,
    selected: usize,
    carousel_areas: Vec<(usize, Rect)>,
    settings: Settings,
    settings_open: bool,
    settings_selected: usize,
    settings_dirty: bool,
    picker: Picker,
    motion: f32,
    last_tick: Instant,
}

impl Launcher {
    fn new(applications: Vec<Application>, settings: Settings, picker: Picker) -> Self {
        Self {
            applications,
            selected: 0,
            carousel_areas: Vec::new(),
            settings,
            settings_open: false,
            settings_selected: 0,
            settings_dirty: false,
            picker,
            motion: 0.0,
            last_tick: Instant::now(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if delta == 0 || self.applications.is_empty() {
            return;
        }
        self.selected = wrapped_index(self.selected, delta, self.applications.len());
        self.motion = (self.motion + delta.signum() as f32).clamp(-4.0, 4.0);
        self.last_tick = Instant::now();
    }

    fn set_selection(&mut self, selected: usize) {
        if selected == self.selected || selected >= self.applications.len() {
            return;
        }
        let delta = circular_delta(self.selected, selected, self.applications.len());
        self.move_selection(delta);
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_tick).as_secs_f32().min(0.05);
        self.last_tick = now;
        if self.motion.abs() > 0.001 {
            self.motion *= (-animation_decay(self.settings.animation_speed) * elapsed).exp();
            if self.motion.abs() < 0.01 {
                self.motion = 0.0;
            }
        }
    }

    fn is_animating(&self) -> bool {
        self.motion != 0.0
    }

    fn select_at(&mut self, column: u16, row: u16) -> bool {
        if let Some((index, _)) = self
            .carousel_areas
            .iter()
            .find(|(_, area)| point_in_rect(column, row, *area))
        {
            self.set_selection(*index);
            true
        } else {
            false
        }
    }

    fn adjust_setting(&mut self, delta: isize) {
        let old_icon_size = (self.settings.icon_width, self.settings.icon_height);
        match self.settings_selected {
            0 => adjust_usize(&mut self.settings.neighbor_count, delta, 1, 3),
            1 => adjust_u16(&mut self.settings.icon_width, delta, 12, 32),
            2 => adjust_u16(&mut self.settings.icon_height, delta, 6, 16),
            3 => self.settings.show_index = !self.settings.show_index,
            4 => self.settings.show_footer = !self.settings.show_footer,
            5 => self.settings.show_header = !self.settings.show_header,
            6 => self.settings.transparent = !self.settings.transparent,
            7 => adjust_usize(&mut self.settings.theme, delta, 0, theme_count() - 1),
            8 => adjust_usize(&mut self.settings.accent, delta, 0, 4),
            9 => adjust_usize(&mut self.settings.border_style, delta, 0, 4),
            10 => adjust_usize(&mut self.settings.animation_speed, delta, 0, 2),
            11 => adjust_u16(&mut self.settings.panel_width, delta * 5, 50, 600),
            12 => {
                self.settings.theme = 0;
                self.settings.accent = 0;
                self.settings.border_style = 0;
                self.settings.animation_speed = 1;
                self.settings.show_header = true;
                self.settings.transparent = true;
            }
            _ => {}
        }
        self.settings_dirty = true;
        if old_icon_size != (self.settings.icon_width, self.settings.icon_height) {
            rebuild_images(&mut self.applications, &self.picker, &self.settings)
                .expect("valid icon dimensions must produce image protocols");
        }
    }
}

fn main() -> Result<()> {
    let applications = discover_applications()?;

    if env::args().any(|argument| argument == "--list") {
        for application in applications {
            println!(
                "{}\t{}",
                application.name,
                application.desktop_file.display()
            );
        }
        return Ok(());
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("tui-launcher must run inside a terminal (use --list for non-interactive output)");
    }
    if applications.is_empty() {
        bail!("no visible desktop applications were found");
    }

    let selected = run_tui(applications)?;
    if let Some(desktop_file) = selected {
        launch(&desktop_file)?;
    }
    Ok(())
}

fn run_tui(mut applications: Vec<Application>) -> Result<Option<PathBuf>> {
    enable_raw_mode().context("failed to enter raw terminal mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to initialize terminal screen")?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;
    terminal.clear()?;

    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let settings = Settings::load();
    load_images(&mut applications, &picker, &settings);
    let mut launcher = Launcher::new(applications, settings, picker);

    let result = event_loop(&mut terminal, &mut launcher);
    let restore_result = restore_terminal(&mut terminal);
    restore_result?;
    if launcher.settings_dirty {
        launcher.settings.save()?;
    }
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    launcher: &mut Launcher,
) -> Result<Option<PathBuf>> {
    loop {
        launcher.tick();
        terminal.draw(|frame| draw(frame, launcher))?;
        let poll_timeout = if launcher.is_animating() {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(100)
        };
        if !event::poll(poll_timeout)? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if launcher.settings_open {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('S') => {
                            launcher.settings_open = false;
                            terminal.clear()?;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            launcher.settings_selected =
                                wrapped_index(launcher.settings_selected, -1, SETTINGS_COUNT);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            launcher.settings_selected =
                                wrapped_index(launcher.settings_selected, 1, SETTINGS_COUNT);
                        }
                        KeyCode::Left | KeyCode::Char('h') => launcher.adjust_setting(-1),
                        KeyCode::Right
                        | KeyCode::Char('l')
                        | KeyCode::Enter
                        | KeyCode::Char(' ') => {
                            launcher.adjust_setting(1);
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            launcher.settings = Settings::default();
                            launcher.settings_dirty = true;
                            rebuild_images(
                                &mut launcher.applications,
                                &launcher.picker,
                                &launcher.settings,
                            )?;
                        }
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                    KeyCode::Char('s') | KeyCode::Char('S') => {
                        launcher.settings_open = true;
                        launcher.motion = 0.0;
                        terminal.clear()?;
                    }
                    KeyCode::Enter => {
                        return Ok(launcher
                            .applications
                            .get(launcher.selected)
                            .map(|application| application.desktop_file.clone()));
                    }
                    KeyCode::Left | KeyCode::Char('h') => launcher.move_selection(-1),
                    KeyCode::Right | KeyCode::Char('l') => launcher.move_selection(1),
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp => {
                        launcher.move_selection(-5)
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => {
                        launcher.move_selection(5)
                    }
                    KeyCode::Home => launcher.set_selection(0),
                    KeyCode::End => {
                        launcher.set_selection(launcher.applications.len().saturating_sub(1))
                    }
                    _ => {}
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Moved => {}
                MouseEventKind::Down(MouseButton::Left) => {
                    let previously_selected = launcher.selected;
                    if launcher.select_at(mouse.column, mouse.row)
                        && launcher.selected == previously_selected
                    {
                        return Ok(launcher
                            .applications
                            .get(launcher.selected)
                            .map(|application| application.desktop_file.clone()));
                    }
                }
                MouseEventKind::ScrollUp => launcher.move_selection(-1),
                MouseEventKind::ScrollDown => launcher.move_selection(1),
                _ => {}
            },
            _ => {}
        }
    }
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to leave raw terminal mode")?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )
    .context("failed to restore terminal screen")?;
    terminal.show_cursor()?;
    Ok(())
}

fn draw(frame: &mut Frame<'_>, launcher: &mut Launcher) {
    let theme = theme(launcher.settings.theme);
    let surface = surface_style(&launcher.settings, theme);
    frame.render_widget(Block::new().style(surface), frame.area());
    let panel = centered_panel(frame.area(), launcher.settings.panel_width);
    let accent = theme.accents[launcher.settings.accent];
    frame.render_widget(Clear, panel);
    let borderless = launcher.settings.border_style == 4;
    let mut panel_block = Block::new().style(surface);
    if !borderless {
        panel_block = panel_block
            .borders(Borders::ALL)
            .border_type(border_type(launcher.settings.border_style))
            .border_style(Style::new().fg(theme.border));
    }
    frame.render_widget(panel_block, panel);
    let inner = if borderless {
        Rect::new(
            panel.x.saturating_add(1),
            panel.y,
            panel.width.saturating_sub(2),
            panel.height,
        )
    } else {
        Rect::new(
            panel.x.saturating_add(2),
            panel.y.saturating_add(1),
            panel.width.saturating_sub(4),
            panel.height.saturating_sub(2),
        )
    };
    launcher.carousel_areas.clear();

    if launcher.settings_open {
        draw_settings(frame, launcher, accent, theme);
        return;
    }

    let header_rows = if launcher.settings.show_header && inner.height >= 5 {
        frame.render_widget(
            Paragraph::new("TUI-LAUNCHER")
                .style(Style::new().fg(accent).add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center),
            Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1),
        );
        3
    } else {
        0
    };

    if launcher.settings.show_index && inner.height >= header_rows + 3 {
        let index = format!(
            "{:02} / {:02}",
            launcher.selected + 1,
            launcher.applications.len()
        );
        frame.render_widget(
            Paragraph::new(index)
                .style(surface.fg(theme.muted))
                .alignment(Alignment::Right),
            Rect::new(inner.x, inner.y.saturating_add(header_rows), inner.width, 1),
        );
    }

    let index_rows =
        u16::from(launcher.settings.show_index && inner.height >= header_rows.saturating_add(3));
    let footer_rows = u16::from(launcher.settings.show_footer && inner.height >= 8);
    let stage = Rect::new(
        inner.x,
        inner
            .y
            .saturating_add(header_rows)
            .saturating_add(index_rows),
        inner.width,
        inner
            .height
            .saturating_sub(header_rows + index_rows + footer_rows),
    );
    let icon_height = launcher
        .settings
        .icon_height
        .min(stage.height.saturating_sub(1));
    let group_height = icon_height.saturating_add(1);
    let group_y = stage.y + stage.height.saturating_sub(group_height) / 2;
    draw_icon_carousel(
        frame,
        launcher,
        Rect::new(inner.x, group_y, inner.width, icon_height.saturating_add(1)),
        accent,
        theme,
        true,
    );

    if footer_rows == 1 {
        let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        let help = if footer.width < 90 {
            "←/→ browse  ↑/↓ jump  ↵ open  s setup"
        } else {
            "h l / ← →  browse     j k / ↑ ↓  jump     enter  open     s  setup     esc  close"
        };
        frame.render_widget(
            Paragraph::new(help)
                .style(surface.fg(theme.muted))
                .alignment(Alignment::Center),
            footer,
        );
    }
}

fn draw_icon_carousel(
    frame: &mut Frame<'_>,
    launcher: &mut Launcher,
    area: Rect,
    accent: Color,
    theme: Theme,
    show_selected_name: bool,
) {
    if area.width < 18 || area.height < 4 {
        return;
    }
    let neighbors = visible_neighbor_count(area.width, launcher.settings.neighbor_count);
    let slots = neighbors * 2 + 1;
    let widths = equal_slot_widths(area.width, slots);
    let base_positions = slot_positions(area, &widths);
    let average_slot_width = f32::from(area.width) / slots as f32;
    let shift = launcher.motion * average_slot_width;
    for slot in 0..slots {
        let offset = slot as isize - neighbors as isize;
        let index = wrapped_index(launcher.selected, offset, launcher.applications.len());
        let selected = offset == 0;
        let width = widths[slot];
        let animated_x = f32::from(base_positions[slot]) + shift;
        let Some(slot_area) =
            clipped_rect(animated_x.round() as i32, area.y, width, area.height, area)
        else {
            continue;
        };
        launcher.carousel_areas.push((index, slot_area));

        let maximum = Size::new(
            slot_area.width.saturating_sub(2),
            area.height.saturating_sub(1),
        );
        if let Some(image) = image_that_fits(&launcher.applications[index], maximum) {
            let size = image.size();
            let image_area = Rect::new(
                slot_area.x + slot_area.width.saturating_sub(size.width) / 2,
                slot_area.y + area.height.saturating_sub(1 + size.height) / 2,
                size.width,
                size.height,
            );
            frame.render_widget(Image::new(image), image_area);
        }

        let max_width = usize::from(slot_area.width.saturating_sub(if selected { 4 } else { 2 }));
        let name = truncate(&launcher.applications[index].name, max_width);
        let line = if selected && show_selected_name {
            Line::from(Span::styled(
                format!("[ {} ]", name.to_uppercase()),
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ))
        } else if selected {
            Line::from(Span::styled("--", Style::new().fg(accent)))
        } else {
            Line::from(Span::styled(name, Style::new().fg(theme.muted)))
        };
        frame.render_widget(
            Paragraph::new(line).alignment(Alignment::Center),
            Rect::new(
                slot_area.x,
                slot_area.bottom().saturating_sub(1),
                slot_area.width,
                1,
            ),
        );
    }
}

fn centered_panel(area: Rect, configured_width: u16) -> Rect {
    let horizontal_margin = if area.width < 80 { 0 } else { 2 };
    let width = area
        .width
        .saturating_sub(horizontal_margin * 2)
        .min(configured_width)
        .max(1);
    let height = area.height.clamp(1, 30);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn draw_settings(frame: &mut Frame<'_>, launcher: &Launcher, accent: Color, theme: Theme) {
    let width = 54.min(frame.area().width.saturating_sub(4)).max(1);
    let height = 16.min(frame.area().height.saturating_sub(2)).max(1);
    let area = Rect::new(
        frame.area().x + frame.area().width.saturating_sub(width) / 2,
        frame.area().y + frame.area().height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::new()
            .title(" Settings ")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::new().fg(accent))
            .style(Style::new().bg(theme.panel)),
        area,
    );
    let values = [
        format!("Neighbors         {}", launcher.settings.neighbor_count),
        format!("Icon width        {}", launcher.settings.icon_width),
        format!("Icon height       {}", launcher.settings.icon_height),
        format!("Index             {}", on_off(launcher.settings.show_index)),
        format!(
            "Footer            {}",
            on_off(launcher.settings.show_footer)
        ),
        format!(
            "Header            {}",
            on_off(launcher.settings.show_header)
        ),
        format!(
            "Transparent       {}",
            on_off(launcher.settings.transparent)
        ),
        format!("Theme             {}", theme.name),
        format!(
            "Accent            {}",
            accent_name(launcher.settings.accent)
        ),
        format!(
            "Border            {}",
            border_name(launcher.settings.border_style)
        ),
        format!(
            "Motion            {}",
            animation_name(launcher.settings.animation_speed)
        ),
        format!("Panel width       {}", launcher.settings.panel_width),
        "Reset appearance".to_owned(),
    ];
    let inner = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 13);
    let lines = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            Line::styled(
                value.as_str(),
                if index == launcher.settings_selected {
                    Style::new()
                        .fg(theme.background)
                        .bg(accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(theme.text).bg(theme.panel)
                },
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
    let help = Rect::new(
        area.x + 1,
        area.bottom().saturating_sub(2),
        area.width - 2,
        1,
    );
    frame.render_widget(
        Paragraph::new("↑↓ select   ←→ change   R reset   S/Esc close")
            .style(Style::new().fg(theme.muted).bg(theme.panel))
            .alignment(Alignment::Center),
        help,
    );
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn surface_style(settings: &Settings, theme: Theme) -> Style {
    if settings.transparent {
        Style::new().fg(theme.text)
    } else {
        Style::new().fg(theme.text).bg(theme.panel)
    }
}

fn legacy_config_version() -> u8 {
    0
}

fn accent_name(index: usize) -> &'static str {
    match index {
        1 => "green",
        2 => "amber",
        3 => "violet",
        4 => "cyan",
        _ => "blue",
    }
}

fn theme_count() -> usize {
    12
}

fn theme(index: usize) -> Theme {
    match index {
        1 => Theme {
            name: "Nord",
            background: Color::Rgb(0x24, 0x2a, 0x35),
            panel: Color::Rgb(0x2e, 0x34, 0x40),
            text: Color::Rgb(0xd8, 0xde, 0xe9),
            muted: Color::Rgb(0x81, 0xa1, 0xc1),
            border: Color::Rgb(0x4c, 0x56, 0x6a),
            accents: [
                Color::Rgb(0x88, 0xc0, 0xd0),
                Color::Rgb(0xa3, 0xbe, 0x8c),
                Color::Rgb(0xeb, 0xcb, 0x8b),
                Color::Rgb(0xb4, 0x8e, 0xad),
                Color::Rgb(0x81, 0xa1, 0xc1),
            ],
        },
        2 => Theme {
            name: "Catppuccin",
            background: Color::Rgb(0x11, 0x11, 0x1b),
            panel: Color::Rgb(0x1e, 0x1e, 0x2e),
            text: Color::Rgb(0xcd, 0xd6, 0xf4),
            muted: Color::Rgb(0x7f, 0x84, 0xa2),
            border: Color::Rgb(0x45, 0x47, 0x5a),
            accents: [
                Color::Rgb(0x89, 0xb4, 0xfa),
                Color::Rgb(0xa6, 0xe3, 0xa1),
                Color::Rgb(0xf9, 0xe2, 0xaf),
                Color::Rgb(0xcb, 0xa6, 0xf7),
                Color::Rgb(0x94, 0xe2, 0xd5),
            ],
        },
        3 => Theme {
            name: "Gruvbox",
            background: Color::Rgb(0x1d, 0x20, 0x21),
            panel: Color::Rgb(0x28, 0x28, 0x28),
            text: Color::Rgb(0xeb, 0xdb, 0xb2),
            muted: Color::Rgb(0x92, 0x83, 0x74),
            border: Color::Rgb(0x50, 0x49, 0x45),
            accents: [
                Color::Rgb(0x83, 0xa5, 0x98),
                Color::Rgb(0xb8, 0xbb, 0x26),
                Color::Rgb(0xfa, 0xbd, 0x2f),
                Color::Rgb(0xd3, 0x86, 0x9b),
                Color::Rgb(0x8e, 0xc0, 0x7c),
            ],
        },
        4 => Theme {
            name: "Everforest",
            background: Color::Rgb(0x1e, 0x23, 0x20),
            panel: Color::Rgb(0x27, 0x2e, 0x2b),
            text: Color::Rgb(0xd3, 0xc6, 0xaa),
            muted: Color::Rgb(0x85, 0x92, 0x89),
            border: Color::Rgb(0x4f, 0x58, 0x53),
            accents: [
                Color::Rgb(0x7f, 0xbb, 0xb3),
                Color::Rgb(0xa7, 0xc0, 0x80),
                Color::Rgb(0xdb, 0xbc, 0x7f),
                Color::Rgb(0xd6, 0x99, 0xb6),
                Color::Rgb(0x83, 0xc0, 0x92),
            ],
        },
        5 => Theme {
            name: "Mono",
            background: Color::Rgb(0x08, 0x08, 0x08),
            panel: Color::Rgb(0x10, 0x10, 0x10),
            text: Color::Rgb(0xe6, 0xe6, 0xe6),
            muted: Color::Rgb(0x77, 0x77, 0x77),
            border: Color::Rgb(0x55, 0x55, 0x55),
            accents: [
                Color::Rgb(0xff, 0xff, 0xff),
                Color::Rgb(0xc8, 0xc8, 0xc8),
                Color::Rgb(0xe0, 0xe0, 0xe0),
                Color::Rgb(0xb0, 0xb0, 0xb0),
                Color::Rgb(0xf2, 0xf2, 0xf2),
            ],
        },
        6 => Theme {
            name: "Dracula",
            background: Color::Rgb(0x28, 0x2a, 0x36),
            panel: Color::Rgb(0x21, 0x22, 0x2c),
            text: Color::Rgb(0xf8, 0xf8, 0xf2),
            muted: Color::Rgb(0x62, 0x72, 0xa4),
            border: Color::Rgb(0x44, 0x47, 0x5a),
            accents: [
                Color::Rgb(0xbd, 0x93, 0xf9),
                Color::Rgb(0x50, 0xfa, 0x7b),
                Color::Rgb(0xf1, 0xfa, 0x8c),
                Color::Rgb(0xff, 0x79, 0xc6),
                Color::Rgb(0x8b, 0xe9, 0xfd),
            ],
        },
        7 => Theme {
            name: "Rose Pine",
            background: Color::Rgb(0x19, 0x17, 0x24),
            panel: Color::Rgb(0x1f, 0x1d, 0x2e),
            text: Color::Rgb(0xe0, 0xde, 0xf4),
            muted: Color::Rgb(0x90, 0x8c, 0xaa),
            border: Color::Rgb(0x40, 0x3d, 0x52),
            accents: [
                Color::Rgb(0xc4, 0xa7, 0xe7),
                Color::Rgb(0x9c, 0xcf, 0xd8),
                Color::Rgb(0xf6, 0xc1, 0x77),
                Color::Rgb(0xeb, 0x6f, 0x92),
                Color::Rgb(0x31, 0x74, 0x8f),
            ],
        },
        8 => Theme {
            name: "Kanagawa",
            background: Color::Rgb(0x1f, 0x1f, 0x28),
            panel: Color::Rgb(0x2a, 0x2a, 0x37),
            text: Color::Rgb(0xdc, 0xd7, 0xba),
            muted: Color::Rgb(0x72, 0x71, 0x69),
            border: Color::Rgb(0x54, 0x54, 0x6d),
            accents: [
                Color::Rgb(0x7e, 0x9c, 0xd8),
                Color::Rgb(0x98, 0xbb, 0x6c),
                Color::Rgb(0xe6, 0xc3, 0x84),
                Color::Rgb(0xd2, 0x7e, 0x99),
                Color::Rgb(0x7a, 0xa8, 0x9f),
            ],
        },
        9 => Theme {
            name: "Solarized Dark",
            background: Color::Rgb(0x00, 0x2b, 0x36),
            panel: Color::Rgb(0x07, 0x36, 0x42),
            text: Color::Rgb(0xee, 0xe8, 0xd5),
            muted: Color::Rgb(0x93, 0xa1, 0xa1),
            border: Color::Rgb(0x58, 0x6e, 0x75),
            accents: [
                Color::Rgb(0x26, 0x8b, 0xd2),
                Color::Rgb(0x85, 0x99, 0x00),
                Color::Rgb(0xb5, 0x89, 0x00),
                Color::Rgb(0xd3, 0x36, 0x82),
                Color::Rgb(0x2a, 0xa1, 0x98),
            ],
        },
        10 => Theme {
            name: "One Half",
            background: Color::Rgb(0x28, 0x2c, 0x34),
            panel: Color::Rgb(0x31, 0x35, 0x3f),
            text: Color::Rgb(0xdc, 0xdf, 0xe4),
            muted: Color::Rgb(0x5c, 0x63, 0x70),
            border: Color::Rgb(0x3e, 0x44, 0x51),
            accents: [
                Color::Rgb(0x61, 0xaf, 0xef),
                Color::Rgb(0x98, 0xc3, 0x79),
                Color::Rgb(0xe5, 0xc0, 0x7b),
                Color::Rgb(0xc6, 0x78, 0xdd),
                Color::Rgb(0x56, 0xb6, 0xc2),
            ],
        },
        11 => Theme {
            name: "GitHub Dark",
            background: Color::Rgb(0x0d, 0x11, 0x17),
            panel: Color::Rgb(0x16, 0x1b, 0x22),
            text: Color::Rgb(0xe6, 0xed, 0xf3),
            muted: Color::Rgb(0x8b, 0x94, 0x9e),
            border: Color::Rgb(0x30, 0x36, 0x3d),
            accents: [
                Color::Rgb(0x58, 0xa6, 0xff),
                Color::Rgb(0x3f, 0xb9, 0x50),
                Color::Rgb(0xd2, 0x99, 0x22),
                Color::Rgb(0xbc, 0x8c, 0xff),
                Color::Rgb(0x39, 0xc5, 0xcf),
            ],
        },
        _ => Theme {
            name: "Tokyo Night",
            background: Color::Rgb(0x12, 0x13, 0x1a),
            panel: Color::Rgb(0x1a, 0x1b, 0x26),
            text: Color::Rgb(0xa9, 0xb1, 0xd6),
            muted: Color::Rgb(0x78, 0x7c, 0x99),
            border: Color::Rgb(0x56, 0x5f, 0x89),
            accents: [
                Color::Rgb(0x7a, 0xa2, 0xf7),
                Color::Rgb(0x9e, 0xce, 0x6a),
                Color::Rgb(0xe0, 0xaf, 0x68),
                Color::Rgb(0xbb, 0x9a, 0xf7),
                Color::Rgb(0x7d, 0xcf, 0xff),
            ],
        },
    }
}

fn border_type(index: usize) -> BorderType {
    match index {
        1 => BorderType::Rounded,
        2 => BorderType::Double,
        3 => BorderType::Thick,
        _ => BorderType::Plain,
    }
}

fn border_name(index: usize) -> &'static str {
    match index {
        1 => "rounded",
        2 => "double",
        3 => "thick",
        4 => "none",
        _ => "plain",
    }
}

fn animation_name(index: usize) -> &'static str {
    match index {
        0 => "slow",
        2 => "fast",
        _ => "normal",
    }
}

fn animation_decay(index: usize) -> f32 {
    match index {
        0 => 6.5,
        2 => 12.0,
        _ => 8.5,
    }
}

fn circular_delta(current: usize, target: usize, length: usize) -> isize {
    if length == 0 {
        return 0;
    }
    let forward = (target + length - current) % length;
    let backward = forward as isize - length as isize;
    if forward <= length / 2 {
        forward as isize
    } else {
        backward
    }
}

fn slot_positions(area: Rect, widths: &[u16]) -> Vec<u16> {
    let mut x = area.x;
    widths
        .iter()
        .map(|width| {
            let position = x;
            x = x.saturating_add(*width);
            position
        })
        .collect()
}

fn equal_slot_widths(total_width: u16, slots: usize) -> Vec<u16> {
    let slots = slots.max(1);
    let base = total_width / slots as u16;
    let remainder = total_width % slots as u16;
    (0..slots)
        .map(|slot| base + u16::from((slot as u16) < remainder))
        .collect()
}

fn clipped_rect(x: i32, y: u16, width: u16, height: u16, bounds: Rect) -> Option<Rect> {
    let left = x.max(i32::from(bounds.x));
    let right = (x + i32::from(width)).min(i32::from(bounds.right()));
    if right <= left {
        return None;
    }
    Some(Rect::new(
        u16::try_from(left).ok()?,
        y,
        u16::try_from(right - left).ok()?,
        height,
    ))
}

fn visible_neighbor_count(width: u16, configured: usize) -> usize {
    configured.min(match width {
        0..=45 => 1,
        46..=84 => 2,
        _ => 3,
    })
}

fn discover_applications() -> Result<Vec<Application>> {
    let locales = get_languages_from_env();
    let desktops = current_desktop().unwrap_or_default();
    let mut seen = HashSet::new();
    let mut applications = Vec::new();

    for path in freedesktop_desktop_entry::Iter::new(default_paths()) {
        let Ok(entry) = DesktopEntry::from_path(&path, Some(&locales)) else {
            continue;
        };
        if !seen.insert(entry.id().to_ascii_lowercase()) {
            continue;
        }
        if !entry_is_visible(&entry, &desktops) {
            continue;
        }

        let Some(name) = entry.name(&locales).map(|name| name.into_owned()) else {
            continue;
        };
        let icon_path = entry.icon().and_then(resolve_icon);
        applications.push(Application {
            id: entry.id().to_owned(),
            name,
            desktop_file: path,
            icon_path,
            icon_source: None,
            focus_image: None,
            thumbnail: None,
            compact_thumbnail: None,
        });
    }

    applications.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(applications)
}

fn entry_is_visible(entry: &DesktopEntry, desktops: &[String]) -> bool {
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

fn desktop_matches(candidate: &str, desktops: &[String]) -> bool {
    desktops
        .iter()
        .any(|desktop| candidate.eq_ignore_ascii_case(desktop))
}

fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file();
    }
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|directory| directory.join(command).is_file())
    })
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

fn load_images(applications: &mut [Application], picker: &Picker, settings: &Settings) {
    for application in &mut *applications {
        let source = application
            .icon_path
            .as_deref()
            .and_then(load_icon)
            .unwrap_or_else(|| fallback_icon(&application.name));
        application.icon_source = Some(normalize_icon(source));
    }
    rebuild_images(applications, picker, settings)
        .expect("fallback image renderer must support valid icon dimensions");
}

fn rebuild_images(
    applications: &mut [Application],
    picker: &Picker,
    settings: &Settings,
) -> Result<()> {
    let fallback_picker = Picker::halfblocks();
    let thumbnail_size = thumbnail_size(settings);
    let compact_size = Size::new(5, 3);
    for application in applications {
        let Some(source) = application.icon_source.clone() else {
            continue;
        };
        application.focus_image = picker
            .new_protocol(
                source.clone(),
                Size::new(settings.icon_width, settings.icon_height),
                Resize::Fit(None),
            )
            .or_else(|_| {
                fallback_picker.new_protocol(
                    source.clone(),
                    Size::new(settings.icon_width, settings.icon_height),
                    Resize::Fit(None),
                )
            })
            .ok();
        application.thumbnail = picker
            .new_protocol(source.clone(), thumbnail_size, Resize::Fit(None))
            .or_else(|_| {
                fallback_picker.new_protocol(source.clone(), thumbnail_size, Resize::Fit(None))
            })
            .ok();
        application.compact_thumbnail = picker
            .new_protocol(source.clone(), compact_size, Resize::Fit(None))
            .or_else(|_| fallback_picker.new_protocol(source, compact_size, Resize::Fit(None)))
            .ok();
        if application.focus_image.is_none()
            || application.thumbnail.is_none()
            || application.compact_thumbnail.is_none()
        {
            bail!("failed to render icon for {}", application.name);
        }
    }
    Ok(())
}

fn thumbnail_size(settings: &Settings) -> Size {
    Size::new(
        settings.icon_width.div_ceil(2).clamp(6, 12),
        settings.icon_height.div_ceil(2).clamp(3, 6),
    )
}

fn image_that_fits(application: &Application, maximum: Size) -> Option<&Protocol> {
    [
        application.focus_image.as_ref(),
        application.thumbnail.as_ref(),
        application.compact_thumbnail.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find(|image| {
        let size = image.size();
        size.width <= maximum.width && size.height <= maximum.height
    })
}

fn load_icon(path: &Path) -> Option<DynamicImage> {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        return load_svg(path);
    }
    image::open(path).ok()
}

fn normalize_icon(source: DynamicImage) -> DynamicImage {
    let size = source.width().max(source.height()).max(1);
    let mut canvas = DynamicImage::new_rgba8(size, size);
    image::imageops::overlay(
        &mut canvas,
        &source,
        i64::from((size - source.width()) / 2),
        i64::from((size - source.height()) / 2),
    );
    canvas.resize_exact(256, 256, FilterType::Lanczos3)
}

fn load_svg(path: &Path) -> Option<DynamicImage> {
    let options = resvg::usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..resvg::usvg::Options::default()
    };
    let data = std::fs::read(path).ok()?;
    let tree = resvg::usvg::Tree::from_data(&data, &options).ok()?;
    let size = tree.size();
    let scale = (256.0 / size.width()).min(256.0 / size.height());
    let width = (size.width() * scale).round().max(1.0) as u32;
    let height = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, pixmap.take())?;
    Some(DynamicImage::ImageRgba8(image))
}

fn fallback_icon(name: &str) -> DynamicImage {
    let hash = name.bytes().fold(0x007a_a2f7_u32, |hash, byte| {
        hash.rotate_left(5) ^ u32::from(byte)
    });
    let color = Rgba([
        80 + (hash & 0x7f) as u8,
        80 + ((hash >> 8) & 0x7f) as u8,
        100 + ((hash >> 16) & 0x7f) as u8,
        255,
    ]);
    let mut image = ImageBuffer::from_pixel(128, 128, Rgba([0x1a, 0x1b, 0x26, 255]));
    for y in 16..112 {
        for x in 16..112 {
            if (24..104).contains(&x) && (24..104).contains(&y) || (x + y) % 13 < 8 {
                image.put_pixel(x, y, color);
            }
        }
    }
    DynamicImage::ImageRgba8(image).resize(128, 128, FilterType::Triangle)
}

fn launch(desktop_file: &Path) -> Result<()> {
    Command::new("gio")
        .arg("launch")
        .arg(desktop_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch {}", desktop_file.display()))?;
    Ok(())
}

fn wrapped_index(current: usize, delta: isize, length: usize) -> usize {
    if length == 0 {
        return 0;
    }
    (current as isize + delta).rem_euclid(length as isize) as usize
}

fn adjust_usize(value: &mut usize, delta: isize, minimum: usize, maximum: usize) {
    *value = (*value as isize + delta).clamp(minimum as isize, maximum as isize) as usize;
}

fn adjust_u16(value: &mut u16, delta: isize, minimum: u16, maximum: u16) {
    *value = (isize::try_from(*value).unwrap_or_default() + delta).clamp(
        isize::try_from(minimum).unwrap_or_default(),
        isize::try_from(maximum).unwrap_or_default(),
    ) as u16;
}

fn point_in_rect(column: u16, row: u16, area: Rect) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn truncate(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_owned();
    }
    if max_width <= 1 {
        return "…".chars().take(max_width).collect();
    }

    let mut output = String::new();
    for character in value.chars() {
        let candidate = format!("{output}{character}");
        if UnicodeWidthStr::width(candidate.as_str()) >= max_width {
            break;
        }
        output.push(character);
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(contents: &str) -> DesktopEntry {
        DesktopEntry::from_str("test.desktop", contents, None::<&[&str]>).unwrap()
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        assert_eq!(wrapped_index(0, -1, 7), 6);
        assert_eq!(wrapped_index(6, 1, 7), 0);
        assert_eq!(wrapped_index(1, 10, 7), 4);
    }

    #[test]
    fn hidden_and_non_application_entries_are_filtered() {
        let hidden =
            entry("[Desktop Entry]\nType=Application\nName=Hidden\nExec=true\nHidden=true");
        let link = entry("[Desktop Entry]\nType=Link\nName=Link\nURL=https://example.com");
        assert!(!entry_is_visible(&hidden, &["hyprland".into()]));
        assert!(!entry_is_visible(&link, &["hyprland".into()]));
    }

    #[test]
    fn desktop_restrictions_are_case_insensitive() {
        let allowed = entry(
            "[Desktop Entry]\nType=Application\nName=Allowed\nExec=true\nOnlyShowIn=Hyprland;",
        );
        let denied =
            entry("[Desktop Entry]\nType=Application\nName=Denied\nExec=true\nNotShowIn=HYPRLAND;");
        assert!(entry_is_visible(&allowed, &["hyprland".into()]));
        assert!(!entry_is_visible(&denied, &["hyprland".into()]));
    }

    #[test]
    fn truncation_respects_display_width() {
        assert_eq!(truncate("Chromium", 6), "Chrom…");
        assert_eq!(truncate("Files", 8), "Files");
        assert!(UnicodeWidthStr::width(truncate("非常に長い名前", 5).as_str()) <= 5);
    }

    #[test]
    fn rectangle_hit_testing_excludes_far_edges() {
        let area = Rect::new(10, 5, 20, 8);
        assert!(point_in_rect(10, 5, area));
        assert!(point_in_rect(29, 12, area));
        assert!(!point_in_rect(30, 12, area));
        assert!(!point_in_rect(29, 13, area));
    }

    #[test]
    fn settings_are_normalized_to_supported_ranges() {
        let mut settings = Settings {
            neighbor_count: 99,
            icon_width: 0,
            icon_height: 50,
            theme: 99,
            accent: 99,
            border_style: 99,
            animation_speed: 99,
            panel_width: 10,
            ..Settings::default()
        };
        settings.normalize();
        assert_eq!(settings.version, CONFIG_VERSION);
        assert_eq!(settings.neighbor_count, 3);
        assert_eq!(settings.icon_width, 12);
        assert_eq!(settings.icon_height, 16);
        assert_eq!(settings.theme, theme_count() - 1);
        assert_eq!(settings.accent, 4);
        assert_eq!(settings.border_style, 4);
        assert_eq!(settings.animation_speed, 2);
        assert_eq!(settings.panel_width, 50);
    }

    #[test]
    fn numeric_settings_stop_at_their_limits() {
        let mut value = 5;
        adjust_usize(&mut value, -10, 2, 8);
        assert_eq!(value, 2);
        adjust_usize(&mut value, 20, 2, 8);
        assert_eq!(value, 8);

        let mut value = 5_u16;
        adjust_u16(&mut value, -10, 3, 9);
        assert_eq!(value, 3);
    }

    #[test]
    fn accent_palette_has_stable_names() {
        assert_eq!(accent_name(0), "blue");
        assert_eq!(accent_name(4), "cyan");
        assert_eq!(accent_name(99), "blue");
    }

    #[test]
    fn grid_config_is_detected_as_legacy() {
        let settings: Settings =
            toml::from_str("columns = 4\ncard_height = 10\nicon_width = 12\nicon_height = 7\n")
                .unwrap();
        assert_eq!(settings.version, 0);
    }

    #[test]
    fn carousel_collapses_neighbors_responsively() {
        assert_eq!(visible_neighbor_count(40, 3), 1);
        assert_eq!(visible_neighbor_count(70, 3), 2);
        assert_eq!(visible_neighbor_count(100, 3), 3);
        assert_eq!(visible_neighbor_count(100, 1), 1);
    }

    #[test]
    fn thumbnails_scale_from_focus_icons() {
        let settings = Settings::default();
        assert_eq!(thumbnail_size(&settings), Size::new(11, 6));

        let settings = Settings {
            icon_width: 12,
            icon_height: 6,
            ..Settings::default()
        };
        assert_eq!(thumbnail_size(&settings), Size::new(6, 3));
    }

    #[test]
    fn circular_delta_takes_shortest_path() {
        assert_eq!(circular_delta(0, 1, 10), 1);
        assert_eq!(circular_delta(9, 0, 10), 1);
        assert_eq!(circular_delta(0, 9, 10), -1);
    }

    #[test]
    fn animation_speeds_are_ordered() {
        assert!(animation_decay(0) < animation_decay(1));
        assert!(animation_decay(1) < animation_decay(2));
    }

    #[test]
    fn themes_and_borders_have_stable_names() {
        assert_eq!(theme_count(), 12);
        assert_eq!(theme(0).name, "Tokyo Night");
        assert_eq!(theme(5).name, "Mono");
        assert_eq!(theme(6).name, "Dracula");
        assert_eq!(theme(7).name, "Rose Pine");
        assert_eq!(theme(8).name, "Kanagawa");
        assert_eq!(theme(9).name, "Solarized Dark");
        assert_eq!(theme(10).name, "One Half");
        assert_eq!(theme(11).name, "GitHub Dark");
        assert_eq!(border_name(0), "plain");
        assert_eq!(border_name(3), "thick");
        assert_eq!(border_name(4), "none");
    }

    #[test]
    fn clipping_keeps_cards_inside_carousel() {
        let bounds = Rect::new(10, 4, 20, 8);
        assert_eq!(
            clipped_rect(5, 4, 10, 8, bounds),
            Some(Rect::new(10, 4, 5, 8))
        );
        assert_eq!(clipped_rect(31, 4, 4, 8, bounds), None);
    }

    #[test]
    fn equal_slots_use_all_available_width() {
        let widths = equal_slot_widths(52, 5);
        assert_eq!(widths, vec![11, 11, 10, 10, 10]);
        assert_eq!(widths.iter().sum::<u16>(), 52);
    }

    #[test]
    fn icons_are_normalized_to_square_canvas() {
        let source = DynamicImage::new_rgba8(64, 32);
        let normalized = normalize_icon(source);
        assert_eq!(normalized.width(), 256);
        assert_eq!(normalized.height(), 256);
    }

    #[test]
    fn transparent_surface_does_not_set_background() {
        let settings = Settings {
            transparent: true,
            ..Settings::default()
        };
        assert_eq!(surface_style(&settings, theme(0)).bg, None);

        let settings = Settings {
            transparent: false,
            ..Settings::default()
        };
        assert_eq!(surface_style(&settings, theme(0)).bg, Some(theme(0).panel));
    }
}
