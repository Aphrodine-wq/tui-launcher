use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant, SystemTime},
};

use chrono::{Datelike, Local};
use eframe::egui::{
    self, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, Mesh, Pos2, Rect,
    Sense, Shape, Stroke, TextureHandle, TextureOptions, Vec2, ViewportCommand,
};
use image::{DynamicImage, ImageBuffer, Rgba};

use crate::{
    config::{PersistentState, Settings, paths},
    feedback::{AudioFeedback, Tone},
    input::{ControllerInput, InputAction},
    model::{Action, AppState, LibraryItem, Mode, SettingAction},
    platform, sources,
};

const FRAME_TIME: Duration = Duration::from_millis(16);

pub struct XmbApp {
    settings: Settings,
    migrated: bool,
    persisted: PersistentState,
    state: AppState,
    controller: ControllerInput,
    audio: Option<AudioFeedback>,
    category_position: f32,
    item_positions: HashMap<Mode, f32>,
    wave_time: f32,
    textures: HashMap<PathBuf, TextureHandle>,
    generated_art: HashMap<String, Option<PathBuf>>,
    art_pending: HashSet<String>,
    art_tx: Sender<(String, Option<PathBuf>)>,
    art_rx: Receiver<(String, Option<PathBuf>)>,
    options_open: bool,
    options_selection: usize,
    information_open: bool,
    pending_destructive: Option<Action>,
    toast: Option<(String, Instant)>,
    last_status_refresh: Instant,
    last_media_refresh: Instant,
    message_tx: Sender<String>,
    message_rx: Receiver<String>,
}

impl XmbApp {
    pub fn new(
        creation: &eframe::CreationContext<'_>,
        settings: Settings,
        migrated: bool,
        persisted: PersistentState,
        start: Mode,
    ) -> Self {
        install_fonts(&creation.egui_ctx);
        creation.egui_ctx.set_visuals(egui::Visuals::dark());
        let items = sources::discover_all(&settings, &persisted);
        let mut state = AppState::new(start, items);
        restore_selections(&mut state, &persisted);
        state.system_status = sources::system_status();
        refresh_now_playing(&mut state);
        let controller = ControllerInput::new(settings.controller.clone());
        state.controller_name = controller.name.clone();
        let item_positions = Mode::ALL
            .into_iter()
            .map(|mode| {
                (
                    mode,
                    state.selections.get(&mode).copied().unwrap_or(0) as f32,
                )
            })
            .collect();
        let (message_tx, message_rx) = mpsc::channel();
        let (art_tx, art_rx) = mpsc::channel();
        Self {
            settings,
            migrated,
            persisted,
            category_position: start.index() as f32,
            item_positions,
            state,
            controller,
            audio: AudioFeedback::new(),
            wave_time: 0.0,
            textures: HashMap::new(),
            generated_art: HashMap::new(),
            art_pending: HashSet::new(),
            art_tx,
            art_rx,
            options_open: false,
            options_selection: 0,
            information_open: false,
            pending_destructive: None,
            toast: None,
            last_status_refresh: Instant::now(),
            last_media_refresh: Instant::now(),
            message_tx,
            message_rx,
        }
    }

    fn update_runtime(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|input| input.stable_dt).clamp(0.0, 0.05);
        self.wave_time += dt;
        let category_target = self.state.mode.index() as f32;
        if self.settings.reduced_motion {
            self.category_position = category_target;
        } else {
            self.category_position = ease(self.category_position, category_target, dt, 13.0);
        }
        for mode in Mode::ALL {
            let target = self.state.selections.get(&mode).copied().unwrap_or(0) as f32;
            let current = self.item_positions.entry(mode).or_insert(target);
            *current = if self.settings.reduced_motion {
                target
            } else {
                ease(*current, target, dt, 15.0)
            };
        }
        self.state.controller_name = self.controller.name.clone();

        if self.last_status_refresh.elapsed() >= Duration::from_secs(5) {
            self.state.system_status = sources::system_status();
            self.last_status_refresh = Instant::now();
        }
        if self.last_media_refresh.elapsed() >= Duration::from_secs(2) {
            refresh_now_playing(&mut self.state);
            self.last_media_refresh = Instant::now();
        }
        while let Ok(message) = self.message_rx.try_recv() {
            self.toast(message);
        }
        while let Ok((id, path)) = self.art_rx.try_recv() {
            self.art_pending.remove(&id);
            self.generated_art.insert(id, path);
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, shown)| shown.elapsed() >= Duration::from_secs(3))
        {
            self.toast = None;
        }
        ctx.request_repaint_after(FRAME_TIME);
    }

    fn collect_input(&mut self, ctx: &egui::Context) -> Vec<InputAction> {
        let mut actions = self
            .controller
            .poll()
            .into_iter()
            .filter(|event| event.pressed)
            .map(|event| event.action)
            .collect::<Vec<_>>();
        ctx.input(|input| {
            let keys = [
                (Key::ArrowLeft, InputAction::PreviousMode),
                (Key::ArrowRight, InputAction::NextMode),
                (Key::ArrowUp, InputAction::PreviousItem),
                (Key::ArrowDown, InputAction::NextItem),
                (Key::Enter, InputAction::Confirm),
                (Key::Space, InputAction::Confirm),
                (Key::Escape, InputAction::Back),
                (Key::T, InputAction::Context),
                (Key::F, InputAction::Favorite),
                (Key::S, InputAction::Settings),
            ];
            for (key, action) in keys {
                if input.key_pressed(key) {
                    actions.push(action);
                }
            }
        });
        actions
    }

    fn handle_action(&mut self, action: InputAction, ctx: &egui::Context) {
        if self.information_open {
            if matches!(
                action,
                InputAction::Back | InputAction::Confirm | InputAction::Context
            ) {
                self.information_open = false;
                self.feedback(Tone::Back);
            }
            return;
        }
        if self.pending_destructive.is_some() {
            match action {
                InputAction::Confirm => {
                    if let Some(action) = self.pending_destructive.take() {
                        self.execute_in_place(&action);
                    }
                }
                InputAction::Back => self.pending_destructive = None,
                _ => {}
            }
            return;
        }
        if self.options_open {
            self.handle_options_action(action);
            return;
        }
        match action {
            InputAction::PreviousMode => self.navigate_mode(-1),
            InputAction::NextMode => self.navigate_mode(1),
            InputAction::PreviousItem => self.navigate_item(-1),
            InputAction::NextItem => self.navigate_item(1),
            InputAction::Settings => {
                self.state.mode = Mode::Settings;
                self.options_open = false;
                self.feedback(Tone::Navigate);
            }
            InputAction::Context => {
                if self.state.selected_item().is_some() {
                    self.options_open = true;
                    self.options_selection = 0;
                    self.feedback(Tone::Confirm);
                }
            }
            InputAction::Favorite => self.toggle_favorite(),
            InputAction::Back => {
                if self.options_open {
                    self.options_open = false;
                    self.feedback(Tone::Back);
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            InputAction::Confirm => self.confirm(ctx),
        }
    }

    fn handle_options_action(&mut self, action: InputAction) {
        match action {
            InputAction::PreviousItem => {
                self.options_selection = self.options_selection.saturating_sub(1);
                self.feedback(Tone::Navigate);
            }
            InputAction::NextItem => {
                self.options_selection = (self.options_selection + 1).min(2);
                self.feedback(Tone::Navigate);
            }
            InputAction::Confirm => match self.options_selection {
                0 => {
                    self.options_open = false;
                    self.toggle_favorite();
                }
                1 => {
                    self.options_open = false;
                    self.information_open = true;
                    self.feedback(Tone::Confirm);
                }
                _ => {
                    self.options_open = false;
                    self.feedback(Tone::Back);
                }
            },
            InputAction::Back | InputAction::Context => {
                self.options_open = false;
                self.feedback(Tone::Back);
            }
            _ => {}
        }
    }

    fn navigate_mode(&mut self, delta: isize) {
        self.state.switch_mode(delta);
        self.options_open = false;
        self.pending_destructive = None;
        self.feedback(Tone::Navigate);
    }

    fn navigate_item(&mut self, delta: isize) {
        self.state.select_delta(delta);
        self.options_open = false;
        self.pending_destructive = None;
        self.feedback(Tone::Navigate);
    }

    fn confirm(&mut self, ctx: &egui::Context) {
        let Some(item) = self.state.selected_item().cloned() else {
            return;
        };
        if !item.available {
            self.toast(
                item.unavailable_reason
                    .unwrap_or_else(|| "This item is unavailable".to_owned()),
            );
            self.feedback(Tone::Warning);
            return;
        }
        if let Action::Setting(action) = item.action {
            adjust_setting(action, &mut self.settings);
            self.controller
                .set_bindings(self.settings.controller.clone());
            self.refresh_settings_items();
            self.toast("Setting updated");
            self.feedback(Tone::Confirm);
            return;
        }
        if item
            .action
            .as_system()
            .is_some_and(|action| action.destructive())
        {
            self.pending_destructive = Some(item.action);
            self.feedback(Tone::Warning);
            return;
        }
        self.persisted.record_recent(self.state.mode, &item.id);
        if item.action.launches_external_window() {
            self.launch_external(item.action, ctx);
        } else {
            self.execute_in_place(&item.action);
        }
    }

    fn execute_in_place(&mut self, action: &Action) {
        match platform::execute(action) {
            Ok(message) => {
                if !message.is_empty() {
                    self.toast(message);
                }
                self.feedback(Tone::Confirm);
            }
            Err(error) => {
                self.toast(error.to_string());
                self.feedback(Tone::Warning);
            }
        }
    }

    fn launch_external(&mut self, action: Action, ctx: &egui::Context) {
        self.feedback(Tone::Confirm);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        let context = ctx.clone();
        let sender = self.message_tx.clone();
        thread::spawn(move || {
            let baseline = hypr_client_addresses();
            let result = platform::execute(&action);
            match result {
                Ok(message) => {
                    wait_for_launched_window(&baseline);
                    let _ = sender.send(if message.is_empty() {
                        "Welcome back".to_owned()
                    } else {
                        message
                    });
                }
                Err(error) => {
                    let _ = sender.send(error.to_string());
                }
            }
            context.send_viewport_cmd(ViewportCommand::Visible(true));
            context.send_viewport_cmd(ViewportCommand::Focus);
            context.request_repaint();
        });
    }

    fn refresh_settings_items(&mut self) {
        let mut items = sources::setting_items(&self.settings);
        items.extend(sources::system_items());
        self.state.items.insert(Mode::Settings, items);
        let len = self
            .state
            .items
            .get(&Mode::Settings)
            .map(Vec::len)
            .unwrap_or(0);
        let selection = self.state.selections.entry(Mode::Settings).or_default();
        *selection = (*selection).min(len.saturating_sub(1));
    }

    fn toggle_favorite(&mut self) {
        if matches!(self.state.mode, Mode::Settings | Mode::Network) {
            return;
        }
        let Some(item) = self.state.selected_item().cloned() else {
            return;
        };
        let added = self.persisted.toggle_favorite(&item.id);
        if let Some(items) = self.state.items.get_mut(&self.state.mode) {
            sources::order_library(items, &self.persisted, self.state.mode);
            if let Some(index) = items.iter().position(|candidate| candidate.id == item.id) {
                self.state.selections.insert(self.state.mode, index);
            }
        }
        self.toast(if added {
            "Added to favorites"
        } else {
            "Removed from favorites"
        });
        self.feedback(Tone::Confirm);
    }

    fn feedback(&mut self, tone: Tone) {
        if self.settings.sound
            && let Some(audio) = &self.audio
        {
            audio.play(tone, self.settings.sound_volume);
        }
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect().shrink(1.0);
        let painter = ui.painter_at(rect);
        paint_background(&painter, rect, self.wave_time, &self.settings);
        self.draw_status(&painter, rect);
        self.draw_categories(ui, &painter, rect);
        self.draw_items(ui, &painter, rect);
        self.draw_footer(&painter, rect);
        if self.options_open {
            self.draw_options(&painter, rect);
        }
        if self.information_open {
            self.draw_information(&painter, rect);
        }
        if let Some(action) = &self.pending_destructive {
            self.draw_confirmation(&painter, rect, action);
        }
        if let Some((message, _)) = &self.toast {
            draw_toast(&painter, rect, message);
        }
    }

    fn draw_status(&self, painter: &egui::Painter, rect: Rect) {
        let top = rect.top() + rect.height() * 0.055;
        let input = self
            .state
            .controller_name
            .as_deref()
            .map(|name| format!("{}  × Enter", truncate(name, 24)))
            .unwrap_or_else(|| "KEYBOARD  ·  × Enter".to_owned());
        shadow_text(
            painter,
            Pos2::new(rect.left() + 42.0, top),
            Align2::LEFT_CENTER,
            &input,
            FontId::proportional(14.0),
            Color32::from_white_alpha(190),
        );
        let mut pieces = Vec::new();
        if let Some(network) = &self.state.system_status.network {
            pieces.push(network.clone());
        }
        if let Some(volume) = &self.state.system_status.volume {
            pieces.push(volume.clone());
        }
        if let Some(battery) = &self.state.system_status.battery {
            pieces.push(battery.clone());
        }
        pieces.push(Local::now().format("%-I:%M %p").to_string());
        shadow_text(
            painter,
            Pos2::new(rect.right() - 42.0, top),
            Align2::RIGHT_CENTER,
            &pieces.join("   "),
            FontId::proportional(14.0),
            Color32::WHITE,
        );
    }

    fn draw_categories(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, rect: Rect) {
        let center = Pos2::new(rect.center().x, rect.top() + rect.height() * 0.30);
        let spacing = (rect.width() * 0.105).clamp(82.0, 132.0);
        for (index, mode) in Mode::ALL.into_iter().enumerate() {
            let offset = index as f32 - self.category_position;
            let x = center.x + offset * spacing;
            if x < rect.left() - 70.0 || x > rect.right() + 70.0 {
                continue;
            }
            let distance = offset.abs().min(1.0);
            let size = 46.0 - distance * 14.0;
            let alpha = (255.0 - distance * 95.0) as u8;
            let hit = Rect::from_center_size(Pos2::new(x, center.y), Vec2::splat(72.0));
            if ui
                .interact(hit, ui.id().with(("category", index)), Sense::click())
                .clicked()
            {
                self.state.mode = mode;
                self.options_open = false;
            }
            shadow_text(
                painter,
                Pos2::new(x, center.y),
                Align2::CENTER_CENTER,
                mode.glyph(),
                FontId::proportional(size),
                Color32::from_white_alpha(alpha),
            );
            if mode == self.state.mode {
                shadow_text(
                    painter,
                    Pos2::new(x, center.y + 43.0),
                    Align2::CENTER_TOP,
                    mode.title(),
                    FontId::proportional(15.0),
                    Color32::WHITE,
                );
            }
        }
    }

    fn draw_items(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, rect: Rect) {
        let mode = self.state.mode;
        let selected = self.state.selected_index();
        let visual = self
            .item_positions
            .get(&mode)
            .copied()
            .unwrap_or(selected as f32);
        let items = self.state.mode_items().to_vec();
        if items.is_empty() {
            shadow_text(
                painter,
                Pos2::new(rect.center().x, rect.center().y + 60.0),
                Align2::CENTER_CENTER,
                "No content found",
                FontId::proportional(22.0),
                Color32::from_white_alpha(180),
            );
            return;
        }
        let anchor = Pos2::new(
            rect.center().x - rect.width() * 0.08,
            rect.top() + rect.height() * 0.53,
        );
        let row_height = (rect.height() * 0.095).clamp(48.0, 70.0);

        let selected_art = items.get(selected).and_then(|item| self.art_for(item));
        if let Some(path) = selected_art
            && let Some(texture) = self.texture_for(ui.ctx(), &path)
        {
            let art_size = Vec2::new(rect.width() * 0.34, rect.height() * 0.42);
            let art_rect = Rect::from_center_size(
                Pos2::new(rect.right() - art_size.x * 0.55, rect.center().y + 70.0),
                art_size,
            );
            painter.image(
                texture.id(),
                art_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::from_white_alpha(105),
            );
        }

        for (index, item) in items.iter().enumerate() {
            let delta = index as f32 - visual;
            if delta.abs() > 4.3 {
                continue;
            }
            let y = anchor.y + delta * row_height;
            let active = index == selected;
            let opacity = ((1.0 - (delta.abs() / 5.0)) * 255.0).clamp(55.0, 255.0) as u8;
            let icon_size = if active { 39.0 } else { 28.0 };
            let item_rect = Rect::from_min_size(
                Pos2::new(anchor.x - 60.0, y - row_height * 0.45),
                Vec2::new(rect.width() * 0.52, row_height * 0.9),
            );
            if ui
                .interact(
                    item_rect,
                    ui.id().with(("item", mode.index(), index)),
                    Sense::click(),
                )
                .clicked()
            {
                self.state.selections.insert(mode, index);
            }
            let icon_path = item
                .art
                .clone()
                .or_else(|| self.generated_art.get(&item.id).and_then(Clone::clone));
            let texture_id = icon_path
                .as_deref()
                .and_then(|path| self.texture_for(ui.ctx(), path))
                .map(TextureHandle::id);
            if let Some(texture_id) = texture_id {
                let side = if active { 47.0 } else { 31.0 };
                painter.image(
                    texture_id,
                    Rect::from_center_size(Pos2::new(anchor.x, y), Vec2::splat(side)),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::from_white_alpha(opacity),
                );
            } else {
                let icon = if active { mode.glyph() } else { "•" };
                shadow_text(
                    painter,
                    Pos2::new(anchor.x, y),
                    Align2::CENTER_CENTER,
                    icon,
                    FontId::proportional(icon_size),
                    if item.available {
                        Color32::from_white_alpha(opacity)
                    } else {
                        Color32::from_gray(105)
                    },
                );
            }
            if active {
                shadow_text(
                    painter,
                    Pos2::new(anchor.x + 48.0, y - 6.0),
                    Align2::LEFT_CENTER,
                    &truncate(&item.title, 42),
                    FontId::proportional(22.0),
                    if item.available {
                        Color32::WHITE
                    } else {
                        Color32::GRAY
                    },
                );
                if !item.subtitle.is_empty() {
                    shadow_text(
                        painter,
                        Pos2::new(anchor.x + 49.0, y + 19.0),
                        Align2::LEFT_CENTER,
                        &truncate(&item.subtitle, 56),
                        FontId::proportional(13.0),
                        Color32::from_white_alpha(165),
                    );
                }
            }
        }
    }

    fn texture_for(&mut self, ctx: &egui::Context, path: &Path) -> Option<&TextureHandle> {
        if !self.textures.contains_key(path) {
            let image = load_visual(path)?;
            let image = image.thumbnail(1200, 800).to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            let pixels = image.into_raw();
            let color = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
            let texture =
                ctx.load_texture(path.display().to_string(), color, TextureOptions::LINEAR);
            self.textures.insert(path.to_path_buf(), texture);
        }
        self.textures.get(path)
    }

    fn art_for(&mut self, item: &LibraryItem) -> Option<PathBuf> {
        if let Some(path) = item.hero.as_ref().or(item.art.as_ref()) {
            return Some(path.clone());
        }
        if let Some(path) = self.generated_art.get(&item.id) {
            return path.clone();
        }
        let Action::Open(path) = &item.action else {
            return None;
        };
        if !is_video(path) || !self.art_pending.insert(item.id.clone()) {
            return None;
        }
        let id = item.id.clone();
        let path = path.clone();
        let sender = self.art_tx.clone();
        thread::spawn(move || {
            let generated = video_thumbnail(&path);
            let _ = sender.send((id, generated));
        });
        None
    }

    fn draw_footer(&self, painter: &egui::Painter, rect: Rect) {
        let text = if self.options_open {
            "× Select     ○ Back"
        } else {
            "× Enter     ○ Back     △ Options"
        };
        shadow_text(
            painter,
            Pos2::new(rect.right() - 38.0, rect.bottom() - 28.0),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(14.0),
            Color32::from_white_alpha(205),
        );
    }

    fn draw_options(&self, painter: &egui::Painter, rect: Rect) {
        let panel = Rect::from_min_size(
            Pos2::new(rect.right() - 280.0, rect.top() + rect.height() * 0.34),
            Vec2::new(240.0, 190.0),
        );
        painter.rect_filled(panel, 4.0, Color32::from_black_alpha(205));
        painter.rect_stroke(
            panel,
            4.0,
            Stroke::new(1.0, Color32::from_white_alpha(100)),
            egui::StrokeKind::Inside,
        );
        let item = self.state.selected_item();
        let favorite = item.is_some_and(|item| self.persisted.favorites.contains(&item.id));
        let lines = [
            "OPTIONS",
            if favorite {
                "Remove Favorite"
            } else {
                "Add Favorite"
            },
            "Information",
            "Close",
        ];
        for (index, line) in lines.into_iter().enumerate() {
            shadow_text(
                painter,
                Pos2::new(
                    panel.left() + 24.0,
                    panel.top() + 28.0 + index as f32 * 42.0,
                ),
                Align2::LEFT_CENTER,
                line,
                FontId::proportional(if index == 0 { 13.0 } else { 17.0 }),
                if index > 0 && index - 1 == self.options_selection {
                    Color32::WHITE
                } else {
                    Color32::from_white_alpha(170)
                },
            );
        }
    }

    fn draw_information(&self, painter: &egui::Painter, rect: Rect) {
        let Some(item) = self.state.selected_item() else {
            return;
        };
        let panel = Rect::from_center_size(rect.center(), Vec2::new(640.0, 340.0));
        painter.rect_filled(panel, 8.0, Color32::from_black_alpha(225));
        painter.rect_stroke(
            panel,
            8.0,
            Stroke::new(1.0, Color32::from_white_alpha(110)),
            egui::StrokeKind::Inside,
        );
        shadow_text(
            painter,
            Pos2::new(panel.left() + 38.0, panel.top() + 48.0),
            Align2::LEFT_CENTER,
            &item.title,
            FontId::proportional(26.0),
            Color32::WHITE,
        );
        shadow_text(
            painter,
            Pos2::new(panel.left() + 39.0, panel.top() + 80.0),
            Align2::LEFT_CENTER,
            &item.subtitle,
            FontId::proportional(15.0),
            Color32::from_white_alpha(175),
        );
        for (index, detail) in item.details.iter().take(5).enumerate() {
            shadow_text(
                painter,
                Pos2::new(
                    panel.left() + 39.0,
                    panel.top() + 135.0 + index as f32 * 31.0,
                ),
                Align2::LEFT_CENTER,
                &truncate(detail, 70),
                FontId::proportional(14.0),
                Color32::from_white_alpha(185),
            );
        }
        shadow_text(
            painter,
            Pos2::new(panel.right() - 30.0, panel.bottom() - 27.0),
            Align2::RIGHT_CENTER,
            "○ Back",
            FontId::proportional(14.0),
            Color32::from_white_alpha(190),
        );
    }

    fn draw_confirmation(&self, painter: &egui::Painter, rect: Rect, _action: &Action) {
        let panel = Rect::from_center_size(rect.center(), Vec2::new(430.0, 170.0));
        painter.rect_filled(panel, 8.0, Color32::from_black_alpha(225));
        painter.rect_stroke(
            panel,
            8.0,
            Stroke::new(1.0, Color32::from_white_alpha(120)),
            egui::StrokeKind::Inside,
        );
        shadow_text(
            painter,
            Pos2::new(panel.center().x, panel.top() + 48.0),
            Align2::CENTER_CENTER,
            "Confirm system action?",
            FontId::proportional(23.0),
            Color32::WHITE,
        );
        shadow_text(
            painter,
            Pos2::new(panel.center().x, panel.bottom() - 48.0),
            Align2::CENTER_CENTER,
            "× Confirm       ○ Cancel",
            FontId::proportional(16.0),
            Color32::from_white_alpha(190),
        );
    }

    fn persist(&mut self) {
        self.persisted.last_mode = self.state.mode;
        for mode in Mode::ALL {
            if let Some(item) =
                self.state.items.get(&mode).and_then(|items| {
                    items.get(self.state.selections.get(&mode).copied().unwrap_or(0))
                })
            {
                self.persisted
                    .selected
                    .insert(mode.title().to_ascii_lowercase(), item.id.clone());
            }
        }
        let _ = self.settings.save(self.migrated);
        self.migrated = false;
        let _ = self.persisted.save();
    }
}

impl eframe::App for XmbApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_runtime(ui.ctx());
        for action in self.collect_input(ui.ctx()) {
            self.handle_action(action, ui.ctx());
        }
        self.draw(ui);
    }

    fn on_exit(&mut self) {
        self.persist();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
}

impl Action {
    fn launches_external_window(&self) -> bool {
        matches!(
            self,
            Self::Desktop(_) | Self::Steam(_) | Self::Open(_) | Self::Network(_)
        )
    }

    fn as_system(&self) -> Option<&crate::model::SystemAction> {
        match self {
            Self::System(action) => Some(action),
            _ => None,
        }
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, path) in [
        ("noto", "/usr/share/fonts/noto/NotoSans-Regular.ttf"),
        (
            "symbols",
            "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        ),
    ] {
        if let Ok(bytes) = fs::read(path) {
            fonts
                .font_data
                .insert(name.to_owned(), FontData::from_owned(bytes).into());
        }
    }
    if fonts.font_data.contains_key("noto") {
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "noto".to_owned());
    }
    if fonts.font_data.contains_key("symbols") {
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .push("symbols".to_owned());
    }
    ctx.set_fonts(fonts);
}

fn load_visual(path: &Path) -> Option<DynamicImage> {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        let options = resvg::usvg::Options {
            resources_dir: path.parent().map(Path::to_path_buf),
            ..Default::default()
        };
        let data = fs::read(path).ok()?;
        let tree = resvg::usvg::Tree::from_data(&data, &options).ok()?;
        let size = tree.size();
        let scale = (512.0 / size.width()).min(512.0 / size.height());
        let width = (size.width() * scale).round().max(1.0) as u32;
        let height = (size.height() * scale).round().max(1.0) as u32;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        return Some(DynamicImage::ImageRgba8(
            ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, pixmap.take())?,
        ));
    }
    image::open(path).ok()
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v"
            )
        })
}

fn video_thumbnail(path: &Path) -> Option<PathBuf> {
    if !sources::command_exists("ffmpegthumbnailer") {
        return None;
    }
    let cache = paths()?.cache.join("thumbs");
    fs::create_dir_all(&cache).ok()?;
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
        .args(["-s", "640", "-q", "9"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(output)
}

fn paint_background(painter: &egui::Painter, rect: Rect, time: f32, settings: &Settings) {
    let [top, bottom] = monthly_palette(Local::now().month0() as usize, settings.theme);
    let mut mesh = Mesh::default();
    let inset = rect.shrink(1.0);
    mesh.colored_vertex(inset.left_top(), top);
    mesh.colored_vertex(inset.right_top(), top);
    mesh.colored_vertex(inset.right_bottom(), bottom);
    mesh.colored_vertex(inset.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
    painter.rect_stroke(
        inset,
        26.0,
        Stroke::new(1.0, Color32::from_white_alpha(45)),
        egui::StrokeKind::Inside,
    );
    if settings.waves {
        for lane in 0..3 {
            let mut points = Vec::with_capacity(121);
            for step in 0..=120 {
                let unit = step as f32 / 120.0;
                let x = rect.left() + unit * rect.width();
                let base = rect.top() + rect.height() * (0.68 + lane as f32 * 0.065);
                let y = base
                    + (unit * std::f32::consts::TAU * (1.25 + lane as f32 * 0.18)
                        + time * (0.32 + lane as f32 * 0.08))
                        .sin()
                        * rect.height()
                        * (0.035 + lane as f32 * 0.008);
                points.push(Pos2::new(x, y));
            }
            painter.add(Shape::line(
                points,
                Stroke::new(1.1, Color32::from_white_alpha(42 - lane as u8 * 7)),
            ));
        }
    }
}

fn monthly_palette(month: usize, theme: usize) -> [Color32; 2] {
    const COLORS: [[(u8, u8, u8); 2]; 12] = [
        [(55, 102, 154), (17, 42, 78)],
        [(99, 74, 148), (35, 24, 75)],
        [(166, 90, 139), (76, 28, 74)],
        [(128, 145, 72), (47, 70, 31)],
        [(51, 151, 125), (13, 71, 66)],
        [(48, 139, 181), (16, 62, 94)],
        [(38, 123, 194), (14, 48, 100)],
        [(62, 102, 177), (24, 38, 92)],
        [(133, 91, 159), (54, 31, 87)],
        [(164, 91, 73), (76, 35, 29)],
        [(136, 104, 68), (65, 45, 27)],
        [(69, 121, 142), (20, 55, 73)],
    ];
    let colors = COLORS[(month + theme) % COLORS.len()];
    [
        Color32::from_rgba_premultiplied(colors[0].0, colors[0].1, colors[0].2, 238),
        Color32::from_rgba_premultiplied(colors[1].0, colors[1].1, colors[1].2, 246),
    ]
}

fn shadow_text(
    painter: &egui::Painter,
    position: Pos2,
    align: Align2,
    text: &str,
    font: FontId,
    color: Color32,
) {
    painter.text(
        position + Vec2::new(1.2, 1.5),
        align,
        text,
        font.clone(),
        Color32::from_black_alpha(color.a().saturating_sub(40)),
    );
    painter.text(position, align, text, font, color);
}

fn draw_toast(painter: &egui::Painter, rect: Rect, message: &str) {
    let width = (message.chars().count() as f32 * 8.5 + 56.0).clamp(220.0, 560.0);
    let panel = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.bottom() - 72.0),
        Vec2::new(width, 44.0),
    );
    painter.rect_filled(panel, 22.0, Color32::from_black_alpha(185));
    shadow_text(
        painter,
        panel.center(),
        Align2::CENTER_CENTER,
        message,
        FontId::proportional(14.0),
        Color32::WHITE,
    );
}

fn adjust_setting(action: SettingAction, settings: &mut Settings) {
    match action {
        SettingAction::Theme => settings.theme = (settings.theme + 1) % 12,
        SettingAction::Accent => settings.accent = (settings.accent + 1) % 5,
        SettingAction::Transparent => settings.transparent = !settings.transparent,
        SettingAction::Waves => settings.waves = !settings.waves,
        SettingAction::Sound => settings.sound = !settings.sound,
        SettingAction::ReducedMotion => settings.reduced_motion = !settings.reduced_motion,
        SettingAction::NetworkArtwork => settings.network_artwork = !settings.network_artwork,
        SettingAction::PanelWidth => {
            settings.panel_width = if settings.panel_width >= 180 {
                90
            } else {
                settings.panel_width + 15
            };
        }
        SettingAction::ResetAppearance => {
            let defaults = Settings::default();
            settings.theme = defaults.theme;
            settings.accent = defaults.accent;
            settings.transparent = defaults.transparent;
            settings.waves = defaults.waves;
            settings.reduced_motion = defaults.reduced_motion;
        }
        SettingAction::Binding(target) => settings.controller.cycle(target),
    }
}

fn refresh_now_playing(state: &mut AppState) {
    let now = sources::now_playing();
    state.now_playing = now.clone();
    let Some(items) = state.items.get_mut(&Mode::Music) else {
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

fn hypr_client_addresses() -> HashSet<String> {
    let Ok(output) = Command::new("hyprctl").args(["-j", "clients"]).output() else {
        return HashSet::new();
    };
    if !output.status.success() {
        return HashSet::new();
    }
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|client| client["address"].as_str().map(str::to_owned))
        .collect()
}

fn wait_for_launched_window(baseline: &HashSet<String>) {
    if baseline.is_empty() {
        thread::sleep(Duration::from_secs(2));
        return;
    }
    let find_deadline = Instant::now() + Duration::from_secs(45);
    let launched = loop {
        let current = hypr_client_addresses();
        if let Some(address) = current.difference(baseline).next() {
            break Some(address.clone());
        }
        if Instant::now() >= find_deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(250));
    };
    let Some(address) = launched else {
        return;
    };
    while hypr_client_addresses().contains(&address) {
        thread::sleep(Duration::from_millis(400));
    }
}

fn ease(current: f32, target: f32, dt: f32, speed: f32) -> f32 {
    current + (target - current) * (1.0 - (-speed * dt).exp())
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut output = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monthly_palette_has_distinct_ends() {
        for month in 0..12 {
            let [top, bottom] = monthly_palette(month, 0);
            assert_ne!(top, bottom);
        }
    }

    #[test]
    fn external_launches_are_classified() {
        assert!(Action::Steam(10).launches_external_window());
        assert!(!Action::Setting(SettingAction::Theme).launches_external_window());
    }

    #[test]
    fn truncation_is_bounded() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
