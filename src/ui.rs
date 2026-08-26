use std::{
    collections::{HashMap, HashSet},
    f32::consts::PI,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::SystemTime,
};

use chrono::Local;
use image::{DynamicImage, ImageBuffer, Rgba, imageops::FilterType};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Gauge, Padding, Paragraph, Widget},
};
use ratatui_image::{Image, Resize, picker::Picker, protocol::Protocol};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::{PersistentState, Settings, paths},
    input::ButtonGlyphs,
    model::{Action, AppState, LibraryItem, Mode},
    sources::command_exists,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presentation {
    Overlay,
    Fullscreen,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub background: Color,
    pub panel: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub accents: [Color; 5],
}

pub struct ImageCache {
    picker: Picker,
    protocols: HashMap<String, Protocol>,
    generated: HashMap<String, Option<PathBuf>>,
    pending: HashSet<String>,
    generated_tx: Sender<(String, Option<PathBuf>)>,
    generated_rx: Receiver<(String, Option<PathBuf>)>,
}

impl ImageCache {
    pub fn new() -> Self {
        let (generated_tx, generated_rx) = mpsc::channel();
        let picker = if std::env::var_os("TUI_LAUNCHER_FORCE_HALFBLOCKS").is_some() {
            Picker::halfblocks()
        } else {
            Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
        };
        Self {
            picker,
            protocols: HashMap::new(),
            generated: HashMap::new(),
            pending: HashSet::new(),
            generated_tx,
            generated_rx,
        }
    }

    pub fn image<'a>(
        &'a mut self,
        item: &LibraryItem,
        size: Size,
        allow_network: bool,
    ) -> Option<&'a Protocol> {
        self.accept_generated();
        let key = format!("{}:{}x{}", item.id, size.width, size.height);
        if !self.protocols.contains_key(&key) {
            let path = item
                .art
                .clone()
                .or_else(|| self.generated.get(&item.id).cloned().flatten());
            if path.is_none() {
                self.request_generated(item, allow_network);
            }
            let image = path
                .as_deref()
                .and_then(load_icon)
                .unwrap_or_else(|| fallback_icon(&item.title));
            let image = normalize_icon(image);
            let fallback = Picker::halfblocks();
            if let Ok(protocol) = self
                .picker
                .new_protocol(image.clone(), size, Resize::Fit(None))
                .or_else(|_| fallback.new_protocol(image, size, Resize::Fit(None)))
            {
                self.protocols.insert(key.clone(), protocol);
            }
        }
        self.protocols.get(&key)
    }

    pub fn clear(&mut self) {
        self.protocols.clear();
    }

    fn request_generated(&mut self, item: &LibraryItem, allow_network: bool) {
        if self.pending.contains(&item.id) || self.generated.contains_key(&item.id) {
            return;
        }
        let task = match &item.action {
            Action::Open(path) if is_video(path) => GeneratedTask::Video(path.clone()),
            Action::Steam(app_id) if allow_network => GeneratedTask::Steam(*app_id),
            _ => return,
        };
        self.pending.insert(item.id.clone());
        let id = item.id.clone();
        let sender = self.generated_tx.clone();
        thread::spawn(move || {
            let result = match task {
                GeneratedTask::Video(path) => video_thumbnail(&path),
                GeneratedTask::Steam(app_id) => fetch_steam_art(app_id),
            };
            let _ = sender.send((id, result));
        });
    }

    fn accept_generated(&mut self) {
        while let Ok((id, path)) = self.generated_rx.try_recv() {
            self.pending.remove(&id);
            self.generated.insert(id.clone(), path);
            let prefix = format!("{id}:");
            self.protocols.retain(|key, _| !key.starts_with(&prefix));
        }
    }
}

enum GeneratedTask {
    Video(PathBuf),
    Steam(u32),
}

pub fn draw(
    frame: &mut Frame<'_>,
    state: &mut AppState,
    settings: &Settings,
    persisted: &PersistentState,
    presentation: Presentation,
    images: &mut ImageCache,
    glyphs: ButtonGlyphs,
) {
    let theme = theme(settings.theme);
    let accent = theme.accents[settings.accent];
    let area = frame.area();
    let paint_background = presentation == Presentation::Fullscreen || !settings.transparent;
    if paint_background {
        frame.render_widget(Block::new().style(Style::new().bg(theme.background)), area);
    }
    if settings.waves && !settings.reduced_motion {
        frame.render_widget(
            WaveWidget {
                color: theme.border,
                phase: state.last_tick.elapsed().as_secs_f32(),
            },
            area,
        );
    }

    let panel = match presentation {
        Presentation::Fullscreen => area,
        Presentation::Overlay => centered_panel(area, settings.panel_width),
    };
    if presentation == Presentation::Overlay {
        frame.render_widget(Clear, panel);
    }
    let borderless = settings.border_style == 4 || presentation == Presentation::Fullscreen;
    let mut shell = Block::new().style(
        if settings.transparent && presentation == Presentation::Overlay {
            Style::new().fg(theme.text)
        } else {
            Style::new().fg(theme.text).bg(theme.panel)
        },
    );
    if !borderless {
        shell = shell
            .borders(Borders::ALL)
            .border_type(border_type(settings.border_style))
            .border_style(Style::new().fg(theme.border));
    }
    frame.render_widget(shell, panel);
    let inner = if borderless {
        panel.inner(Margin::new(2, 0))
    } else {
        panel.inner(Margin::new(2, 2))
    };
    if inner.width < 30 || inner.height < 10 {
        frame.render_widget(
            Paragraph::new("Terminal too small for the XMB shell")
                .style(Style::new().fg(accent))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let regions = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(inner);
    draw_status(frame, regions[0], state, theme, &glyphs);
    draw_modes(frame, regions[1], state.mode, theme, accent);
    if state.detail_open {
        draw_detail(
            frame, regions[2], state, settings, persisted, images, theme, accent,
        );
    } else {
        draw_library(
            frame, regions[2], state, settings, persisted, images, theme, accent,
        );
    }
    draw_footer(frame, regions[3], state, theme, &glyphs);

    if let Some((message, _)) = &state.toast {
        let width = (UnicodeWidthStr::width(message.as_str()) as u16 + 6)
            .min(inner.width)
            .max(16);
        let toast = Rect::new(
            inner.x + inner.width.saturating_sub(width) / 2,
            inner.bottom().saturating_sub(4),
            width,
            3,
        );
        frame.render_widget(Clear, toast);
        frame.render_widget(
            Paragraph::new(message.as_str())
                .alignment(Alignment::Center)
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().fg(accent)),
                )
                .style(Style::new().fg(theme.text).bg(theme.panel)),
            toast,
        );
    }

    if let Some((id, started)) = &state.hold_started
        && state.selected_item().is_some_and(|item| &item.id == id)
    {
        let ratio = started.elapsed().as_secs_f64().min(1.0);
        let width = inner.width.min(52);
        let hold = Rect::new(
            inner.x + (inner.width - width) / 2,
            inner.bottom().saturating_sub(6),
            width,
            3,
        );
        frame.render_widget(Clear, hold);
        frame.render_widget(
            Gauge::default()
                .block(
                    Block::bordered()
                        .title(" Hold to confirm ")
                        .border_style(Style::new().fg(accent)),
                )
                .gauge_style(Style::new().fg(accent).bg(theme.panel))
                .ratio(ratio),
            hold,
        );
    }
}

fn draw_status(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    theme: Theme,
    glyphs: &ButtonGlyphs,
) {
    let clock = Local::now().format("%a %b %-d   %-I:%M %p").to_string();
    let input = state
        .controller_name
        .as_ref()
        .map(|name| format!("{}  {} confirm", truncate(name, 22), glyphs.confirm))
        .unwrap_or_else(|| format!("KEYBOARD  ·  {}", theme.name));
    let mut context = [
        state.system_status.network.as_deref(),
        state.system_status.volume.as_deref(),
        state.system_status.battery.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if let Some(now) = &state.now_playing {
        context.insert(0, now.title.as_str());
    }
    let context = if context.is_empty() {
        clock
    } else {
        format!("{}  ·  {clock}", context.join("  ·  "))
    };
    let columns =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(area);
    frame.render_widget(
        Paragraph::new(input).style(Style::new().fg(theme.muted)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(context)
            .style(Style::new().fg(theme.muted))
            .alignment(Alignment::Right),
        columns[1],
    );
}

fn draw_modes(frame: &mut Frame<'_>, area: Rect, active: Mode, theme: Theme, accent: Color) {
    let lines = Mode::ALL
        .into_iter()
        .map(|mode| {
            let label = if area.width < 78 {
                mode.glyph().to_owned()
            } else {
                format!("{} {}", mode.glyph(), mode.title())
            };
            Span::styled(
                format!("  {label}  "),
                if mode == active {
                    Style::new().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(theme.muted)
                },
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(lines)).alignment(Alignment::Center),
        area,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_library(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    settings: &Settings,
    persisted: &PersistentState,
    images: &mut ImageCache,
    theme: Theme,
    accent: Color,
) {
    let items = state.mode_items();
    if items.is_empty() {
        let message = match state.mode {
            Mode::Games => "No installed Steam games found",
            Mode::Media => "No supported media found in the configured folders",
            _ => "Nothing is available in this mode",
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::new().fg(theme.muted))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }
    let columns = Layout::horizontal([
        Constraint::Percentage(if area.width < 80 { 45 } else { 34 }),
        Constraint::Percentage(if area.width < 80 { 55 } else { 66 }),
    ])
    .split(area);
    draw_rail(frame, columns[0], state, persisted, theme, accent);
    draw_focus(
        frame,
        columns[1],
        state.selected_item(),
        settings,
        images,
        theme,
        accent,
    );
}

fn draw_rail(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    persisted: &PersistentState,
    theme: Theme,
    accent: Color,
) {
    let items = state.mode_items();
    let selected = state.selected_index();
    let visible = usize::from(area.height.saturating_sub(3)).clamp(1, 11);
    let start = selected
        .saturating_sub(visible / 2)
        .min(items.len().saturating_sub(visible));
    let mut lines = Vec::new();
    let mut last_section = "";
    for (index, item) in items.iter().enumerate().skip(start).take(visible) {
        let section = if persisted.favorites.contains(&item.id) {
            "FAVORITES"
        } else if persisted.recent_position(state.mode, &item.id).is_some() {
            "RECENT"
        } else {
            "ALL"
        };
        if section != last_section {
            lines.push(Line::styled(
                format!("  {section}"),
                Style::new().fg(theme.border).add_modifier(Modifier::BOLD),
            ));
            last_section = section;
        }
        let marker = if persisted.favorites.contains(&item.id) {
            "★"
        } else if item.available {
            "•"
        } else {
            "×"
        };
        lines.push(Line::from(vec![
            Span::styled(
                if index == selected { "  › " } else { "    " },
                Style::new().fg(accent),
            ),
            Span::styled(
                format!(
                    "{marker} {}",
                    truncate(&item.title, usize::from(area.width.saturating_sub(8)))
                ),
                if index == selected {
                    Style::new().fg(accent).add_modifier(Modifier::BOLD)
                } else if item.available {
                    Style::new().fg(theme.text)
                } else {
                    Style::new().fg(theme.muted)
                },
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::new().padding(Padding::vertical(1))),
        area,
    );
}

fn draw_focus(
    frame: &mut Frame<'_>,
    area: Rect,
    item: Option<&LibraryItem>,
    settings: &Settings,
    images: &mut ImageCache,
    theme: Theme,
    accent: Color,
) {
    let Some(item) = item else {
        return;
    };
    let rows = Layout::vertical([Constraint::Min(6), Constraint::Length(4)]).split(area);
    let maximum = Size::new(
        rows[0]
            .width
            .saturating_sub(4)
            .min(settings.icon_width.max(18)),
        rows[0]
            .height
            .saturating_sub(1)
            .min(settings.icon_height.max(10)),
    );
    if let Some(image) = images.image(item, maximum, settings.network_artwork) {
        let size = image.size();
        let image_area = Rect::new(
            rows[0].x + rows[0].width.saturating_sub(size.width) / 2,
            rows[0].y + rows[0].height.saturating_sub(size.height) / 2,
            size.width,
            size.height,
        );
        frame.render_widget(Image::new(image), image_area);
    }
    let subtitle = item.unavailable_reason.as_deref().unwrap_or(&item.subtitle);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                truncate(&item.title, usize::from(rows[1].width.saturating_sub(2))).to_uppercase(),
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                truncate(subtitle, usize::from(rows[1].width.saturating_sub(2))),
                Style::new().fg(if item.available {
                    theme.text
                } else {
                    Color::Red
                }),
            ),
        ])
        .alignment(Alignment::Center),
        rows[1],
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    settings: &Settings,
    persisted: &PersistentState,
    images: &mut ImageCache,
    theme: Theme,
    accent: Color,
) {
    let Some(item) = state.selected_item() else {
        return;
    };
    let card = centered_panel(area, area.width.min(96));
    frame.render_widget(Clear, card);
    frame.render_widget(
        Block::bordered()
            .title(" DETAILS ")
            .title_alignment(Alignment::Center)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(accent))
            .style(Style::new().fg(theme.text).bg(theme.panel)),
        card,
    );
    let inner = card.inner(Margin::new(2, 2));
    let columns =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(inner);
    if let Some(image) = images.image(
        item,
        Size::new(
            columns[0].width.saturating_sub(2),
            columns[0].height.saturating_sub(2),
        ),
        settings.network_artwork,
    ) {
        let size = image.size();
        let image_area = Rect::new(
            columns[0].x + (columns[0].width - size.width) / 2,
            columns[0].y + (columns[0].height - size.height) / 2,
            size.width,
            size.height,
        );
        frame.render_widget(Image::new(image), image_area);
    }
    let mut lines = vec![
        Line::styled(
            item.title.to_uppercase(),
            Style::new().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Line::styled(item.subtitle.as_str(), Style::new().fg(theme.text)),
        Line::default(),
    ];
    lines.extend(item.details.iter().map(|detail| {
        Line::styled(
            truncate(detail, usize::from(columns[1].width)),
            Style::new().fg(theme.muted),
        )
    }));
    lines.push(Line::default());
    lines.push(Line::styled(
        if persisted.favorites.contains(&item.id) {
            "★ Favorite"
        } else {
            "☆ Add favorite"
        },
        Style::new().fg(accent),
    ));
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        columns[1],
    );
}

fn draw_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    theme: Theme,
    glyphs: &ButtonGlyphs,
) {
    let help = if state.detail_open {
        format!(
            "{} launch    {} favorite    {} back",
            glyphs.confirm, glyphs.favorite, glyphs.back
        )
    } else {
        format!(
            "← → modes    ↑ ↓ browse    {} launch    {} details    {} favorite    {} back",
            glyphs.confirm, glyphs.context, glyphs.favorite, glyphs.back
        )
    };
    frame.render_widget(
        Paragraph::new(help)
            .style(Style::new().fg(theme.muted))
            .alignment(Alignment::Center),
        area,
    );
}

struct WaveWidget {
    color: Color,
    phase: f32,
}

impl Widget for WaveWidget {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width < 4 || area.height < 4 {
            return;
        }
        for x in 0..area.width {
            let unit = f32::from(x) / f32::from(area.width);
            for lane in 0..2 {
                let wave = ((unit * PI * (2.0 + lane as f32 * 0.35))
                    + self.phase * (0.3 + lane as f32 * 0.12))
                    .sin();
                let base = f32::from(area.height) * (0.82 + lane as f32 * 0.07);
                let y = (base + wave * f32::from(area.height) * 0.025).round() as u16;
                if y < area.height {
                    let cell = buffer.cell_mut((area.x + x, area.y + y));
                    if let Some(cell) = cell {
                        cell.set_symbol(if lane == 1 { "·" } else { "╌" });
                        cell.set_fg(self.color);
                    }
                }
            }
        }
    }
}

fn centered_panel(area: Rect, configured_width: u16) -> Rect {
    let width = area.width.saturating_sub(2).min(configured_width).max(1);
    let height = area.height.saturating_sub(2).clamp(1, 34);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn border_type(index: usize) -> BorderType {
    match index {
        1 => BorderType::Rounded,
        2 => BorderType::Double,
        3 => BorderType::Thick,
        _ => BorderType::Plain,
    }
}

pub fn theme(index: usize) -> Theme {
    type ThemeData = (&'static str, [u8; 3], [u8; 3], [u8; 3], [u8; 3], [u8; 3]);
    const DATA: [ThemeData; 12] = [
        (
            "Tokyo Night",
            [18, 19, 26],
            [26, 27, 38],
            [169, 177, 214],
            [120, 124, 153],
            [86, 95, 137],
        ),
        (
            "Nord",
            [46, 52, 64],
            [59, 66, 82],
            [216, 222, 233],
            [129, 161, 193],
            [76, 86, 106],
        ),
        (
            "Catppuccin",
            [30, 30, 46],
            [49, 50, 68],
            [205, 214, 244],
            [147, 153, 178],
            [88, 91, 112],
        ),
        (
            "Gruvbox",
            [40, 40, 40],
            [60, 56, 54],
            [235, 219, 178],
            [168, 153, 132],
            [102, 92, 84],
        ),
        (
            "Everforest",
            [45, 53, 59],
            [52, 63, 68],
            [211, 198, 170],
            [133, 146, 137],
            [78, 94, 97],
        ),
        (
            "Mono",
            [10, 10, 10],
            [22, 22, 22],
            [235, 235, 235],
            [145, 145, 145],
            [72, 72, 72],
        ),
        (
            "Dracula",
            [40, 42, 54],
            [52, 55, 70],
            [248, 248, 242],
            [98, 114, 164],
            [68, 71, 90],
        ),
        (
            "Rose Pine",
            [25, 23, 36],
            [31, 29, 46],
            [224, 222, 244],
            [144, 140, 170],
            [64, 61, 82],
        ),
        (
            "Kanagawa",
            [31, 31, 40],
            [42, 42, 55],
            [220, 215, 186],
            [114, 113, 105],
            [84, 84, 109],
        ),
        (
            "Solarized",
            [0, 43, 54],
            [7, 54, 66],
            [238, 232, 213],
            [147, 161, 161],
            [88, 110, 117],
        ),
        (
            "One Half",
            [40, 44, 52],
            [49, 53, 63],
            [220, 223, 228],
            [92, 99, 112],
            [62, 68, 81],
        ),
        (
            "GitHub Dark",
            [13, 17, 23],
            [22, 27, 34],
            [230, 237, 243],
            [139, 148, 158],
            [48, 54, 61],
        ),
    ];
    let (name, background, panel, text, muted, border) = DATA[index.min(DATA.len() - 1)];
    Theme {
        name,
        background: rgb(background),
        panel: rgb(panel),
        text: rgb(text),
        muted: rgb(muted),
        border: rgb(border),
        accents: [
            Color::Rgb(122, 162, 247),
            Color::Rgb(158, 206, 106),
            Color::Rgb(224, 175, 104),
            Color::Rgb(187, 154, 247),
            Color::Rgb(125, 207, 255),
        ],
    }
}

fn rgb(value: [u8; 3]) -> Color {
    Color::Rgb(value[0], value[1], value[2])
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

fn load_svg(path: &Path) -> Option<DynamicImage> {
    let options = resvg::usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..Default::default()
    };
    let data = std::fs::read(path).ok()?;
    let tree = resvg::usvg::Tree::from_data(&data, &options).ok()?;
    let size = tree.size();
    let scale = (320.0 / size.width()).min(320.0 / size.height());
    let width = (size.width() * scale).round().max(1.0) as u32;
    let height = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(DynamicImage::ImageRgba8(
        ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, pixmap.take())?,
    ))
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
    canvas.resize_exact(320, 320, FilterType::Lanczos3)
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
    let mut image = ImageBuffer::from_pixel(128, 128, Rgba([0x12, 0x13, 0x1a, 255]));
    for y in 14..114 {
        for x in 14..114 {
            if (24..104).contains(&x) && (24..104).contains(&y) || (x + y) % 13 < 7 {
                image.put_pixel(x, y, color);
            }
        }
    }
    DynamicImage::ImageRgba8(image)
}

fn video_thumbnail(path: &Path) -> Option<PathBuf> {
    if !command_exists("ffmpegthumbnailer") {
        return None;
    }
    let cache = paths()?.cache.join("thumbs");
    std::fs::create_dir_all(&cache).ok()?;
    let fingerprint = path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let hash = path
        .to_string_lossy()
        .bytes()
        .fold(fingerprint, |hash, byte| {
            hash.rotate_left(5) ^ u64::from(byte)
        });
    let output = cache.join(format!("{hash:016x}.jpg"));
    if output.is_file() {
        return Some(output);
    }
    let status = Command::new("ffmpegthumbnailer")
        .arg("-i")
        .arg(path)
        .arg("-o")
        .arg(&output)
        .args(["-s", "320", "-q", "8"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(output)
}

fn fetch_steam_art(app_id: u32) -> Option<PathBuf> {
    let cache = paths()?.cache.join("art");
    std::fs::create_dir_all(&cache).ok()?;
    let output = cache.join(format!("steam-{app_id}.jpg"));
    if output.is_file() {
        return Some(output);
    }
    let url =
        format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/library_600x900.jpg");
    let mut response = ureq::get(&url).call().ok()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_vec()
        .ok()?;
    image::load_from_memory(&bytes).ok()?;
    std::fs::write(&output, bytes).ok()?;
    trim_art_cache(&cache, 256 * 1024 * 1024);
    Some(output)
}

fn trim_art_cache(directory: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then_some((
                entry.path(),
                metadata.len(),
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            ))
        })
        .collect::<Vec<_>>();
    let mut total = files.iter().map(|(_, size, _)| size).sum::<u64>();
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn is_video(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v")
    )
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

pub fn destructive(item: &LibraryItem) -> bool {
    matches!(&item.action, Action::System(action) if action.destructive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn all_themes_are_stable() {
        assert_eq!(theme(0).name, "Tokyo Night");
        assert_eq!(theme(5).name, "Mono");
        assert_eq!(theme(11).name, "GitHub Dark");
    }

    #[test]
    fn truncation_respects_display_width() {
        assert_eq!(truncate("Chromium", 6), "Chrom…");
        assert!(UnicodeWidthStr::width(truncate("非常に長い名前", 5).as_str()) <= 5);
    }

    #[test]
    fn power_actions_require_hold() {
        let item = LibraryItem::simple(
            "off",
            "Off",
            Action::System(crate::model::SystemAction::Shutdown),
        );
        assert!(destructive(&item));
    }

    #[test]
    fn xmb_shell_renders_at_supported_sizes() {
        for (width, height) in [(60, 18), (100, 30), (180, 40)] {
            let mut modes = HashMap::new();
            let mut item = LibraryItem::simple(
                "application:demo",
                "Demo App",
                Action::Setting(crate::model::SettingAction::Theme),
            );
            item.subtitle = "A fixture application".into();
            modes.insert(Mode::Applications, vec![item]);
            let mut state = AppState::new(Mode::Applications, modes);
            let settings = Settings {
                reduced_motion: true,
                waves: false,
                transparent: false,
                ..Settings::default()
            };
            let persisted = PersistentState::default();
            let (tx, rx) = mpsc::channel();
            let mut images = ImageCache {
                picker: Picker::halfblocks(),
                protocols: HashMap::new(),
                generated: HashMap::new(),
                pending: HashSet::new(),
                generated_tx: tx,
                generated_rx: rx,
            };
            let glyphs = ButtonGlyphs {
                confirm: "↵".into(),
                back: "Esc".into(),
                context: "C".into(),
                favorite: "F".into(),
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw(
                        frame,
                        &mut state,
                        &settings,
                        &persisted,
                        Presentation::Fullscreen,
                        &mut images,
                        glyphs.clone(),
                    )
                })
                .unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                rendered.contains("DEMO APP"),
                "missing title at {width}x{height}"
            );
        }
    }
}
