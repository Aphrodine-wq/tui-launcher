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
    model::{Action, AppState, LibraryItem, Mode, SettingAction, SettingsGroup},
    platform, sources,
};

const FRAME_TIME: Duration = Duration::from_millis(16);

/// Texture resolution tier. Icons get a small CPU-downscaled texture so they
/// stay crisp at list size (the wgpu backend has no mipmaps); heroes and
/// backgrounds get the large version.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum TextureTier {
    Thumb,
    Full,
}

struct SubMenu {
    group: SettingsGroup,
    items: Vec<LibraryItem>,
    selection: usize,
    position: f32,
}

enum OptionEntry {
    Favorite,
    Information,
    SetBackground(PathBuf),
    Close,
}

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
    textures: HashMap<(PathBuf, TextureTier), TextureHandle>,
    submenu: Option<SubMenu>,
    background_override: Option<PathBuf>,
    generated_art: HashMap<String, Option<PathBuf>>,
    art_pending: HashSet<String>,
    art_tx: Sender<(String, Option<PathBuf>)>,
    art_rx: Receiver<(String, Option<PathBuf>)>,
    options_open: bool,
    options_selection: usize,
    information_open: bool,
    pending_destructive: Option<Action>,
    boot_at: Instant,
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
        background_override: Option<PathBuf>,
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
        if settings.network_artwork {
            spawn_steam_art_fetch(&state, art_tx.clone());
        }
        let audio = AudioFeedback::new();
        if settings.sound
            && let Some(audio) = &audio
        {
            audio.play(Tone::Boot, settings.sound_volume);
        }
        Self {
            settings,
            migrated,
            persisted,
            category_position: start.index() as f32,
            item_positions,
            state,
            controller,
            audio,
            wave_time: 0.0,
            textures: HashMap::new(),
            submenu: None,
            background_override,
            generated_art: HashMap::new(),
            art_pending: HashSet::new(),
            art_tx,
            art_rx,
            options_open: false,
            options_selection: 0,
            information_open: false,
            pending_destructive: None,
            boot_at: Instant::now(),
            toast: None,
            last_status_refresh: Instant::now(),
            last_media_refresh: Instant::now(),
            message_tx,
            message_rx,
        }
    }

    fn update_runtime(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|input| input.stable_dt).clamp(0.0, 0.05);
        if !self.settings.reduced_motion {
            self.wave_time += dt;
        }
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
        if let Some(submenu) = &mut self.submenu {
            let target = submenu.selection as f32;
            submenu.position = if self.settings.reduced_motion {
                target
            } else {
                ease(submenu.position, target, dt, 15.0)
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
        if self.submenu.is_some() {
            self.handle_submenu_action(action);
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

    fn option_entries(&self) -> Vec<(String, OptionEntry)> {
        let mut entries = Vec::new();
        let item = self.state.selected_item();
        if !matches!(self.state.mode, Mode::Settings | Mode::Network) {
            let favorite = item.is_some_and(|item| self.persisted.favorites.contains(&item.id));
            entries.push((
                if favorite {
                    "Remove Favorite"
                } else {
                    "Add Favorite"
                }
                .to_owned(),
                OptionEntry::Favorite,
            ));
        }
        if self.state.mode == Mode::Photo
            && let Some(Action::Open(path)) = item.map(|item| &item.action)
        {
            entries.push((
                "Set as Background".to_owned(),
                OptionEntry::SetBackground(path.clone()),
            ));
        }
        entries.push(("Information".to_owned(), OptionEntry::Information));
        entries.push(("Close".to_owned(), OptionEntry::Close));
        entries
    }

    fn handle_options_action(&mut self, action: InputAction) {
        let entries = self.option_entries();
        match action {
            InputAction::PreviousItem => {
                self.options_selection = self.options_selection.saturating_sub(1);
                self.feedback(Tone::Navigate);
            }
            InputAction::NextItem => {
                self.options_selection =
                    (self.options_selection + 1).min(entries.len().saturating_sub(1));
                self.feedback(Tone::Navigate);
            }
            InputAction::Confirm => {
                self.options_open = false;
                match entries
                    .into_iter()
                    .nth(self.options_selection)
                    .map(|(_, entry)| entry)
                {
                    Some(OptionEntry::Favorite) => self.toggle_favorite(),
                    Some(OptionEntry::Information) => {
                        self.information_open = true;
                        self.feedback(Tone::Confirm);
                    }
                    Some(OptionEntry::SetBackground(path)) => self.set_background(path),
                    _ => self.feedback(Tone::Back),
                }
            }
            InputAction::Back | InputAction::Context => {
                self.options_open = false;
                self.feedback(Tone::Back);
            }
            _ => {}
        }
    }

    fn set_background(&mut self, path: PathBuf) {
        self.settings.background_image = Some(path);
        self.background_override = None;
        self.refresh_settings_items();
        let _ = self.settings.save(self.migrated);
        self.migrated = false;
        self.toast("Background updated");
        self.feedback(Tone::Confirm);
    }

    fn navigate_mode(&mut self, delta: isize) {
        self.state.switch_mode(delta);
        self.options_open = false;
        self.pending_destructive = None;
        self.submenu = None;
        self.feedback(Tone::Navigate);
    }

    fn handle_submenu_action(&mut self, action: InputAction) {
        match action {
            InputAction::PreviousItem | InputAction::NextItem => {
                let Some(submenu) = &mut self.submenu else {
                    return;
                };
                let len = submenu.items.len();
                if len > 0 {
                    let delta: isize = if matches!(action, InputAction::PreviousItem) {
                        -1
                    } else {
                        1
                    };
                    submenu.selection =
                        (submenu.selection as isize + delta).rem_euclid(len as isize) as usize;
                }
                self.feedback(Tone::Navigate);
            }
            InputAction::PreviousMode => self.navigate_mode(-1),
            InputAction::NextMode => self.navigate_mode(1),
            InputAction::Back | InputAction::Settings => {
                self.submenu = None;
                self.feedback(Tone::Back);
            }
            InputAction::Confirm => self.confirm_submenu_item(),
            InputAction::Context | InputAction::Favorite => {}
        }
    }

    fn open_submenu(&mut self, group: SettingsGroup) {
        let items = sources::settings_group_items(group, &self.settings);
        self.submenu = Some(SubMenu {
            group,
            items,
            selection: 0,
            position: 0.0,
        });
        self.feedback(Tone::Confirm);
    }

    fn confirm_submenu_item(&mut self) {
        let Some(item) = self
            .submenu
            .as_ref()
            .and_then(|submenu| submenu.items.get(submenu.selection))
            .cloned()
        else {
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
        match &item.action {
            Action::Setting(setting) => {
                let setting = *setting;
                let had_background = self.settings.background_image.is_some();
                adjust_setting(setting, &mut self.settings);
                self.controller
                    .set_bindings(self.settings.controller.clone());
                self.refresh_settings_items();
                if setting == SettingAction::Background {
                    self.background_override = None;
                    self.toast(if had_background {
                        "Background cleared — monthly gradient restored"
                    } else {
                        "Pick one under Photo: △ Options → Set as background"
                    });
                } else {
                    self.toast("Setting updated");
                }
                self.feedback(Tone::Confirm);
            }
            Action::System(system) if system.destructive() => {
                self.pending_destructive = Some(item.action);
                self.feedback(Tone::Warning);
            }
            _ => self.execute_in_place(&item.action),
        }
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
        if let Action::Group(group) = item.action {
            self.open_submenu(group);
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
        let items = sources::setting_root_items();
        let len = items.len();
        self.state.items.insert(Mode::Settings, items);
        let selection = self.state.selections.entry(Mode::Settings).or_default();
        *selection = (*selection).min(len.saturating_sub(1));
        if let Some(submenu) = &mut self.submenu {
            submenu.items = sources::settings_group_items(submenu.group, &self.settings);
            submenu.selection = submenu.selection.min(submenu.items.len().saturating_sub(1));
        }
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
        self.paint_backdrop(ui.ctx(), &painter, rect);
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
        if !self.settings.reduced_motion
            && let Some((veil, wordmark)) = boot_phase(self.boot_at.elapsed().as_secs_f32())
        {
            draw_boot(&painter, rect, veil, wordmark, self.settings.accent);
        }
    }

    fn paint_backdrop(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: Rect) {
        let background = self
            .background_override
            .clone()
            .or_else(|| self.settings.background_image.clone());
        let mut image_drawn = false;
        if let Some(path) = background
            && let Some(texture) = self.texture_for(ctx, &path, TextureTier::Full)
        {
            let texture_id = texture.id();
            let source = texture.size_vec2();
            let inset = rect.shrink(1.0);
            let alpha = if self.settings.transparent { 244 } else { 255 };
            painter.add(
                egui::epaint::RectShape::filled(
                    inset,
                    egui::CornerRadius::same(26),
                    Color32::from_white_alpha(alpha),
                )
                .with_texture(texture_id, cover_uv(source, inset.size())),
            );
            // Legibility scrim so white text and waves keep contrast on any picture.
            painter.add(egui::epaint::RectShape::filled(
                inset,
                egui::CornerRadius::same(26),
                Color32::from_black_alpha(88),
            ));
            image_drawn = true;
        }
        paint_background(painter, rect, self.wave_time, &self.settings, image_drawn);
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
        pieces.push(Local::now().format("%-m/%-d %-I:%M %p").to_string());
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
        // The active category sits on the same vertical spine the item column
        // descends from, left of center like the original crossbar.
        let center = Pos2::new(
            rect.center().x - rect.width() * 0.08,
            rect.top() + rect.height() * 0.30,
        );
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
            shadow_icon(
                painter,
                mode,
                Pos2::new(x, center.y),
                size,
                Color32::from_white_alpha(alpha),
            );
            if mode == self.state.mode {
                let label = match &self.submenu {
                    Some(submenu) => format!(
                        "{} · {}",
                        mode.title(),
                        submenu.group.title().to_ascii_uppercase()
                    ),
                    None => mode.title().to_owned(),
                };
                shadow_text(
                    painter,
                    Pos2::new(x, center.y + 43.0),
                    Align2::CENTER_TOP,
                    &label,
                    FontId::proportional(15.0),
                    Color32::WHITE,
                );
            }
        }
    }

    fn draw_items(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, rect: Rect) {
        let mode = self.state.mode;
        let in_submenu = self.submenu.is_some();
        let (items, selected, visual) = if let Some(submenu) = &self.submenu {
            (submenu.items.clone(), submenu.selection, submenu.position)
        } else {
            let selected = self.state.selected_index();
            (
                self.state.mode_items().to_vec(),
                selected,
                self.item_positions
                    .get(&mode)
                    .copied()
                    .unwrap_or(selected as f32),
            )
        };
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

        if !in_submenu {
            self.draw_hero_preview(ui.ctx(), painter, rect, items.get(selected));
        }

        // Items above the selection jump over the category crossbar, like the
        // original XMB; the ramp keeps scrolling continuous.
        let category_y = rect.top() + rect.height() * 0.30;
        let crossbar_gap = (anchor.y - category_y) - row_height * 0.25;
        for (index, item) in items.iter().enumerate() {
            let delta = index as f32 - visual;
            if delta.abs() > 4.3 {
                continue;
            }
            let ramp = (-delta).clamp(0.0, 1.0);
            let ramp = ramp * ramp * (3.0 - 2.0 * ramp);
            let y = anchor.y + delta * row_height - ramp * crossbar_gap;
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
                    ui.id()
                        .with(("item", mode.index(), in_submenu, index)),
                    Sense::click(),
                )
                .clicked()
            {
                if let Some(submenu) = &mut self.submenu {
                    submenu.selection = index;
                } else {
                    self.state.selections.insert(mode, index);
                }
            }
            let icon_path = item
                .art
                .clone()
                .or_else(|| self.generated_art.get(&item.id).and_then(Clone::clone));
            let icon = icon_path
                .as_deref()
                .and_then(|path| self.texture_for(ui.ctx(), path, TextureTier::Thumb))
                .map(|texture| (texture.id(), texture.size_vec2()));
            if let Some((texture_id, source)) = icon {
                let side = if active { 47.0 } else { 31.0 };
                let display = fit_size(source, Vec2::splat(side), 3.0);
                painter.add(
                    egui::epaint::RectShape::filled(
                        Rect::from_center_size(Pos2::new(anchor.x, y), display),
                        egui::CornerRadius::same(3),
                        Color32::from_white_alpha(opacity),
                    )
                    .with_texture(texture_id, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))),
                );
            } else {
                let color = if item.available {
                    Color32::from_white_alpha(opacity)
                } else {
                    Color32::from_gray(105)
                };
                shadow_icon(painter, mode, Pos2::new(anchor.x, y), icon_size, color);
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

    /// A soft-shadowed, rounded, aspect-correct preview of the selected
    /// item's artwork on the right side. Small sources (resolved theme icons)
    /// are skipped rather than blown up into pixel mush.
    fn draw_hero_preview(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: Rect,
        item: Option<&LibraryItem>,
    ) {
        let Some(item) = item else {
            return;
        };
        let Some(path) = self.art_for(item) else {
            return;
        };
        let Some(texture) = self.texture_for(ctx, &path, TextureTier::Full) else {
            return;
        };
        let source = texture.size_vec2();
        let texture_id = texture.id();
        if source.max_elem() < 280.0 {
            return;
        }
        // Bottom-right corner, clear of the selected item's title row at any
        // window shape.
        let bounds = Vec2::new(rect.width() * 0.26, rect.height() * 0.38);
        let display = fit_size(source, bounds, 1.2);
        let center = Pos2::new(
            rect.right() - rect.width() * 0.05 - display.x * 0.5,
            rect.bottom() - rect.height() * 0.09 - display.y * 0.5,
        );
        let frame = Rect::from_center_size(center, display);
        let mut shadow = egui::epaint::RectShape::filled(
            frame.expand(4.0),
            egui::CornerRadius::same(14),
            Color32::from_black_alpha(120),
        );
        shadow.blur_width = 30.0;
        painter.add(shadow);
        painter.add(
            egui::epaint::RectShape::new(
                frame,
                egui::CornerRadius::same(10),
                Color32::from_white_alpha(248),
                Stroke::new(1.0, Color32::from_white_alpha(70)),
                egui::StrokeKind::Inside,
            )
            .with_texture(texture_id, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))),
        );
    }

    fn texture_for(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
        tier: TextureTier,
    ) -> Option<&TextureHandle> {
        let key = (path.to_path_buf(), tier);
        if !self.textures.contains_key(&key) {
            let image = load_visual(path)?;
            // No mipmaps on the wgpu backend, so list icons get their own
            // small high-quality downscale instead of GPU-minified originals.
            let image = match tier {
                TextureTier::Thumb if image.width().max(image.height()) > 96 => {
                    image.resize(96, 96, image::imageops::FilterType::Lanczos3)
                }
                TextureTier::Full if image.width().max(image.height()) > 1600 => {
                    image.resize(1600, 1600, image::imageops::FilterType::CatmullRom)
                }
                _ => image,
            };
            let image = image.to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            let pixels = image.into_raw();
            let color = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
            let suffix = match tier {
                TextureTier::Thumb => "thumb",
                TextureTier::Full => "full",
            };
            let texture = ctx.load_texture(
                format!("{}#{suffix}", path.display()),
                color,
                TextureOptions::LINEAR,
            );
            self.textures.insert(key.clone(), texture);
        }
        self.textures.get(&key)
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
        let text = if self.options_open || self.submenu.is_some() {
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
        let entries = self.option_entries();
        let panel = Rect::from_min_size(
            Pos2::new(rect.right() - 300.0, rect.top() + rect.height() * 0.34),
            Vec2::new(260.0, 66.0 + entries.len() as f32 * 42.0),
        );
        painter.rect_filled(panel, 4.0, Color32::from_black_alpha(205));
        painter.rect_stroke(
            panel,
            4.0,
            Stroke::new(1.0, Color32::from_white_alpha(100)),
            egui::StrokeKind::Inside,
        );
        shadow_text(
            painter,
            Pos2::new(panel.left() + 24.0, panel.top() + 28.0),
            Align2::LEFT_CENTER,
            "OPTIONS",
            FontId::proportional(13.0),
            Color32::from_white_alpha(170),
        );
        for (index, (label, _)) in entries.iter().enumerate() {
            shadow_text(
                painter,
                Pos2::new(
                    panel.left() + 24.0,
                    panel.top() + 70.0 + index as f32 * 42.0,
                ),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(17.0),
                if index == self.options_selection {
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

fn paint_background(
    painter: &egui::Painter,
    rect: Rect,
    time: f32,
    settings: &Settings,
    image_drawn: bool,
) {
    let inset = rect.shrink(1.0);
    if !image_drawn {
        let [top, bottom] = monthly_palette(
            Local::now().month0() as usize,
            settings.theme,
            settings.transparent,
        );
        let mut mesh = Mesh::default();
        mesh.colored_vertex(inset.left_top(), top);
        mesh.colored_vertex(inset.right_top(), top);
        mesh.colored_vertex(inset.right_bottom(), bottom);
        mesh.colored_vertex(inset.left_bottom(), bottom);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(mesh));
    }
    painter.rect_stroke(
        inset,
        26.0,
        Stroke::new(1.0, Color32::from_white_alpha(45)),
        egui::StrokeKind::Inside,
    );
    if settings.waves {
        let accent = accent_color(settings.accent);
        paint_wave_ribbons(painter, rect, time, accent);
        paint_sparkles(painter, rect, time, accent);
    }
}

/// The signature XMB element: filled translucent ribbons whose bright crest
/// fades into the background, drawn as vertical gradient strips under a
/// two-harmonic crest line with a layered glow.
fn paint_wave_ribbons(painter: &egui::Painter, rect: Rect, time: f32, accent: Color32) {
    const STEPS: usize = 96;
    let lanes = [
        // (vertical anchor, amplitude, cycles, speed, crest alpha, fill alpha)
        (0.70_f32, 0.052_f32, 1.15_f32, 0.26_f32, 60_u8, 26_u8),
        (0.78, 0.040, 1.55, 0.38, 44, 18),
    ];
    for (anchor, amplitude, cycles, speed, crest_alpha, fill_alpha) in lanes {
        let crest = |unit: f32| -> f32 {
            let phase = unit * std::f32::consts::TAU * cycles + time * speed;
            let swell = phase.sin();
            let ripple = (phase * 2.7 + time * 0.11).sin() * 0.28;
            rect.top() + rect.height() * anchor + (swell + ripple) * rect.height() * amplitude
        };
        let mut mesh = Mesh::default();
        let mut crest_points = Vec::with_capacity(STEPS + 1);
        for step in 0..=STEPS {
            let unit = step as f32 / STEPS as f32;
            let x = rect.left() + unit * rect.width();
            let y = crest(unit);
            crest_points.push(Pos2::new(x, y));
            mesh.colored_vertex(Pos2::new(x, y), tint(accent, fill_alpha));
            mesh.colored_vertex(Pos2::new(x, rect.bottom()), Color32::TRANSPARENT);
        }
        for step in 0..STEPS as u32 {
            let base = step * 2;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 3, base + 2);
        }
        painter.add(Shape::mesh(mesh));
        for (width, alpha) in [(4.6, crest_alpha / 4), (2.2, crest_alpha / 2), (1.1, crest_alpha)] {
            painter.add(Shape::line(
                crest_points.clone(),
                Stroke::new(width, tint(accent, alpha)),
            ));
        }
    }
}

/// Small lights drifting along the wave band, each fully determined by its
/// index and the shared clock so the field stays stable frame to frame.
fn paint_sparkles(painter: &egui::Painter, rect: Rect, time: f32, accent: Color32) {
    const COUNT: usize = 26;
    for index in 0..COUNT {
        let seed = index as f32 * 2.399963;
        let hash = (seed.sin() * 43758.547).fract().abs();
        let drift = 0.010 + hash * 0.022;
        let unit = (seed * 0.618 + time * drift).fract();
        let x = rect.left() + unit * rect.width();
        let band = 0.66 + (seed * 1.7).sin().abs() * 0.20;
        let bob = ((time * (0.5 + hash * 0.7)) + seed * 7.0).sin() * rect.height() * 0.035;
        let y = rect.top() + rect.height() * band + bob;
        let pulse = ((time * (1.1 + hash * 1.4) + seed * 3.0).sin() * 0.5 + 0.5).powi(2);
        let alpha = (30.0 + pulse * 150.0) as u8;
        let radius = 0.9 + hash * 1.6 + pulse * 0.7;
        painter.circle_filled(Pos2::new(x, y), radius + 1.6, tint(accent, alpha / 5));
        painter.circle_filled(Pos2::new(x, y), radius, tint(accent, alpha));
    }
}

fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn accent_color(accent: usize) -> Color32 {
    match accent {
        1 => Color32::from_rgb(150, 226, 255),
        2 => Color32::from_rgb(255, 214, 140),
        3 => Color32::from_rgb(255, 164, 196),
        4 => Color32::from_rgb(158, 240, 186),
        _ => Color32::from_rgb(235, 240, 248),
    }
}

fn monthly_palette(month: usize, theme: usize, transparent: bool) -> [Color32; 2] {
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
    let [top_alpha, bottom_alpha] = if transparent { [238, 246] } else { [255, 255] };
    [
        Color32::from_rgba_premultiplied(colors[0].0, colors[0].1, colors[0].2, top_alpha),
        Color32::from_rgba_premultiplied(colors[1].0, colors[1].1, colors[1].2, bottom_alpha),
    ]
}

/// Original vector icons for the seven categories, drawn at any size from
/// strokes and fills so they stay crisp and carry the wave accent tint.
fn shadow_icon(painter: &egui::Painter, mode: Mode, center: Pos2, size: f32, color: Color32) {
    let shadow = Color32::from_black_alpha(color.a().saturating_sub(60));
    paint_category_icon(painter, mode, center + Vec2::new(1.2, 1.5), size, shadow);
    paint_category_icon(painter, mode, center, size, color);
}

fn paint_category_icon(painter: &egui::Painter, mode: Mode, center: Pos2, size: f32, color: Color32) {
    let unit = size / 2.0;
    let stroke = Stroke::new((size * 0.075).max(1.4), color);
    match mode {
        Mode::Settings => {
            // Gear: ring, radial teeth, hub.
            let radius = unit * 0.62;
            painter.circle_stroke(center, radius, stroke);
            painter.circle_filled(center, unit * 0.2, color);
            for tooth in 0..8 {
                let angle = tooth as f32 / 8.0 * std::f32::consts::TAU;
                let direction = Vec2::angled(angle);
                painter.line_segment(
                    [
                        center + direction * (radius + stroke.width * 0.4),
                        center + direction * unit * 0.95,
                    ],
                    stroke,
                );
            }
        }
        Mode::Extras => {
            // Application drawer: 2×2 rounded tiles.
            let tile = unit * 0.72;
            let gap = unit * 0.18;
            for (dx, dy) in [(-1.0_f32, -1.0_f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let tile_center = center + Vec2::new(dx, dy) * (tile + gap) / 2.0;
                painter.rect_stroke(
                    Rect::from_center_size(tile_center, Vec2::splat(tile)),
                    tile * 0.22,
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
        }
        Mode::Photo => {
            // Framed landscape: border, sun, two peaks.
            let frame = Rect::from_center_size(center, Vec2::new(size * 0.98, size * 0.78));
            painter.rect_stroke(frame, size * 0.08, stroke, egui::StrokeKind::Middle);
            painter.circle_filled(
                frame.min + Vec2::new(frame.width() * 0.28, frame.height() * 0.3),
                size * 0.09,
                color,
            );
            let floor = frame.bottom() - stroke.width;
            let points = vec![
                Pos2::new(frame.left() + frame.width() * 0.1, floor),
                Pos2::new(frame.left() + frame.width() * 0.38, floor - frame.height() * 0.42),
                Pos2::new(frame.left() + frame.width() * 0.58, floor - frame.height() * 0.14),
                Pos2::new(frame.left() + frame.width() * 0.74, floor - frame.height() * 0.34),
                Pos2::new(frame.right() - frame.width() * 0.08, floor),
            ];
            painter.add(Shape::line(points, stroke));
        }
        Mode::Music => {
            // Eighth note: head, stem, swept flag.
            let head = center + Vec2::new(-unit * 0.3, unit * 0.55);
            painter.circle_filled(head, unit * 0.3, color);
            let stem_top = head + Vec2::new(unit * 0.28, -unit * 1.35);
            painter.line_segment([head + Vec2::new(unit * 0.28, 0.0), stem_top], stroke);
            let flag = vec![
                stem_top,
                stem_top + Vec2::new(unit * 0.5, unit * 0.22),
                stem_top + Vec2::new(unit * 0.62, unit * 0.68),
            ];
            painter.add(Shape::line(flag, Stroke::new(stroke.width * 1.15, color)));
        }
        Mode::Video => {
            // Filmstrip: frame with sprocket bands.
            let frame = Rect::from_center_size(center, Vec2::new(size * 0.98, size * 0.74));
            painter.rect_stroke(frame, size * 0.06, stroke, egui::StrokeKind::Middle);
            for row in 0..3 {
                let y = frame.top() + frame.height() * (0.22 + row as f32 * 0.28);
                for edge in [frame.left() + frame.width() * 0.12, frame.right() - frame.width() * 0.12]
                {
                    painter.rect_filled(
                        Rect::from_center_size(Pos2::new(edge, y), Vec2::splat(size * 0.085)),
                        1.0,
                        color,
                    );
                }
            }
            let play = vec![
                center + Vec2::new(-unit * 0.16, -unit * 0.26),
                center + Vec2::new(unit * 0.28, 0.0),
                center + Vec2::new(-unit * 0.16, unit * 0.26),
            ];
            painter.add(Shape::convex_polygon(play, color, Stroke::NONE));
        }
        Mode::Game => {
            // Gamepad: body, directional cross, action dots.
            let body = Rect::from_center_size(center, Vec2::new(size * 1.02, size * 0.56));
            painter.rect_stroke(body, body.height() * 0.45, stroke, egui::StrokeKind::Middle);
            let pad = center + Vec2::new(-unit * 0.48, 0.0);
            let arm = unit * 0.24;
            painter.line_segment([pad - Vec2::new(arm, 0.0), pad + Vec2::new(arm, 0.0)], stroke);
            painter.line_segment([pad - Vec2::new(0.0, arm), pad + Vec2::new(0.0, arm)], stroke);
            let buttons = center + Vec2::new(unit * 0.48, 0.0);
            for direction in [
                Vec2::new(0.0, -1.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, 1.0),
                Vec2::new(-1.0, 0.0),
            ] {
                painter.circle_filled(buttons + direction * unit * 0.2, size * 0.05, color);
            }
        }
        Mode::Network => {
            // Globe: sphere, meridian, equator.
            let radius = unit * 0.8;
            painter.circle_stroke(center, radius, stroke);
            painter.line_segment(
                [center - Vec2::new(radius, 0.0), center + Vec2::new(radius, 0.0)],
                stroke,
            );
            let arc = |squeeze: f32| {
                let mut points = Vec::with_capacity(25);
                for step in 0..=24 {
                    let angle = step as f32 / 24.0 * std::f32::consts::TAU;
                    points.push(
                        center + Vec2::new(angle.cos() * radius * squeeze, angle.sin() * radius),
                    );
                }
                points
            };
            painter.add(Shape::closed_line(arc(0.42), stroke));
        }
    }
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

/// Boot-in timeline: returns (veil alpha, wordmark alpha), or None once the
/// intro has finished. The veil hides the interface briefly, the wordmark
/// fades in over it and dissolves as the veil lifts.
fn boot_phase(elapsed: f32) -> Option<(u8, u8)> {
    const DONE: f32 = 1.9;
    if elapsed >= DONE {
        return None;
    }
    let ramp = |from: f32, to: f32| ((elapsed - from) / (to - from)).clamp(0.0, 1.0);
    let veil = 1.0 - ramp(0.55, 1.35);
    let wordmark = ramp(0.08, 0.42) * (1.0 - ramp(1.15, 1.65));
    Some(((veil * 255.0) as u8, (wordmark * 255.0) as u8))
}

fn draw_boot(painter: &egui::Painter, rect: Rect, veil: u8, wordmark: u8, accent: usize) {
    if veil > 0 {
        painter.rect_filled(rect.shrink(1.0), 26.0, Color32::from_black_alpha(veil));
    }
    if wordmark > 0 {
        let center = rect.center() - Vec2::new(0.0, rect.height() * 0.04);
        shadow_text(
            painter,
            center,
            Align2::CENTER_CENTER,
            "XMB",
            FontId::proportional(58.0),
            Color32::from_white_alpha(wordmark),
        );
        shadow_text(
            painter,
            center + Vec2::new(0.0, 44.0),
            Align2::CENTER_CENTER,
            "L A U N C H E R",
            FontId::proportional(15.0),
            tint(accent_color(accent), wordmark.saturating_sub(45)),
        );
    }
}

fn spawn_steam_art_fetch(state: &AppState, sender: Sender<(String, Option<PathBuf>)>) {
    let missing = state
        .items
        .get(&Mode::Game)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.art.is_none())
                .filter_map(|item| match item.action {
                    Action::Steam(app_id) => Some((item.id.clone(), app_id)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if missing.is_empty() {
        return;
    }
    thread::spawn(move || {
        for (item_id, app_id) in missing {
            if let Some(path) = sources::fetch_steam_cover(app_id) {
                let _ = sender.send((item_id, Some(path)));
            }
        }
    });
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
        SettingAction::Background => settings.background_image = None,
        SettingAction::ResetAppearance => {
            let defaults = Settings::default();
            settings.theme = defaults.theme;
            settings.accent = defaults.accent;
            settings.transparent = defaults.transparent;
            settings.waves = defaults.waves;
            settings.reduced_motion = defaults.reduced_motion;
            settings.background_image = None;
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

/// Scale `source` to fit inside `bounds`, preserving aspect ratio and never
/// upscaling beyond `max_upscale` of the source resolution.
fn fit_size(source: Vec2, bounds: Vec2, max_upscale: f32) -> Vec2 {
    if source.x <= 0.0 || source.y <= 0.0 {
        return bounds;
    }
    let scale = (bounds.x / source.x)
        .min(bounds.y / source.y)
        .min(max_upscale);
    source * scale
}

/// UV rectangle that crops `source` centrally to the aspect ratio of
/// `target`, for aspect-fill drawing without distortion.
fn cover_uv(source: Vec2, target: Vec2) -> Rect {
    if source.x <= 0.0 || source.y <= 0.0 || target.x <= 0.0 || target.y <= 0.0 {
        return Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    }
    let source_aspect = source.x / source.y;
    let target_aspect = target.x / target.y;
    if source_aspect > target_aspect {
        let used = target_aspect / source_aspect;
        Rect::from_min_max(
            Pos2::new((1.0 - used) / 2.0, 0.0),
            Pos2::new((1.0 + used) / 2.0, 1.0),
        )
    } else {
        let used = source_aspect / target_aspect;
        Rect::from_min_max(
            Pos2::new(0.0, (1.0 - used) / 2.0),
            Pos2::new(1.0, (1.0 + used) / 2.0),
        )
    }
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
            let [top, bottom] = monthly_palette(month, 0, true);
            assert_ne!(top, bottom);
        }
    }

    #[test]
    fn opaque_mode_removes_translucency() {
        for color in monthly_palette(3, 0, false) {
            assert_eq!(color.a(), 255);
        }
        for color in monthly_palette(3, 0, true) {
            assert!(color.a() < 255);
        }
    }

    #[test]
    fn accents_are_distinct() {
        let colors = (0..5).map(accent_color).collect::<Vec<_>>();
        for (index, color) in colors.iter().enumerate() {
            assert!(!colors[..index].contains(color));
        }
    }

    #[test]
    fn fit_preserves_aspect_and_never_overupscales() {
        // 2:3 cover into a square box keeps the ratio
        let display = fit_size(Vec2::new(600.0, 900.0), Vec2::splat(47.0), 3.0);
        assert!((display.x / display.y - 600.0 / 900.0).abs() < 1e-4);
        assert!(display.y <= 47.0);
        // a tiny 12px icon may only grow 3x, not fill the box
        let display = fit_size(Vec2::new(12.0, 12.0), Vec2::splat(47.0), 3.0);
        assert_eq!(display, Vec2::splat(36.0));
    }

    #[test]
    fn cover_uv_crops_the_long_axis_centrally() {
        // wide source on a square target: x is cropped, y full
        let uv = cover_uv(Vec2::new(200.0, 100.0), Vec2::new(100.0, 100.0));
        assert!((uv.min.x - 0.25).abs() < 1e-4 && (uv.max.x - 0.75).abs() < 1e-4);
        assert_eq!(uv.min.y, 0.0);
        assert_eq!(uv.max.y, 1.0);
        // tall source on a square target: y is cropped
        let uv = cover_uv(Vec2::new(100.0, 200.0), Vec2::new(100.0, 100.0));
        assert!((uv.min.y - 0.25).abs() < 1e-4 && (uv.max.y - 0.75).abs() < 1e-4);
    }

    #[test]
    fn boot_timeline_covers_veil_then_wordmark_then_ends() {
        let (veil, wordmark) = boot_phase(0.0).unwrap();
        assert_eq!(veil, 255);
        assert_eq!(wordmark, 0);
        let (_, wordmark) = boot_phase(0.8).unwrap();
        assert!(wordmark > 200);
        let (veil, _) = boot_phase(1.5).unwrap();
        assert_eq!(veil, 0);
        assert!(boot_phase(1.9).is_none());
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
