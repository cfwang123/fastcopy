use crate::model::{OperationKind, Settings, TaskRequest};
use anyhow::{Context, Result, anyhow};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError, WAIT_FAILED};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHCNE_ASSOCCHANGED, SHCNE_UPDATEDIR, SHCNF_FLUSH,
    SHCNF_IDLIST, SHCNF_PATHW, SHChangeNotify, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE};

const APP_DIRECTORY: &str = "FastCopy";
const INSTANCE_LOCK: &str = "instance.lock";
const CLIPBOARD_FILE: &str = "clipboard.json";
const CLIPBOARD_LOCK: &str = "clipboard.lock";
const PENDING_FILE: &str = "pending.jsonl";
const PENDING_LOCK: &str = "pending.lock";
const HKCU_PASTE_VERB: &str = r"Software\Classes\Directory\Background\shell\FastCopyPaste";
const HKCU_CLEAR_VERB: &str = r"Software\Classes\Directory\Background\shell\FastCopyClear";
const CASCADE_CUT: &str = r"shell\1cut";
const CASCADE_COPY: &str = r"shell\2copy";
const CASCADE_DELETE: &str = r"shell\3delete";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardKind {
    Copy,
    Move,
}

#[derive(Debug, Serialize, Deserialize)]
struct ClipboardData {
    kind: ClipboardKind,
    paths: Vec<PathBuf>,
    updated_millis: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PendingCommand {
    Paste(PathBuf),
    Delete(PathBuf),
}

pub struct InstanceGuard {
    _file: File,
}

pub fn app_data_directory() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    base.join(APP_DIRECTORY)
}

pub fn try_acquire_instance() -> Result<Option<InstanceGuard>> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(INSTANCE_LOCK))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(InstanceGuard { _file: file })),
        Err(_) => Ok(None),
    }
}

pub fn update_clipboard(kind: ClipboardKind, paths: Vec<PathBuf>) -> Result<()> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let lock = open_lock(&directory.join(CLIPBOARD_LOCK))?;
    lock.lock_exclusive()?;

    let clipboard_path = directory.join(CLIPBOARD_FILE);
    let now = now_millis();
    let mut data = read_json::<ClipboardData>(&clipboard_path).unwrap_or(ClipboardData {
        kind,
        paths: Vec::new(),
        updated_millis: 0,
    });
    if data.kind != kind || now.saturating_sub(data.updated_millis) > 1500 {
        data.kind = kind;
        data.paths.clear();
    }
    for path in paths {
        if !data.paths.contains(&path) {
            data.paths.push(path);
        }
    }
    data.updated_millis = now;
    let bytes = serde_json::to_vec_pretty(&data)?;
    fs::write(clipboard_path, bytes)?;
    let should_show = !data.paths.is_empty();
    FileExt::unlock(&lock)?;
    sync_background_verbs(should_show);
    Ok(())
}

pub fn clear_clipboard(folder: Option<&Path>) -> Result<()> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let lock = open_lock(&directory.join(CLIPBOARD_LOCK))?;
    lock.lock_exclusive()?;
    let clipboard_path = directory.join(CLIPBOARD_FILE);
    if clipboard_path.exists() {
        fs::remove_file(&clipboard_path)?;
    }
    FileExt::unlock(&lock)?;
    let _ = hide_background_verbs();
    notify_shell(folder);
    Ok(())
}
pub fn clipboard_task(destination: PathBuf, settings: Settings) -> Result<TaskRequest> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let lock = open_lock(&directory.join(CLIPBOARD_LOCK))?;
    lock.lock_exclusive()?;
    let result = take_clipboard_locked(&directory, destination.clone(), settings);
    FileExt::unlock(&lock)?;
    if result.is_ok() {
        let _ = hide_background_verbs();
        notify_shell(Some(&destination));
    }
    result
}

fn take_clipboard_locked(
    directory: &Path,
    destination: PathBuf,
    settings: Settings,
) -> Result<TaskRequest> {
    let path = directory.join(CLIPBOARD_FILE);
    let t = ui_strings();
    let data = read_json::<ClipboardData>(&path).ok_or_else(|| anyhow!(t.clipboard_empty_hint))?;
    if data.paths.is_empty() {
        return Err(anyhow!(t.clipboard_empty));
    }
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(TaskRequest {
        kind: match data.kind {
            ClipboardKind::Copy => OperationKind::Copy,
            ClipboardKind::Move => OperationKind::Move,
        },
        sources: data.paths,
        destination: Some(destination),
        settings,
        retry_items: Vec::new(),
    })
}

pub fn append_pending(command: &PendingCommand) -> Result<()> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let lock = open_lock(&directory.join(PENDING_LOCK))?;
    lock.lock_exclusive()?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(PENDING_FILE))?;
    serde_json::to_writer(&mut file, command)?;
    file.write_all(b"\n")?;
    file.flush()?;
    FileExt::unlock(&lock)?;
    Ok(())
}

pub fn take_pending() -> Result<Vec<PendingCommand>> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let lock = open_lock(&directory.join(PENDING_LOCK))?;
    lock.lock_exclusive()?;
    let path = directory.join(PENDING_FILE);
    if !path.exists() {
        FileExt::unlock(&lock)?;
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    fs::write(&path, [])?;
    FileExt::unlock(&lock)?;
    Ok(content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

pub fn settings_path() -> PathBuf {
    app_data_directory().join("settings.json")
}

pub fn refresh_background_verbs() {
    if is_user_registered() && menu_needs_repair(HKEY_CURRENT_USER, r"Software\Classes") {
        let _ = repair_cascade_menu(HKEY_CURRENT_USER, r"Software\Classes");
    }
    if is_machine_registered() && menu_needs_repair(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes") {
        let _ = repair_cascade_menu(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes");
    }
    if clipboard_has_items() {
        if show_background_verbs().is_ok() {
            notify_assoc_changed();
        }
        return;
    }
    sync_background_verbs(false);
}

pub fn try_update_menu_labels() {
    let t = ui_strings();
    let mut updated = false;
    for (hive, classes) in [
        (HKEY_CURRENT_USER, r"Software\Classes"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes"),
    ] {
        for parent in [
            format!(r"{classes}\*\shell\FastCopyRust"),
            format!(r"{classes}\Directory\shell\FastCopyRust"),
        ] {
            updated |= set_verb_label(hive, &parent, t.menu_cascade).is_ok();
            updated |=
                set_verb_label(hive, &format!(r"{parent}\{CASCADE_CUT}"), t.menu_cut).is_ok();
            updated |=
                set_verb_label(hive, &format!(r"{parent}\{CASCADE_COPY}"), t.menu_copy).is_ok();
            updated |=
                set_verb_label(hive, &format!(r"{parent}\{CASCADE_DELETE}"), t.menu_delete).is_ok();
        }
    }
    if updated {
        notify_assoc_changed();
    }
}

pub fn is_user_registered() -> bool {
    hive_has(HKEY_CURRENT_USER, r"Software\Classes\*\shell\FastCopyRust")
}

pub fn is_machine_registered() -> bool {
    hive_has(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes\*\shell\FastCopyRust")
}

pub fn register() -> Result<()> {
    register_hive(HKEY_CURRENT_USER, r"Software\Classes")
}

fn register_hive(hive: winreg::HKEY, classes: &str) -> Result<()> {
    write_cascade_keys(hive, classes)?;
    let root = RegKey::predef(hive);
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyCut"))?;
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyCopy"))?;
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyDelete"))?;
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyPaste"))?;
    delete_if_exists(
        &root,
        &format!(r"{classes}\Directory\Background\shell\FastCopyRust"),
    )?;
    delete_if_exists(
        &root,
        &format!(r"{classes}\Directory\Background\shell\FastCopyPaste"),
    )?;
    delete_if_exists(
        &root,
        &format!(r"{classes}\Directory\Background\shell\FastCopyClear"),
    )?;
    sync_background_verbs(clipboard_has_items());
    notify_assoc_changed();
    Ok(())
}

fn write_cascade_keys(hive: winreg::HKEY, classes: &str) -> Result<()> {
    let t = ui_strings();
    let executable = env::current_exe().context(t.cannot_get_exe_path())?;
    let executable = executable.to_string_lossy();
    let icons = install_menu_icons()?;
    let root = RegKey::predef(hive);
    create_cascade(
        &root,
        &format!(r"{classes}\*\shell\FastCopyRust"),
        &executable,
        &icons,
    )?;
    create_cascade(
        &root,
        &format!(r"{classes}\Directory\shell\FastCopyRust"),
        &executable,
        &icons,
    )?;
    Ok(())
}

fn delete_cascade_keys(hive: winreg::HKEY, classes: &str) -> Result<()> {
    let root = RegKey::predef(hive);
    for rel in [
        r"*\shell\FastCopyRust",
        r"Directory\shell\FastCopyRust",
        r"Directory\shell\FastCopyCut",
        r"Directory\shell\FastCopyCopy",
        r"Directory\shell\FastCopyDelete",
        r"Directory\shell\FastCopyPaste",
        r"Directory\Background\shell\FastCopyRust",
        r"Directory\Background\shell\FastCopyPaste",
        r"Directory\Background\shell\FastCopyClear",
    ] {
        delete_if_exists(&root, &format!(r"{classes}\{rel}"))?;
    }
    Ok(())
}

pub fn unregister() -> Result<()> {
    unregister_user()?;
    let _ = unregister_machine();
    notify_assoc_changed();
    Ok(())
}

pub fn unregister_user() -> Result<()> {
    delete_cascade_keys(HKEY_CURRENT_USER, r"Software\Classes")?;
    let user = RegKey::predef(HKEY_CURRENT_USER);
    delete_if_exists(&user, HKCU_PASTE_VERB)?;
    delete_if_exists(&user, HKCU_CLEAR_VERB)?;
    delete_command_store(
        &user,
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\CommandStore\shell",
    )?;
    notify_assoc_changed();
    Ok(())
}

pub fn unregister_machine() -> Result<()> {
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    for path in [
        r"SOFTWARE\Classes\*\shell\FastCopyRust",
        r"SOFTWARE\Classes\Directory\shell\FastCopyRust",
        r"SOFTWARE\Classes\Directory\shell\FastCopyCut",
        r"SOFTWARE\Classes\Directory\shell\FastCopyCopy",
        r"SOFTWARE\Classes\Directory\shell\FastCopyDelete",
        r"SOFTWARE\Classes\Directory\shell\FastCopyPaste",
        r"SOFTWARE\Classes\Directory\Background\shell\FastCopyRust",
        r"SOFTWARE\Classes\Directory\Background\shell\FastCopyPaste",
        r"SOFTWARE\Classes\Directory\Background\shell\FastCopyClear",
    ] {
        delete_if_exists(&root, path)?;
    }
    delete_command_store(
        &root,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\CommandStore\shell",
    )?;
    Ok(())
}

fn delete_command_store(root: &RegKey, store: &str) -> Result<()> {
    let Ok(command_store) = root.open_subkey_with_flags(store, winreg::enums::KEY_ALL_ACCESS)
    else {
        return Ok(());
    };
    for name in [
        "FastCopy.Cut",
        "FastCopy.Copy",
        "FastCopy.Paste",
        "FastCopy.PasteBackground",
        "FastCopy.Delete",
    ] {
        match command_store.delete_subkey_all(name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn hive_has(hive: winreg::HKEY, path: &str) -> bool {
    RegKey::predef(hive)
        .open_subkey_with_flags(path, KEY_READ)
        .is_ok()
}

pub fn elevate(argument: &str) -> Result<()> {
    let executable = env::current_exe()?;
    let executable = wide(executable.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let parameters = wide(OsStr::new(argument));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: executable.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_CANCELLED {
            return Err(anyhow!("{}", ui_strings().uac_cancelled()));
        }
        return Err(anyhow!("{}", ui_strings().uac_start_failed(code)));
    }
    if !info.hProcess.is_null() {
        unsafe {
            if WaitForSingleObject(info.hProcess, INFINITE) == WAIT_FAILED {
                let code = GetLastError();
                CloseHandle(info.hProcess);
                return Err(anyhow!("{}", ui_strings().uac_wait_failed(code)));
            }
            CloseHandle(info.hProcess);
        }
    }
    Ok(())
}

fn clipboard_has_items() -> bool {
    read_json::<ClipboardData>(&app_data_directory().join(CLIPBOARD_FILE))
        .is_some_and(|data| !data.paths.is_empty())
}

fn background_verbs_visible() -> bool {
    if user_verb_disabled(HKCU_PASTE_VERB) {
        return false;
    }
    machine_background_verbs_exist() || user_has_paste_command()
}

fn machine_background_verbs_exist() -> bool {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Classes\Directory\Background\shell\FastCopyPaste")
        .is_ok()
}

fn user_has_paste_command() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(format!(r"{HKCU_PASTE_VERB}\command"))
        .is_ok()
}

fn user_verb_disabled(path: &str) -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(path)
        .ok()
        .and_then(|key| key.get_value::<String, _>("LegacyDisable").ok())
        .is_some()
}

fn sync_background_verbs(should_show: bool) {
    if should_show == background_verbs_visible() {
        return;
    }
    let result = if should_show {
        show_background_verbs()
    } else {
        hide_background_verbs()
    };
    if result.is_ok() {
        notify_assoc_changed();
    }
}

fn show_background_verbs() -> Result<()> {
    if machine_background_verbs_exist() {
        let user = RegKey::predef(HKEY_CURRENT_USER);
        delete_if_exists(&user, HKCU_PASTE_VERB)?;
        delete_if_exists(&user, HKCU_CLEAR_VERB)?;
        return Ok(());
    }
    let executable = env::current_exe()?.to_string_lossy().into_owned();
    let icons = install_menu_icons()?;
    let user = RegKey::predef(HKEY_CURRENT_USER);
    let t = ui_strings();
    upsert_user_verb(
        &user,
        HKCU_PASTE_VERB,
        t.menu_paste,
        &format!("\"{executable}\" --shell-paste \"%V\""),
        &icons.join("paste.ico"),
    )?;
    upsert_user_verb(
        &user,
        HKCU_CLEAR_VERB,
        t.menu_clear,
        &format!("\"{executable}\" --shell-clear-clipboard \"%V\""),
        &icons.join("cut.ico"),
    )?;
    Ok(())
}

fn hide_background_verbs() -> Result<()> {
    disable_user_verb(HKCU_PASTE_VERB)?;
    disable_user_verb(HKCU_CLEAR_VERB)?;
    Ok(())
}

fn disable_user_verb(path: &str) -> Result<()> {
    let user = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = user.create_subkey(path)?;
    key.set_value("LegacyDisable", &"")?;
    key.set_value("ProgrammaticAccessOnly", &"")?;
    key.set_value("AppliesTo", &"System.Kind:file")?;
    Ok(())
}

fn notify_assoc_changed() {
    notify_shell(None);
}

fn notify_shell(folder: Option<&Path>) {
    if let Some(folder) = folder {
        let path = wide(folder.as_os_str());
        unsafe {
            SHChangeNotify(
                SHCNE_UPDATEDIR as i32,
                SHCNF_PATHW | SHCNF_FLUSH,
                path.as_ptr().cast(),
                std::ptr::null(),
            );
        }
    }
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST | SHCNF_FLUSH,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

fn create_cascade(root: &RegKey, parent: &str, executable: &str, icons: &Path) -> Result<()> {
    let t = ui_strings();
    let (key, _) = root.create_subkey(parent)?;
    key.set_value("MUIVerb", &t.menu_cascade)?;
    key.set_value("SubCommands", &"")?;
    key.set_value("MultiSelectModel", &"Single")?;
    key.set_value("Icon", &icon_value(&icons.join("app.ico")))?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_CUT}"),
        t.menu_cut,
        &format!("\"{executable}\" --shell-cut \"%1\""),
        &icons.join("cut.ico"),
    )?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_COPY}"),
        t.menu_copy,
        &format!("\"{executable}\" --shell-copy \"%1\""),
        &icons.join("copy.ico"),
    )?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_DELETE}"),
        t.menu_delete,
        &format!("\"{executable}\" --shell-delete \"%1\""),
        &icons.join("delete.ico"),
    )?;
    Ok(())
}

fn menu_needs_repair(hive: winreg::HKEY, classes: &str) -> bool {
    hive_has(hive, &format!(r"{classes}\Directory\shell\FastCopyCopy"))
        || hive_has(hive, &format!(r"{classes}\Directory\shell\FastCopyCut"))
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_COPY}"),
        )
        || !verb_is_single(hive, &format!(r"{classes}\Directory\shell\FastCopyRust"))
        || !verb_is_single(hive, &format!(r"{classes}\*\shell\FastCopyRust"))
}

fn verb_is_single(hive: winreg::HKEY, path: &str) -> bool {
    let Ok(key) = RegKey::predef(hive).open_subkey(path) else {
        return true;
    };
    let model: String = key.get_value("MultiSelectModel").unwrap_or_default();
    model == "Single"
}

fn repair_cascade_menu(hive: winreg::HKEY, classes: &str) -> Result<()> {
    register_hive(hive, classes)
}

fn set_verb_label(hive: winreg::HKEY, path: &str, label: &str) -> Result<()> {
    let key = RegKey::predef(hive).open_subkey_with_flags(path, KEY_SET_VALUE)?;
    key.set_value("MUIVerb", &label)?;
    Ok(())
}

fn ui_strings() -> &'static crate::i18n::Strings {
    crate::i18n::strings(
        read_json::<Settings>(&settings_path())
            .map(|settings| settings.language)
            .unwrap_or_default(),
    )
}

fn upsert_user_verb(
    root: &RegKey,
    path: &str,
    label: &str,
    command: &str,
    icon: &Path,
) -> Result<()> {
    let (key, _) = root.create_subkey(path)?;
    key.set_value("MUIVerb", &label)?;
    key.set_value("Icon", &icon_value(icon))?;
    key.set_value("MultiSelectModel", &"Single")?;
    delete_value_if_exists(&key, "LegacyDisable")?;
    delete_value_if_exists(&key, "ProgrammaticAccessOnly")?;
    delete_value_if_exists(&key, "AppliesTo")?;
    let (command_key, _) = key.create_subkey("command")?;
    command_key.set_value("", &command)?;
    Ok(())
}

fn delete_value_if_exists(key: &RegKey, name: &str) -> Result<()> {
    match key.delete_value(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn install_menu_icons() -> Result<PathBuf> {
    let directory = app_data_directory().join("icons");
    fs::create_dir_all(&directory)?;
    for (name, bytes) in [
        (
            "app.ico",
            include_bytes!("../../assets/icons/app.ico").as_slice(),
        ),
        (
            "copy.ico",
            include_bytes!("../../assets/icons/copy.ico").as_slice(),
        ),
        (
            "cut.ico",
            include_bytes!("../../assets/icons/cut.ico").as_slice(),
        ),
        (
            "paste.ico",
            include_bytes!("../../assets/icons/paste.ico").as_slice(),
        ),
        (
            "delete.ico",
            include_bytes!("../../assets/icons/delete.ico").as_slice(),
        ),
    ] {
        fs::write(directory.join(name), bytes)?;
    }
    Ok(directory)
}

fn icon_value(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn delete_if_exists(root: &RegKey, path: &str) -> Result<()> {
    match root.delete_subkey_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn open_lock(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let mut file = File::open(path).ok()?;
    let mut content = Vec::new();
    file.read_to_end(&mut content).ok()?;
    serde_json::from_slice(&content).ok()
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use winreg::enums::HKEY_CURRENT_USER;

    const TEST_ROOT: &str = r"Software\FastCopyRustMenuTest";
    const TEST_CLASSES: &str = r"Software\FastCopyRustMenuTest\Classes";

    #[test]
    fn register_and_unregister_test_classes() {
        let _ = delete_if_exists(&RegKey::predef(HKEY_CURRENT_USER), TEST_ROOT);
        write_cascade_keys(HKEY_CURRENT_USER, TEST_CLASSES).unwrap();
        let file_key = format!(r"{TEST_CLASSES}\*\shell\FastCopyRust");
        let dir_key = format!(r"{TEST_CLASSES}\Directory\shell\FastCopyRust");
        assert!(hive_has(HKEY_CURRENT_USER, &file_key));
        assert!(hive_has(HKEY_CURRENT_USER, &dir_key));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{dir_key}\{CASCADE_COPY}")
        ));
        let model: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&dir_key)
            .unwrap()
            .get_value("MultiSelectModel")
            .unwrap();
        assert_eq!(model, "Single");
        delete_cascade_keys(HKEY_CURRENT_USER, TEST_CLASSES).unwrap();
        assert!(!hive_has(HKEY_CURRENT_USER, &file_key));
        assert!(!hive_has(HKEY_CURRENT_USER, &dir_key));
        let _ = delete_if_exists(&RegKey::predef(HKEY_CURRENT_USER), TEST_ROOT);
    }
}
