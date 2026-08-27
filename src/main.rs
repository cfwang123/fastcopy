#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
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
    notify::init();
    let is_headless_cli = env::args().nth(1).is_some_and(|argument| {
        argument == "--copy"
            || argument == "--move"
            || argument == "--delete"
            || argument == "--dump-background-menu"
    });
    let t = strings(app::load_settings().language);
    match run() {
        Ok(code) => {
            if code != 0 {
                std::process::exit(code);
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            eprintln!("{message}");
            if !is_headless_cli {
                let _ = rfd::MessageDialog::new()
                    .set_title(t.app_title)
                    .set_description(&message)
                    .set_level(rfd::MessageLevel::Error)
                    .show();
            }
            std::process::exit(cli::EXIT_USAGE);
        }
    }
}

fn run() -> Result<i32> {
    let arguments: Vec<String> = env::args().collect();
    let t = strings(app::load_settings().language);
    match arguments.get(1).map(String::as_str) {
        Some("--copy") | Some("--move") => {
            let kind = if arguments[1] == "--move" {
                OperationKind::Move
            } else {
                OperationKind::Copy
            };
            let (options, paths) = cli::parse_options(&arguments[2..], t)?;
            if paths.is_empty() {
                return Err(anyhow!("{}", t.missing_cli_source()));
            }
            if paths.len() < 2 {
                return Err(anyhow!("{}", t.missing_cli_destination()));
            }
            let destination = paths[paths.len() - 1].clone();
            let sources = paths[..paths.len() - 1].to_vec();
            let mut settings = app::load_settings();
            cli::apply_options(&mut settings, &options);
            return run_headless(kind, sources, Some(destination), settings, t);
        }
        Some("--delete") => {
            let (options, paths) = cli::parse_options(&arguments[2..], t)?;
            if paths.is_empty() {
                return Err(anyhow!("{}", t.missing_cli_delete_path()));
            }
            let mut settings = app::load_settings();
            cli::apply_options(&mut settings, &options);
            if settings.delete_mode == DeleteMode::Permanent
                && !options.yes
                && !app::confirm_permanent_delete(t, &paths)
            {
                return Ok(cli::EXIT_CANCELLED);
            }
            return run_headless(OperationKind::Delete, paths, None, settings, t);
        }
        Some("--register-shell") => {
            shell_menu::register()?;
            return Ok(0);
        }
        Some("--dump-background-menu") => {
            let folder = argument_path(&arguments, t)?;
            for label in windows::explorer_sel::background_menu_labels(&folder)
                .map_err(|error| anyhow!("{error}"))?
            {
                println!("{label}");
            }
            return Ok(0);
        }
        Some("--unregister-shell") => {
            shell_menu::unregister()?;
            return Ok(0);
        }
        Some("--shell-copy") | Some("--shell-cut") => {
            let path = argument_path(&arguments, t)?;
            let kind = if arguments[1] == "--shell-copy" {
                ClipboardKind::Copy
            } else {
                ClipboardKind::Move
            };
            shell_menu::update_clipboard(kind, windows::explorer_sel::selected_paths(&path))?;
            return Ok(0);
        }
        Some("--shell-copy-symlink") | Some("--shell-copy-hardlink") => {
            let path = argument_path(&arguments, t)?;
            let kind = if arguments[1] == "--shell-copy-symlink" {
                ClipboardKind::CopySymlink
            } else {
                ClipboardKind::CopyHardlink
            };
            shell_menu::update_clipboard(kind, windows::explorer_sel::selected_paths(&path))?;
            return Ok(0);
        }
        Some("--shell-clear-clipboard") => {
            let folder = arguments.get(2).map(PathBuf::from);
            shell_menu::clear_clipboard(folder.as_deref())?;
            return Ok(0);
        }
        Some("--shell-paste") => {
            let destination = argument_path(&arguments, t)?;
            if shell_menu::clipboard_is_link_copy() {
                let settings = app::load_settings();
                let request = shell_menu::clipboard_task(destination, settings)?;
                return run_headless(
                    request.kind,
                    request.sources,
                    request.destination,
                    request.settings,
                    t,
                );
            }
            shell_menu::append_pending(&PendingCommand::Paste(destination))?;
        }
        Some("--shell-delete") => {
            let path = argument_path(&arguments, t)?;
            for selected in windows::explorer_sel::selected_paths(&path) {
                shell_menu::append_pending(&PendingCommand::Delete(selected))?;
            }
        }
        Some("--settings") => {}
        Some("--shell-open-target") => {
            let path = argument_path(&arguments, t)?;
            let Some(target) = windows::explorer_sel::resolve_link_target(&path) else {
                return Ok(0);
            };
            if !target.exists() {
                return Err(anyhow!("{}", t.link_target_missing(&target)));
            }
            windows::explorer_sel::reveal_path(&target)
                .map_err(|error| anyhow!("{}", t.cannot_open_target(&error)))?;
            return Ok(0);
        }
        Some("--shell-show-source") => {
            let path = argument_path(&arguments, t)?;
            app::run_source_path_dialog(path)?;
            return Ok(0);
        }
        Some(argument) => return Err(anyhow!("{}", t.unknown_cli_argument(argument))),
        None => {}
    }

    let Some(instance_guard) = shell_menu::try_acquire_instance()? else {
        return Ok(0);
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
        &t.window_title(),
        options,
        Box::new(move |context| Ok(Box::new(FastCopyApp::new(context, instance_guard)))),
    )
    .map_err(|error| anyhow!(error.to_string()))?;
    Ok(0)
}

fn run_headless(
    kind: OperationKind,
    sources: Vec<PathBuf>,
    destination: Option<PathBuf>,
    settings: Settings,
    t: &crate::i18n::Strings,
) -> Result<i32> {
    let notify = settings.notify_when_done(kind);
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
        notify,
    )
}

fn wait_for_engine(
    handle: EngineHandle,
    t: &crate::i18n::Strings,
    kind: OperationKind,
    notify: bool,
) -> Result<i32> {
    loop {
        match handle.recv() {
            Some(EngineEvent::Finished {
                cancelled,
                error_count,
                skip_count,
            }) => {
                if notify {
                    crate::notify::finished(t, kind, cancelled, error_count);
                }
                let code = cli::exit_code(cancelled, error_count, skip_count);
                if cancelled {
                    eprintln!("{}", t.cancelled());
                } else if error_count > 0 {
                    eprintln!("{}", t.finished_with_errors(error_count));
                } else if skip_count > 0 {
                    eprintln!("{}", t.finished_with_skips(skip_count));
                }
                return Ok(code);
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
