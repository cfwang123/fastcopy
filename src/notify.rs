use crate::i18n::Strings;
use crate::model::OperationKind;
use crate::windows::shell_menu;
use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Once;
use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const APP_ID: &str = "FastCopy.App";
static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(|| {
        let _ = set_process_aumid();
        let _ = register_aumid();
    });
}

pub fn finished(strings: &Strings, kind: OperationKind, cancelled: bool, errors: usize) {
    if cancelled {
        return;
    }
    init();
    let body = if errors == 0 {
        strings.notify_done(kind)
    } else {
        strings.notify_done_errors(kind, errors)
    };
    let _ = winrt_notification::Toast::new(APP_ID)
        .title(strings.app_title)
        .text1(&body)
        .show();
}

fn set_process_aumid() -> Result<(), ()> {
    let wide: Vec<u16> = OsStr::new(APP_ID)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    if hr == 0 { Ok(()) } else { Err(()) }
}

fn register_aumid() -> std::io::Result<()> {
    let icon = ensure_app_icon()?;
    let display = crate::app::load_settings().language.strings().app_title;
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = root.create_subkey(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))?;
    key.set_value("DisplayName", &display)?;
    key.set_value("IconUri", &icon.to_string_lossy().into_owned())?;
    Ok(())
}

fn ensure_app_icon() -> std::io::Result<PathBuf> {
    let directory = shell_menu::app_data_directory().join("icons");
    fs::create_dir_all(&directory)?;
    let path = directory.join("app.ico");
    fs::write(&path, include_bytes!("../assets/icons/app.ico"))?;
    Ok(path)
}
