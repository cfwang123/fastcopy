#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod engine;
mod i18n;
mod model;
mod notify;
mod windows;

use anyhow::{Result, anyhow};
use app::{FastCopyApp, SETTINGS_MIN_SIZE, SETTINGS_SIZE};
use engine::{EngineEvent, EngineHandle};
use i18n::strings;
use model::{DeleteMode, OperationKind, Settings, TaskRequest};
use std::env;
use std::path::PathBuf;
use windows::shell_menu::{self, ClipboardKind, PendingCommand};

fn main() {
    let is_headless_cli = env::args().nth(1).is_some_and(|argument| {
        argument == "--copy" || argument == "--move" || argument == "--delete"
    });
    let t = strings(app::load_settings().language);
    if let Err(error) = run() {
        let message = format!("{error:#}");
        eprintln!("{message}");
        if !is_headless_cli {
            let _ = rfd::MessageDialog::new()
                .set_title(t.app_title)
                .set_description(&message)
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let arguments: Vec<String> = env::args().collect();
    let t = strings(app::load_settings().language);
    match arguments.get(1).map(String::as_str) {
        Some("--copy") | Some("--move") => {
            let kind = if arguments[1] == "--move" {
                OperationKind::Move
            } else {
                OperationKind::Copy
            };
            let source = arguments
                .get(2)
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("{}", t.missing_cli_source()))?;
            let destination = arguments
                .get(3)
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("{}", t.missing_cli_destination()))?;
            let mut settings = app::load_settings();
            apply_worker_override(&arguments, &mut settings, t)?;
            run_headless(kind, vec![source], Some(destination), settings, t)?;
            return Ok(());
        }
        Some("--delete") => {
            let path = arguments
                .get(2)
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("{}", t.missing_cli_delete_path()))?;
            let mut settings = app::load_settings();
            if arguments.iter().any(|argument| argument == "--permanent") {
                settings.delete_mode = DeleteMode::Permanent;
            } else if arguments.iter().any(|argument| argument == "--recycle-bin") {
                settings.delete_mode = DeleteMode::RecycleBin;
            }
            apply_worker_override(&arguments, &mut settings, t)?;
            if settings.delete_mode == DeleteMode::Permanent
                && !arguments.iter().any(|argument| argument == "--yes")
                && !app::confirm_permanent_delete(t, std::slice::from_ref(&path))
            {
                return Err(anyhow!("{}", t.cancelled()));
            }
            run_headless(OperationKind::Delete, vec![path], None, settings, t)?;
            return Ok(());
        }
        Some("--register-shell") => {
            shell_menu::register()?;
            return Ok(());
        }
        Some("--unregister-shell") => {
            shell_menu::unregister()?;
            return Ok(());
        }
        Some("--shell-copy") | Some("--shell-cut") => {
            let path = argument_path(&arguments, t)?;
            let kind = if arguments[1] == "--shell-copy" {
                ClipboardKind::Copy
            } else {
                ClipboardKind::Move
            };
            shell_menu::update_clipboard(kind, windows::explorer_sel::selected_paths(&path))?;
            return Ok(());
        }
        Some("--shell-clear-clipboard") => {
            let folder = arguments.get(2).map(PathBuf::from);
            shell_menu::clear_clipboard(folder.as_deref())?;
            return Ok(());
        }
        Some("--shell-paste") => {
            shell_menu::append_pending(&PendingCommand::Paste(argument_path(&arguments, t)?))?;
        }
        Some("--shell-delete") => {
            let path = argument_path(&arguments, t)?;
            for selected in windows::explorer_sel::selected_paths(&path) {
                shell_menu::append_pending(&PendingCommand::Delete(selected))?;
            }
        }
        Some(argument) => return Err(anyhow!("{}", t.unknown_cli_argument(argument))),
        None => {}
    }

    let Some(instance_guard) = shell_menu::try_acquire_instance()? else {
        return Ok(());
    };
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/app.png"))
        .map_err(|error| anyhow!("{}", t.cannot_load_icon(&error)))?;
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([SETTINGS_SIZE.x, SETTINGS_SIZE.y])
        .with_min_inner_size([SETTINGS_MIN_SIZE.x, SETTINGS_MIN_SIZE.y])
        .with_resizable(true)
        .with_icon(icon);
    if let Some(position) = windows::centered_outer_position(SETTINGS_SIZE) {
        viewport = viewport.with_position(position);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        t.app_title,
        options,
        Box::new(move |context| Ok(Box::new(FastCopyApp::new(context, instance_guard)))),
    )
    .map_err(|error| anyhow!(error.to_string()))
}

fn apply_worker_override(
    arguments: &[String],
    settings: &mut Settings,
    t: &crate::i18n::Strings,
) -> Result<()> {
    let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--workers")
    else {
        return Ok(());
    };
    let raw = arguments
        .get(index + 1)
        .ok_or_else(|| anyhow!("{}", t.workers_missing_value()))?;
    let count = raw
        .parse::<usize>()
        .map_err(|_| anyhow!("{}", t.workers_not_integer()))?;
    if count == 0 {
        return Err(anyhow!("{}", t.workers_not_positive()));
    }
    settings.worker_count = count.clamp(1, 64);
    Ok(())
}

fn run_headless(
    kind: OperationKind,
    sources: Vec<PathBuf>,
    destination: Option<PathBuf>,
    settings: Settings,
    t: &crate::i18n::Strings,
) -> Result<()> {
    wait_for_engine(
        engine::start(TaskRequest {
            kind,
            sources,
            destination,
            settings,
            retry_items: Vec::new(),
        }),
        t,
        kind,
    )
}

fn wait_for_engine(
    handle: EngineHandle,
    t: &crate::i18n::Strings,
    kind: OperationKind,
) -> Result<()> {
    loop {
        match handle.recv() {
            Some(EngineEvent::Finished {
                cancelled,
                error_count,
            }) => {
                crate::notify::finished(t, kind, cancelled, error_count);
                if cancelled {
                    return Err(anyhow!("{}", t.cancelled()));
                }
                if error_count > 0 {
                    return Err(anyhow!("{}", t.finished_with_errors(error_count)));
                }
                return Ok(());
            }
            Some(EngineEvent::Error(error) | EngineEvent::Failed { message: error, .. }) => {
                eprintln!("{error}");
            }
            Some(_) => {}
            None => return Err(anyhow!("{}", t.engine_stopped())),
        }
    }
}

fn argument_path(arguments: &[String], t: &crate::i18n::Strings) -> Result<PathBuf> {
    arguments
        .get(2)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("{}", t.missing_cli_path()))
}
