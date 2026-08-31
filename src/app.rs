use crate::engine::{self, EngineEvent, EngineHandle};
use crate::model::{
    ConflictPolicy, DeleteMode, Language, LinkPolicy, OperationKind, ProgressSnapshot, Settings,
    TaskRequest,
};
use crate::tools::{self, RenamePlan, SizeStats};
use crate::windows::shell_menu::{self, InstanceGuard, PendingCommand};
use eframe::egui;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(244, 247, 252);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(37, 99, 235);
const ACCENT_SOFT: egui::Color32 = egui::Color32::from_rgb(232, 240, 254);
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(24, 35, 52);
const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(99, 115, 137);
const BORDER: egui::Color32 = egui::Color32::from_rgb(220, 227, 238);
const DANGER: egui::Color32 = egui::Color32::from_rgb(220, 38, 38);

pub(crate) const SETTINGS_SIZE: egui::Vec2 = egui::vec2(440.0, 700.0);
pub(crate) const SETTINGS_MIN_SIZE: egui::Vec2 = egui::vec2(380.0, 620.0);
const PROGRESS_SIZE: egui::Vec2 = egui::vec2(640.0, 400.0);
const SOURCE_PATH_SIZE: egui::Vec2 = egui::vec2(520.0, 240.0);
const SOURCE_PATH_MIN_SIZE: egui::Vec2 = egui::vec2(400.0, 200.0);
const SIZE_DIALOG_SIZE: egui::Vec2 = egui::vec2(440.0, 280.0);
const RENAME_DIALOG_SIZE: egui::Vec2 = egui::vec2(700.0, 580.0);
const RENAME_DIALOG_MIN_SIZE: egui::Vec2 = egui::vec2(620.0, 500.0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum UiMode {
    Settings,
    Progress,
}

struct ActiveTask {
    kind: OperationKind,
    handle: EngineHandle,
    progress: ProgressSnapshot,
    started: Instant,
}

struct LastResult {
    kind: OperationKind,
    progress: ProgressSnapshot,
    cancelled: bool,
}

pub struct FastCopyApp {
    settings: Settings,
    active: Option<ActiveTask>,
    queued: VecDeque<TaskRequest>,
    last_result: Option<LastResult>,
    settings_status: String,
    shell_user: bool,
    shell_machine: bool,
    last_pending_check: Instant,
    last_mode: Option<UiMode>,
    close_after_task: bool,
    _instance_guard: InstanceGuard,
}

impl FastCopyApp {
    pub fn new(context: &eframe::CreationContext<'_>, instance_guard: InstanceGuard) -> Self {
        install_chinese_font(&context.egui_ctx);
        configure_style(&context.egui_ctx);
        let settings = load_settings();
        let mut app = Self {
            settings,
            active: None,
            queued: VecDeque::new(),
            last_result: None,
            settings_status: String::new(),
            shell_user: shell_menu::is_user_registered(),
            shell_machine: shell_menu::is_machine_registered(),
            last_pending_check: Instant::now() - Duration::from_secs(1),
            last_mode: None,
            close_after_task: false,
            _instance_guard: instance_guard,
        };
        shell_menu::refresh_background_verbs();
        app.collect_pending_commands();
        app.start_next_task();
        app
    }

    fn refresh_shell_status(&mut self) {
        self.shell_user = shell_menu::is_user_registered();
        self.shell_machine = shell_menu::is_machine_registered();
    }

    fn t(&self) -> &'static crate::i18n::Strings {
        self.settings.language.strings()
    }

    fn ui_mode(&self) -> UiMode {
        if self.active.is_some() || !self.queued.is_empty() || self.last_result.is_some() {
            UiMode::Progress
        } else {
            UiMode::Settings
        }
    }

    fn collect_pending_commands(&mut self) {
        let commands = match shell_menu::take_pending() {
            Ok(commands) => commands,
            Err(error) => {
                show_error(self.t(), self.t().pending_failed(&format!("{error:#}")));
                return;
            }
        };
        let mut delete_paths = Vec::new();
        for command in commands {
            match command {
                PendingCommand::Paste(destination) => {
                    match shell_menu::clipboard_task(destination, self.settings.clone(), false) {
                        Ok(request) => self.queued.push_back(request),
                        Err(error) => show_error(self.t(), error.to_string()),
                    }
                }
                PendingCommand::PasteKeep(destination) => {
                    match shell_menu::clipboard_task(destination, self.settings.clone(), true) {
                        Ok(request) => self.queued.push_back(request),
                        Err(error) => show_error(self.t(), error.to_string()),
                    }
                }
                PendingCommand::Delete(path) => {
                    if !delete_paths.contains(&path) {
                        delete_paths.push(path);
                    }
                }
            }
        }
        if !delete_paths.is_empty() {
            self.queued.push_back(TaskRequest {
                kind: OperationKind::Delete,
                sources: delete_paths,
                destination: None,
                settings: self.settings.clone(),
                retry_items: Vec::new(),
            });
        }
    }

    fn process_engine_events(&mut self) {
        let mut finished = None;
        if let Some(active) = &mut self.active {
            while let Some(event) = active.handle.try_recv() {
                match event {
                    EngineEvent::Scanning {
                        total_bytes,
                        total_items,
                        current,
                    } => {
                        let io_started = active.progress.completed_bytes > 0
                            || active.progress.completed_items > 0;
                        if !io_started {
                            active.progress.scanning = true;
                            active.progress.current_path = current.display().to_string();
                        }
                        active.progress.total_bytes = active.progress.total_bytes.max(total_bytes);
                        active.progress.total_items = active.progress.total_items.max(total_items);
                    }
                    EngineEvent::Started {
                        total_bytes,
                        total_items,
                    } => {
                        active.progress.scanning = false;
                        active.progress.total_bytes = active.progress.total_bytes.max(total_bytes);
                        active.progress.total_items = active.progress.total_items.max(total_items);
                    }
                    EngineEvent::Current(path) => {
                        active.progress.current_path = path.display().to_string();
                    }
                    EngineEvent::BytesDone(bytes) => {
                        active.progress.scanning = false;
                        active.progress.completed_bytes =
                            active.progress.completed_bytes.saturating_add(bytes);
                    }
                    EngineEvent::ItemsDone(items) => {
                        active.progress.scanning = false;
                        active.progress.completed_items =
                            active.progress.completed_items.saturating_add(items);
                    }
                    EngineEvent::Failed { item, message } => {
                        if active.progress.errors.len() < 1000 {
                            active.progress.errors.push(message);
                            active.progress.failed.push(item);
                        }
                    }
                    EngineEvent::Error(error) => {
                        if active.progress.errors.len() < 1000 {
                            active.progress.errors.push(error);
                        }
                    }
                    EngineEvent::Finished {
                        cancelled,
                        error_count,
                        skip_count: _,
                    } => {
                        finished = Some((cancelled, error_count));
                    }
                }
            }
        }
        if let Some((cancelled, error_count)) = finished {
            if let Some(active) = self.active.take() {
                let has_errors = error_count > 0 || !active.progress.errors.is_empty();
                if self.settings.notify_when_done(active.kind) {
                    crate::notify::finished(self.t(), active.kind, cancelled, error_count);
                }
                if has_errors {
                    self.last_result = Some(LastResult {
                        kind: active.kind,
                        progress: active.progress,
                        cancelled,
                    });
                }
            }
            self.start_next_task();
            if self.active.is_none() && self.queued.is_empty() && self.last_result.is_none() {
                self.close_after_task = true;
            }
        }
    }

    fn start_next_task(&mut self) {
        if self.active.is_some() {
            return;
        }
        if let Some(request) = self.queued.pop_front() {
            if request.kind == OperationKind::Delete
                && request.settings.delete_mode == DeleteMode::Permanent
                && !confirm_permanent_delete(self.t(), &request.sources)
            {
                if self.queued.is_empty() && self.last_result.is_none() {
                    self.close_after_task = true;
                }
                return;
            }
            self.last_result = None;
            self.active = Some(ActiveTask {
                kind: request.kind,
                handle: engine::start(request),
                progress: ProgressSnapshot {
                    scanning: true,
                    ..ProgressSnapshot::default()
                },
                started: Instant::now(),
            });
        }
    }

    fn apply_window_mode(&mut self, context: &egui::Context) {
        let mode = self.ui_mode();
        if self.last_mode == Some(mode) {
            return;
        }
        self.last_mode = Some(mode);
        let (min_size, inner_size) = match mode {
            UiMode::Settings => (SETTINGS_MIN_SIZE, SETTINGS_SIZE),
            UiMode::Progress => (egui::vec2(520.0, 300.0), PROGRESS_SIZE),
        };
        context.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
        context.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min_size));
        context.send_viewport_cmd(egui::ViewportCommand::InnerSize(inner_size));
        if let Some(position) = crate::windows::centered_outer_position(inner_size) {
            context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
        }
        if mode == UiMode::Progress {
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn show_progress(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        fill_background(ui, |ui| {
            if let Some(active) = &self.active {
                let scanning = active.progress.scanning;
                let title = if scanning {
                    t.scanning.to_owned()
                } else {
                    t.in_progress(active.kind)
                };
                let subtitle = if self.queued.is_empty() {
                    String::new()
                } else {
                    t.queued(self.queued.len())
                };
                show_card(ui, &title, &subtitle, |ui| {
                    let progress = &active.progress;
                    let fraction = if scanning {
                        0.0
                    } else {
                        progress_fraction(progress)
                    };
                    let mut bar = egui::ProgressBar::new(fraction)
                        .desired_width(ui.available_width())
                        .desired_height(16.0)
                        .animate(true)
                        .fill(ACCENT);
                    if !scanning {
                        bar = bar.show_percentage();
                    }
                    ui.add(bar);
                    ui.add_space(10.0);
                    let elapsed = active.started.elapsed().as_secs_f64().max(0.001);
                    if scanning {
                        ui.columns(2, |columns| {
                            show_metric(
                                &mut columns[0],
                                t.scanning,
                                &format!("{}", progress.total_items),
                            );
                            show_metric(
                                &mut columns[1],
                                t.completed,
                                &format_bytes(progress.total_bytes),
                            );
                        });
                        ui.add_space(8.0);
                        ui.colored_label(
                            TEXT_SECONDARY,
                            t.scanned(progress.total_items, &format_bytes(progress.total_bytes)),
                        );
                    } else {
                        let speed = progress.completed_bytes as f64 / elapsed;
                        let remaining = if speed > 0.0 {
                            progress
                                .total_bytes
                                .saturating_sub(progress.completed_bytes)
                                as f64
                                / speed
                        } else {
                            0.0
                        };
                        let items_per_sec = progress.completed_items as f64 / elapsed;
                        ui.columns(2, |columns| {
                            show_metric(
                                &mut columns[0],
                                t.completed,
                                &format!(
                                    "{} / {}",
                                    format_bytes(progress.completed_bytes),
                                    format_bytes(progress.total_bytes)
                                ),
                            );
                            show_metric(
                                &mut columns[1],
                                t.speed,
                                &format!("{}/s", format_bytes(speed as u64)),
                            );
                        });
                        ui.add_space(8.0);
                        ui.columns(2, |columns| {
                            show_metric(
                                &mut columns[0],
                                t.file_speed,
                                &t.items_per_sec(items_per_sec),
                            );
                            show_metric(&mut columns[1], t.eta, &format_duration(remaining));
                        });
                    }
                    ui.add_space(8.0);
                    if !scanning {
                        egui::Frame::new()
                            .fill(BACKGROUND)
                            .corner_radius(8)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.colored_label(
                                    TEXT_SECONDARY,
                                    t.files_elapsed(
                                        progress.completed_items,
                                        progress.total_items,
                                        &format_duration(elapsed),
                                    ),
                                );
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(t.current_file(&progress.current_path))
                                            .color(TEXT_PRIMARY),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&progress.current_path);
                            });
                    } else {
                        egui::Frame::new()
                            .fill(BACKGROUND)
                            .corner_radius(8)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(t.current_file(&progress.current_path))
                                            .color(TEXT_PRIMARY),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&progress.current_path);
                            });
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if active.handle.is_paused() {
                            if ui.add(primary_button(t.resume)).clicked() {
                                active.handle.resume();
                            }
                        } else if ui.button(t.pause).clicked() {
                            active.handle.pause();
                        }
                        if ui
                            .add(
                                egui::Button::new(t.cancel_task)
                                    .fill(egui::Color32::from_rgb(254, 226, 226)),
                            )
                            .clicked()
                        {
                            active.handle.cancel();
                        }
                    });
                    if !progress.errors.is_empty() {
                        ui.collapsing(t.error_details(progress.errors.len()), |ui| {
                            for error in progress.errors.iter().rev().take(50) {
                                ui.colored_label(DANGER, error);
                            }
                        });
                    }
                });
            } else if self.last_result.is_some() {
                let mut retry = false;
                let mut export = false;
                if let Some(result) = &self.last_result {
                    let title =
                        t.result_title(result.kind, result.cancelled, result.progress.errors.len());
                    show_card(ui, &title, "", |ui| {
                        ui.colored_label(
                            TEXT_SECONDARY,
                            t.processed(
                                &format_bytes(result.progress.completed_bytes),
                                result.progress.completed_items,
                            ),
                        );
                        if !result.progress.errors.is_empty() {
                            ui.collapsing(t.error_details(result.progress.errors.len()), |ui| {
                                for error in result.progress.errors.iter().rev().take(200) {
                                    ui.colored_label(DANGER, error);
                                }
                            });
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if !result.progress.failed.is_empty()
                                && ui.add(primary_button(t.retry_failed)).clicked()
                            {
                                retry = true;
                            }
                            if !result.progress.errors.is_empty()
                                && ui.button(t.export_errors).clicked()
                            {
                                export = true;
                            }
                            if ui.button(t.close).clicked() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                    });
                }
                if retry {
                    self.queue_retry();
                }
                if export {
                    self.export_errors();
                }
            }
        });
    }

    fn queue_retry(&mut self) {
        let Some(result) = self.last_result.take() else {
            return;
        };
        if result.progress.failed.is_empty() {
            self.last_result = Some(result);
            return;
        }
        self.queued.push_back(TaskRequest {
            kind: result.kind,
            sources: result
                .progress
                .failed
                .iter()
                .map(|item| item.source.clone())
                .collect(),
            destination: None,
            settings: self.settings.clone(),
            retry_items: result.progress.failed,
        });
    }

    fn export_errors(&mut self) {
        let Some(result) = &self.last_result else {
            return;
        };
        let t = self.t();
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("fastcopy-errors.txt")
            .save_file()
        else {
            return;
        };
        let text = result.progress.errors.join("\r\n");
        match fs::write(&path, text) {
            Ok(()) => self.settings_status = t.export_ok.to_owned(),
            Err(error) => show_error(t, t.export_failed(&error.to_string())),
        }
    }

    fn show_settings_page(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        fill_background(ui, |ui| {
            let size = ui.available_size();
            ui.set_max_size(size);
            ui.allocate_ui(size, |ui| {
                show_card(ui, t.settings_title, t.settings_subtitle, |ui| {
                    ui.set_min_height(ui.available_height());
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.horizontal(|ui| {
                            if ui.add(primary_button(t.save_settings)).clicked() {
                                match save_settings(&self.settings) {
                                    Ok(()) => self.settings_status = t.settings_saved.to_owned(),
                                    Err(error) => {
                                        self.settings_status =
                                            t.settings_save_failed(&error.to_string())
                                    }
                                }
                            }
                            if ui.button(t.close).clicked() {
                                let _ = save_settings(&self.settings);
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                        if !self.settings_status.is_empty() {
                            ui.colored_label(TEXT_SECONDARY, &self.settings_status);
                        }
                        ui.add_space(8.0);
                        ui.with_layout(
                            egui::Layout::top_down(egui::Align::Min).with_cross_justify(true),
                            |ui| {
                                egui::ScrollArea::vertical()
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        self.show_settings_form(ui);
                                    });
                            },
                        );
                    });
                });
            });
        });
    }

    fn show_settings_form(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        ui.label(t.language);
        let mut language = self.settings.language;
        egui::ComboBox::from_id_salt("language")
            .width(ui.available_width())
            .selected_text(language.native_name())
            .show_ui(ui, |ui| {
                for item in [Language::Zh, Language::En] {
                    ui.selectable_value(&mut language, item, item.native_name());
                }
            });
        if language != self.settings.language {
            self.settings.language = language;
            let _ = save_settings(&self.settings);
            shell_menu::try_update_menu_labels();
            shell_menu::refresh_background_verbs();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(
                language.strings().window_title(),
            ));
        }
        ui.add_space(6.0);
        ui.label(t.workers);
        ui.add(
            egui::Slider::new(&mut self.settings.worker_count, 1..=64)
                .suffix(t.workers_suffix)
                .clamping(egui::SliderClamping::Always),
        );
        ui.add_space(6.0);
        ui.label(t.conflict);
        egui::ComboBox::from_id_salt("conflict_policy")
            .width(ui.available_width())
            .selected_text(t.conflict_policy(self.settings.conflict_policy))
            .show_ui(ui, |ui| {
                for policy in [
                    ConflictPolicy::Overwrite,
                    ConflictPolicy::Skip,
                    ConflictPolicy::Rename,
                ] {
                    ui.selectable_value(
                        &mut self.settings.conflict_policy,
                        policy,
                        t.conflict_policy(policy),
                    );
                }
            });
        ui.checkbox(&mut self.settings.verify_file_size, t.verify_size);
        ui.checkbox(&mut self.settings.skip_unchanged, t.skip_unchanged);
        ui.checkbox(&mut self.settings.use_ignore_file, t.use_ignore_file);
        ui.label(t.ignore_file_name);
        if self.settings.use_ignore_file && self.settings.ignore_file_name.trim().is_empty() {
            self.settings.ignore_file_name = crate::model::default_ignore_file_name();
        }
        ui.add_enabled(
            self.settings.use_ignore_file,
            egui::TextEdit::singleline(&mut self.settings.ignore_file_name)
                .desired_width(ui.available_width()),
        );
        ui.add_space(6.0);
        ui.label(t.link_policy);
        egui::ComboBox::from_id_salt("link_policy")
            .width(ui.available_width())
            .selected_text(t.link_policy_label(self.settings.link_policy))
            .show_ui(ui, |ui| {
                for policy in [LinkPolicy::Ignore, LinkPolicy::Follow, LinkPolicy::Preserve] {
                    ui.selectable_value(
                        &mut self.settings.link_policy,
                        policy,
                        t.link_policy_label(policy),
                    );
                }
            });
        ui.add_space(6.0);
        ui.label(t.delete_mode);
        egui::ComboBox::from_id_salt("delete_mode")
            .width(ui.available_width())
            .selected_text(t.delete_mode_label(self.settings.delete_mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.settings.delete_mode,
                    DeleteMode::RecycleBin,
                    t.recycle_bin,
                );
                ui.selectable_value(
                    &mut self.settings.delete_mode,
                    DeleteMode::Permanent,
                    t.permanent,
                );
            });
        ui.checkbox(&mut self.settings.notify_on_finish, t.notify_on_finish);
        ui.separator();
        ui.label(t.shell_status(self.shell_user, self.shell_machine));
        ui.horizontal_wrapped(|ui| {
            if !self.shell_user && !self.shell_machine && ui.button(t.register).clicked() {
                match shell_menu::register() {
                    Ok(()) => {
                        self.refresh_shell_status();
                        self.settings_status = if self.shell_user {
                            t.register_ok.to_owned()
                        } else {
                            t.register_incomplete.to_owned()
                        };
                    }
                    Err(error) => {
                        self.refresh_shell_status();
                        self.settings_status = t.register_failed(&format!("{error:#}"));
                    }
                }
            }
            if self.shell_user && ui.button(t.unregister).clicked() {
                match shell_menu::unregister_user() {
                    Ok(()) => {
                        self.refresh_shell_status();
                        self.settings_status = if self.shell_user {
                            t.unregister_incomplete.to_owned()
                        } else {
                            t.unregister_ok.to_owned()
                        };
                    }
                    Err(error) => {
                        self.refresh_shell_status();
                        self.settings_status = t.unregister_failed(&format!("{error:#}"));
                    }
                }
            }
            if self.shell_machine && ui.button(t.unregister_admin).clicked() {
                match shell_menu::elevate("--unregister-shell") {
                    Ok(()) => {
                        self.refresh_shell_status();
                        self.settings_status = if self.shell_machine {
                            t.unregister_incomplete.to_owned()
                        } else {
                            t.unregister_ok.to_owned()
                        };
                    }
                    Err(error) => {
                        self.refresh_shell_status();
                        self.settings_status = t.unregister_failed(&format!("{error:#}"));
                    }
                }
            }
            if ui.button(t.refresh_status).clicked() {
                self.refresh_shell_status();
            }
        });
    }
}

impl eframe::App for FastCopyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        self.process_engine_events();
        if self.last_pending_check.elapsed() >= Duration::from_millis(300) {
            self.last_pending_check = Instant::now();
            self.collect_pending_commands();
            self.start_next_task();
        }
        if self.close_after_task {
            if self.active.is_some() || !self.queued.is_empty() {
                self.close_after_task = false;
            } else {
                context.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        self.apply_window_mode(&context);

        ui.set_min_size(ui.available_size());
        match self.ui_mode() {
            UiMode::Settings => self.show_settings_page(ui),
            UiMode::Progress => self.show_progress(ui),
        }
        context.request_repaint_after(Duration::from_millis(100));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = save_settings(&self.settings);
        if let Some(active) = &self.active {
            active.handle.cancel();
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BACKGROUND.to_normalized_gamma_f32()
    }
}

struct SourcePathDialog {
    current: String,
    source: String,
    show_current: bool,
    status: String,
    status_ok: bool,
    copied_until: Option<Instant>,
}

impl SourcePathDialog {
    fn new(context: &eframe::CreationContext<'_>, clicked: PathBuf) -> Self {
        install_chinese_font(&context.egui_ctx);
        configure_style(&context.egui_ctx);
        let current = crate::windows::explorer_sel::display_path(&clicked);
        let source = crate::windows::explorer_sel::source_path_for(&clicked);
        let current = current.display().to_string();
        let source = source.display().to_string();
        let show_current = !current.eq_ignore_ascii_case(&source);
        Self {
            current,
            source,
            show_current,
            status: String::new(),
            status_ok: false,
            copied_until: None,
        }
    }
}

impl eframe::App for SourcePathDialog {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = crate::i18n::strings(load_settings().language);
        if let Some(until) = self.copied_until {
            let now = Instant::now();
            if now >= until {
                if self.status_ok {
                    self.status.clear();
                }
                self.copied_until = None;
            } else {
                ctx.request_repaint_after(until.saturating_duration_since(now));
            }
        }
        fill_background(ui, |ui| {
            show_card(ui, t.menu_show_source, "", |ui| {
                if self.show_current {
                    ui.colored_label(TEXT_SECONDARY, t.current_item);
                    ui.add(
                        egui::Label::new(egui::RichText::new(&self.current).monospace())
                            .wrap()
                            .selectable(true),
                    );
                    ui.add_space(6.0);
                }
                ui.colored_label(TEXT_SECONDARY, t.source_path);
                ui.add(
                    egui::TextEdit::multiline(&mut self.source)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(primary_button(t.copy_path)).clicked() {
                        ctx.copy_text(self.source.clone());
                        self.status = t.path_copied.to_owned();
                        self.status_ok = true;
                        self.copied_until = Some(Instant::now() + Duration::from_secs(2));
                    }
                    if ui.button(t.open_path).clicked() {
                        let path = PathBuf::from(self.source.trim());
                        if !path.exists() {
                            self.status = t.path_missing(&path);
                            self.status_ok = false;
                            self.copied_until = None;
                        } else if let Err(error) =
                            crate::windows::explorer_sel::reveal_path(&path)
                        {
                            self.status = t.cannot_open_path(&error);
                            self.status_ok = false;
                            self.copied_until = None;
                        } else {
                            self.status.clear();
                            self.status_ok = false;
                            self.copied_until = None;
                        }
                    }
                    if ui.button(t.close).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if !self.status.is_empty() {
                        let color = if self.status_ok { ACCENT } else { DANGER };
                        ui.label(egui::RichText::new(&self.status).color(color));
                    }
                });
            });
        });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BACKGROUND.to_normalized_gamma_f32()
    }
}

pub(crate) fn run_source_path_dialog(clicked: PathBuf) -> anyhow::Result<()> {
    let t = crate::i18n::strings(load_settings().language);
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/app.png"))
        .map_err(|error| anyhow::anyhow!("{}", t.cannot_load_icon(&error)))?;
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([SOURCE_PATH_SIZE.x, SOURCE_PATH_SIZE.y])
        .with_min_inner_size([SOURCE_PATH_MIN_SIZE.x, SOURCE_PATH_MIN_SIZE.y])
        .with_resizable(true)
        .with_icon(icon);
    if let Some(position) = crate::windows::centered_outer_position(SOURCE_PATH_SIZE) {
        viewport = viewport.with_position(position);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        t.menu_show_source,
        options,
        Box::new(move |context| Ok(Box::new(SourcePathDialog::new(context, clicked)))),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

struct SizeDialog {
    shared: Arc<Mutex<(SizeStats, String, bool)>>,
    cancelled: Arc<AtomicBool>,
    status: String,
    status_ok: bool,
    copied_until: Option<Instant>,
}

impl SizeDialog {
    fn new(context: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        install_chinese_font(&context.egui_ctx);
        configure_style(&context.egui_ctx);
        let shared = Arc::new(Mutex::new((SizeStats::default(), String::new(), false)));
        let cancelled = Arc::new(AtomicBool::new(false));
        let shared_worker = Arc::clone(&shared);
        let cancelled_worker = Arc::clone(&cancelled);
        thread::spawn(move || {
            let stats = tools::scan_size(&paths, &cancelled_worker, |stats, path| {
                if let Ok(mut guard) = shared_worker.lock() {
                    guard.0 = stats.clone();
                    guard.1 = path.display().to_string();
                }
            });
            if let Ok(mut guard) = shared_worker.lock() {
                guard.0 = stats;
                guard.2 = true;
            }
        });
        Self {
            shared,
            cancelled,
            status: String::new(),
            status_ok: false,
            copied_until: None,
        }
    }
}

impl eframe::App for SizeDialog {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = crate::i18n::strings(load_settings().language);
        if let Some(until) = self.copied_until {
            let now = Instant::now();
            if now >= until {
                if self.status_ok {
                    self.status.clear();
                }
                self.copied_until = None;
            } else {
                ctx.request_repaint_after(until.saturating_duration_since(now));
            }
        }
        let (stats, current, done) = self
            .shared
            .lock()
            .map(|guard| (guard.0.clone(), guard.1.clone(), guard.2))
            .unwrap_or_default();
        if !done {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        fill_background(ui, |ui| {
            let subtitle = if done {
                String::new()
            } else {
                t.scanning.to_owned()
            };
            show_card(ui, t.menu_size, &subtitle, |ui| {
                ui.columns(2, |columns| {
                    show_metric(&mut columns[0], t.size_files, &stats.files.to_string());
                    show_metric(&mut columns[1], t.size_dirs, &stats.dirs.to_string());
                });
                ui.add_space(8.0);
                ui.columns(2, |columns| {
                    show_metric(
                        &mut columns[0],
                        t.size_bytes,
                        &tools::format_bytes(stats.bytes),
                    );
                    show_metric(&mut columns[1], t.size_errors, &stats.errors.to_string());
                });
                ui.add_space(8.0);
                ui.colored_label(TEXT_SECONDARY, format!("{bytes} B", bytes = stats.bytes));
                if !done && !current.is_empty() {
                    ui.add_space(6.0);
                    ui.colored_label(TEXT_SECONDARY, t.current_file(&current));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add(primary_button(t.size_copy)).clicked() {
                        let text = t.size_summary(stats.files, stats.dirs, stats.bytes);
                        match crate::windows::set_clipboard_text(&text) {
                            Ok(()) => {
                                self.status = t.path_copied.to_owned();
                                self.status_ok = true;
                                self.copied_until = Some(Instant::now() + Duration::from_secs(2));
                            }
                            Err(error) => {
                                self.status = t.clipboard_failed(&error);
                                self.status_ok = false;
                                self.copied_until = None;
                            }
                        }
                    }
                    if ui.button(t.close).clicked() {
                        self.cancelled.store(true, Ordering::Release);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if !self.status.is_empty() {
                        let color = if self.status_ok { ACCENT } else { DANGER };
                        ui.label(egui::RichText::new(&self.status).color(color));
                    }
                });
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BACKGROUND.to_normalized_gamma_f32()
    }
}

pub(crate) fn run_size_dialog(paths: Vec<PathBuf>) -> anyhow::Result<()> {
    let t = crate::i18n::strings(load_settings().language);
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/app.png"))
        .map_err(|error| anyhow::anyhow!("{}", t.cannot_load_icon(&error)))?;
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([SIZE_DIALOG_SIZE.x, SIZE_DIALOG_SIZE.y])
        .with_min_inner_size([380.0, 240.0])
        .with_resizable(true)
        .with_icon(icon);
    if let Some(position) = crate::windows::centered_outer_position(SIZE_DIALOG_SIZE) {
        viewport = viewport.with_position(position);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        t.menu_size,
        options,
        Box::new(move |context| Ok(Box::new(SizeDialog::new(context, paths)))),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

struct RenameDialog {
    source_items: Vec<tools::RenameItem>,
    items: Vec<tools::RenameItem>,
    old_text: String,
    new_text: String,
    new_edit_epoch: u64,
    options: tools::RenameOptions,
    plans: Vec<RenamePlan>,
    status: String,
    status_ok: bool,
    close: bool,
}

impl RenameDialog {
    fn new(context: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        install_chinese_font(&context.egui_ctx);
        configure_style(&context.egui_ctx);
        let items: Vec<tools::RenameItem> = paths
            .into_iter()
            .map(|path| {
                let from = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_owned();
                tools::RenameItem { path, from }
            })
            .collect();
        let mut dialog = Self {
            old_text: items
                .iter()
                .map(|item| item.from.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            new_text: String::new(),
            new_edit_epoch: 0,
            source_items: items.clone(),
            items,
            options: tools::RenameOptions::default(),
            plans: Vec::new(),
            status: String::new(),
            status_ok: false,
            close: false,
        };
        dialog.apply_auto_pattern();
        dialog.rebuild_from_pattern();
        dialog
    }

    fn apply_auto_pattern(&mut self) {
        let names: Vec<String> = self.items.iter().map(|item| item.from.clone()).collect();
        let (old_pattern, new_pattern) =
            tools::common_rename_pattern(&names, self.options.ignore_extension);
        self.options.old_pattern = old_pattern;
        self.options.new_pattern = new_pattern;
    }

    fn paths_and_from(&self) -> (Vec<PathBuf>, Vec<String>) {
        let paths = self.items.iter().map(|item| item.path.clone()).collect();
        let from_names = self.items.iter().map(|item| item.from.clone()).collect();
        (paths, from_names)
    }

    fn rebuild_from_pattern(&mut self) {
        let (paths, from_names) = self.paths_and_from();
        self.plans = tools::plan_renames_from(&paths, &from_names, &self.options);
        let new_names: Vec<String> = self.plans.iter().map(|plan| plan.to.clone()).collect();
        self.new_text = tools::align_new_name_text(&self.old_text, &new_names);
        self.new_edit_epoch = self.new_edit_epoch.saturating_add(1);
    }

    fn rebuild_from_new_text(&mut self) {
        let (paths, from_names) = self.paths_and_from();
        let to_names = tools::name_list_from_text(&self.new_text, self.items.len(), &from_names);
        self.plans = tools::plan_renames_to(&paths, &from_names, &to_names);
        self.new_text = to_names.join("\n");
    }

    fn apply(&mut self, t: &crate::i18n::Strings) -> bool {
        self.rebuild_from_new_text();
        let ready: Vec<RenamePlan> = self
            .plans
            .iter()
            .filter(|plan| plan.kind == tools::RenameKind::Ready)
            .cloned()
            .collect();
        if ready.is_empty() {
            self.status = t.rename_none().to_owned();
            self.status_ok = false;
            return false;
        }
        let mut ok = 0usize;
        let mut last_error = String::new();
        for plan in ready {
            match tools::apply_rename(&plan) {
                Ok(()) => {
                    if let Some(item) = self
                        .items
                        .iter_mut()
                        .find(|item| item.path == plan.source)
                    {
                        item.path = plan.dest.clone();
                        item.from = plan.to.clone();
                    }
                    ok += 1;
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        self.old_text = self
            .items
            .iter()
            .map(|item| item.from.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.apply_auto_pattern();
        self.rebuild_from_pattern();
        if ok == 0 {
            self.status = t.rename_failed(&last_error);
            self.status_ok = false;
            false
        } else {
            self.status = t.rename_done(ok);
            self.status_ok = true;
            last_error.is_empty()
        }
    }
}

impl eframe::App for RenameDialog {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = crate::i18n::strings(load_settings().language);
        if self.close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
        let conflict_text = self
            .plans
            .iter()
            .find(|plan| {
                matches!(
                    plan.kind,
                    tools::RenameKind::Conflict | tools::RenameKind::Invalid
                )
            })
            .map(|plan| format!("{} ({})", plan.to, t.rename_status(plan.kind)));
        fill_background(ui, |ui| {
            ui.set_max_size(ui.available_size());
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    if ui.add(primary_button(t.ok)).clicked() && self.apply(t) {
                        self.close = true;
                    }
                    if ui.button(t.cancel).clicked() {
                        self.close = true;
                    }
                    if !self.status.is_empty() {
                        let color = if self.status_ok { ACCENT } else { DANGER };
                        ui.label(egui::RichText::new(&self.status).color(color));
                    } else if let Some(text) = &conflict_text {
                        ui.colored_label(DANGER, text);
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_max_height(ui.available_height());
                    let mut pattern_changed = false;
                    let mut ignore_ext_changed = false;
                    ui.colored_label(TEXT_SECONDARY, t.rename_old_list);
                    ui.colored_label(TEXT_SECONDARY, t.rename_old_list_hint);
                    let rest = ui.available_height();
                    const MIDDLE: f32 = 196.0;
                    let list_budget = (rest - MIDDLE).max(0.0);
                    let old_h = (list_budget * 0.5).clamp(0.0, 220.0);
                    let old_id = ui.id().with("rename_old_names");
                    names_editor(ui, old_h, &mut self.old_text, old_id);
                    let next_items = tools::reconcile_selection(
                        &self.source_items,
                        &self.items,
                        &self.old_text,
                    );
                    let new_names: Vec<String> =
                        self.plans.iter().map(|plan| plan.to.clone()).collect();
                    let aligned = tools::align_new_name_text(&self.old_text, &new_names);
                    if next_items != self.items {
                        self.items = next_items;
                        self.rebuild_from_pattern();
                        self.status.clear();
                    } else if tools::split_name_lines(&aligned)
                        != tools::split_name_lines(&self.new_text)
                    {
                        self.new_text = aligned;
                        self.new_edit_epoch = self.new_edit_epoch.saturating_add(1);
                        self.status.clear();
                    }
                    ui.add_space(8.0);
                    pattern_changed |=
                        expression_edit(ui, t.rename_old_expr, &mut self.options.old_pattern);
                    pattern_changed |=
                        expression_edit(ui, t.rename_new_expr, &mut self.options.new_pattern);
                    ui.horizontal(|ui| {
                        pattern_changed |= ui
                            .checkbox(&mut self.options.match_case, t.rename_match_case)
                            .changed();
                        pattern_changed |= ui
                            .checkbox(&mut self.options.use_regex, t.rename_regex)
                            .changed();
                        ignore_ext_changed = ui
                            .checkbox(&mut self.options.ignore_extension, t.rename_ignore_ext)
                            .changed();
                        pattern_changed |= ignore_ext_changed;
                    });
                    ui.colored_label(TEXT_SECONDARY, t.rename_number_hint);
                    if ignore_ext_changed {
                        self.apply_auto_pattern();
                    }
                    if pattern_changed {
                        self.rebuild_from_pattern();
                        self.status.clear();
                    }
                    ui.add_space(8.0);
                    ui.colored_label(TEXT_SECONDARY, t.rename_new_list);
                    let new_h = ui.available_height().max(1.0);
                    let new_id = ui.id().with(("rename_new_names", self.new_edit_epoch));
                    if names_editor(ui, new_h, &mut self.new_text, new_id) {
                        self.rebuild_from_new_text();
                        self.status.clear();
                    }
                });
            });
        });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BACKGROUND.to_normalized_gamma_f32()
    }
}

fn expression_edit(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    ui.colored_label(TEXT_SECONDARY, label);
    ui.add_sized(
        [ui.available_width(), 24.0],
        egui::TextEdit::singleline(value)
            .font(egui::TextStyle::Monospace)
            .background_color(SURFACE)
            .margin(egui::vec2(6.0, 4.0)),
    )
    .changed()
}

fn names_editor(ui: &mut egui::Ui, height: f32, text: &mut String, id: egui::Id) -> bool {
    ui.add_sized(
        [ui.available_width(), height],
        egui::TextEdit::multiline(text)
            .id(id)
            .font(egui::TextStyle::Monospace)
            .background_color(SURFACE)
            .margin(egui::vec2(6.0, 4.0)),
    )
    .changed()
}

pub(crate) fn run_rename_dialog(paths: Vec<PathBuf>) -> anyhow::Result<()> {
    let t = crate::i18n::strings(load_settings().language);
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/app.png"))
        .map_err(|error| anyhow::anyhow!("{}", t.cannot_load_icon(&error)))?;
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([RENAME_DIALOG_SIZE.x, RENAME_DIALOG_SIZE.y])
        .with_min_inner_size([RENAME_DIALOG_MIN_SIZE.x, RENAME_DIALOG_MIN_SIZE.y])
        .with_resizable(true)
        .with_icon(icon);
    if let Some(position) = crate::windows::centered_outer_position(RENAME_DIALOG_SIZE) {
        viewport = viewport.with_position(position);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        t.menu_rename,
        options,
        Box::new(move |context| Ok(Box::new(RenameDialog::new(context, paths)))),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn configure_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Light);
    let mut style = (*context.style_of(egui::Theme::Light)).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = SURFACE;
    style.visuals.faint_bg_color = BACKGROUND;
    style.visuals.extreme_bg_color = BACKGROUND;
    style.visuals.override_text_color = Some(TEXT_PRIMARY);
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    style.visuals.widgets.hovered.bg_fill = ACCENT_SOFT;
    style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.bg_fill = ACCENT;
    style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, SURFACE);
    let proportional = egui::FontFamily::Proportional;
    style.text_styles = [
        (egui::TextStyle::Small, egui::FontId::new(10.0, proportional.clone())),
        (egui::TextStyle::Body, egui::FontId::new(12.0, proportional.clone())),
        (egui::TextStyle::Button, egui::FontId::new(12.0, proportional.clone())),
        (egui::TextStyle::Heading, egui::FontId::new(13.0, proportional)),
        (egui::TextStyle::Monospace, egui::FontId::new(12.0, egui::FontFamily::Monospace)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size = egui::vec2(40.0, 23.0);
    style.spacing.icon_width = 14.0;
    style.spacing.icon_width_inner = 8.0;
    context.set_style_of(egui::Theme::Light, style);
}

fn fill_background(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(BACKGROUND)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add_contents(ui);
        });
}

fn show_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(6)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(title)
                    .size(13.0)
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            if !subtitle.is_empty() {
                ui.colored_label(TEXT_SECONDARY, subtitle);
            }
            ui.add_space(6.0);
            add_contents(ui);
        });
}

fn show_metric(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.colored_label(TEXT_SECONDARY, label);
    ui.label(
        egui::RichText::new(value)
            .size(13.0)
            .strong()
            .color(TEXT_PRIMARY),
    );
}

fn primary_button(text: &'static str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).strong().color(SURFACE))
        .fill(ACCENT)
        .stroke(egui::Stroke::new(1.0, ACCENT))
        .corner_radius(3)
}

fn show_error(strings: &crate::i18n::Strings, message: String) {
    let _ = rfd::MessageDialog::new()
        .set_title(strings.app_title)
        .set_description(&message)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

pub(crate) fn confirm_permanent_delete(strings: &crate::i18n::Strings, paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    matches!(
        rfd::MessageDialog::new()
            .set_title(strings.app_title)
            .set_description(strings.confirm_permanent_body(paths))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Warning)
            .show(),
        rfd::MessageDialogResult::Yes
    )
}

pub(crate) fn load_settings() -> Settings {
    fs::read(shell_menu::settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) -> std::io::Result<()> {
    let path = shell_menu::settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut to_save = settings.clone();
    to_save.ignore_file_name = crate::model::sanitize_ignore_file_name(&to_save.ignore_file_name);
    let bytes = serde_json::to_vec_pretty(&to_save).map_err(std::io::Error::other)?;
    fs::write(path, bytes)
}

fn install_chinese_font(context: &egui::Context) {
    let candidates = [
        PathBuf::from(r"C:\Windows\Fonts\msyh.ttc"),
        PathBuf::from(r"C:\Windows\Fonts\simhei.ttf"),
    ];
    if let Some(bytes) = candidates.iter().find_map(|path| fs::read(path).ok()) {
        let mut definitions = egui::FontDefinitions::default();
        definitions.font_data.insert(
            "windows-chinese".to_owned(),
            egui::FontData::from_owned(bytes).into(),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            definitions
                .families
                .entry(family)
                .or_default()
                .insert(0, "windows-chinese".to_owned());
        }
        context.set_fonts(definitions);
    }
}

fn progress_fraction(progress: &ProgressSnapshot) -> f32 {
    if progress.total_bytes > 0 {
        (progress.completed_bytes as f64 / progress.total_bytes as f64).clamp(0.0, 1.0) as f32
    } else if progress.total_items > 0 {
        (progress.completed_items as f64 / progress.total_items as f64).clamp(0.0, 1.0) as f32
    } else {
        0.0
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn format_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}
