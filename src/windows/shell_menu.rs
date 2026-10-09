use crate::model::{OperationKind, RetryItem, Settings, TaskRequest};
use crate::windows::explorer_sel;
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
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError, WAIT_FAILED};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHCNE_ASSOCCHANGED, SHCNE_UPDATEDIR, SHCNF_FLUSH,
    SHCNF_IDLIST, SHCNF_PATHW, SHChangeNotify, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
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
const CASCADE_SYMLINK: &str = r"shell\4symlink";
const CASCADE_HARDLINK: &str = r"shell\5hardlink";
const CASCADE_OPEN_TARGET: &str = r"shell\6open";
const CASCADE_SHOW_SOURCE: &str = r"shell\6path";
const CASCADE_SIZE: &str = r"shell\7size";
const CASCADE_COPY_PATHS: &str = r"shell\8copypath";
const CASCADE_SETTINGS: &str = r"shell\zsettings";
const LEGACY_CASCADE_RENAME: &str = r"shell\9rename";
const LEGACY_RENAME_VERB: &str = r"Directory\Background\shell\FastCopyRename";
const SHELL_CLSID: &str = "{B3E8D47A-6C1F-4A92-9E05-8F4C2B17A6D0}";
const SHELL_DLL_NAME: &str = "fastcopy_shell.dll";
const SHELL_HANDLER: &str = "FastCopyShell";
/// One progid, so Explorer invokes the submenu once.
const COM_REGISTER_PROGIDS: &[&str] = &["AllFilesystemObjects"];
/// Every progid that has carried a FastCopy handler. Unregister deletes all of them.
const COM_CLEANUP_PROGIDS: &[&str] = &[
    "*",
    "Directory",
    "Folder",
    "AllFilesystemObjects",
    "Drive",
    "LibraryFolder",
    r"SystemFileAssociations\video",
    r"SystemFileAssociations\audio",
];
/// Registry cascades that draw a second top-level 快速复制. Kept out of Explorer.
const VISIBLE_CASCADES: &[&str] = &[
    r"*\shell\FastCopyRust",
    r"Directory\shell\FastCopyRust",
    r"Directory\shell\FastCopyCut",
    r"Directory\shell\FastCopyCopy",
    r"Directory\shell\FastCopyDelete",
    r"Directory\shell\FastCopyPaste",
    r"Directory\Background\shell\FastCopyRust",
    r"Drive\shell\FastCopyRust",
    r"Folder\shell\FastCopyRust",
    r"AllFilesystemObjects\shell\FastCopyRust",
    r"LibraryFolder\shell\FastCopyRust",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardKind {
    Copy,
    Move,
    CopySymlink,
    CopyHardlink,
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
    PasteKeep(PathBuf),
    Delete(PathBuf),
}

pub struct InstanceGuard {
    _file: File,
}

pub struct SelectionClaim {
    _lock: File,
    kind: &'static str,
    pub paths: Vec<PathBuf>,
}

impl Drop for SelectionClaim {
    fn drop(&mut self) {
        let directory = app_data_directory();
        let _ = fs::remove_file(directory.join(format!("{}.json", self.kind)));
    }
}

#[derive(Serialize, Deserialize)]
struct SelectionBatch {
    paths: Vec<PathBuf>,
    updated_millis: u128,
}

pub fn claim_selection(kind: &'static str, paths: Vec<PathBuf>) -> Result<Option<SelectionClaim>> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    merge_selection_paths(kind, paths)?;
    let lock = open_lock(&directory.join(format!("{kind}.lock")))?;
    match lock.try_lock_exclusive() {
        Ok(()) => {
            thread::sleep(Duration::from_millis(400));
            let batch = merge_selection_paths(kind, Vec::new())?;
            Ok(Some(SelectionClaim {
                _lock: lock,
                kind,
                paths: batch.paths,
            }))
        }
        Err(_) => Ok(None),
    }
}

fn merge_selection_paths(kind: &str, paths: Vec<PathBuf>) -> Result<SelectionBatch> {
    let directory = app_data_directory();
    let data_lock = open_lock(&directory.join(format!("{kind}-data.lock")))?;
    data_lock.lock_exclusive()?;
    let json_path = directory.join(format!("{kind}.json"));
    let now = now_millis();
    let mut batch = read_json::<SelectionBatch>(&json_path).unwrap_or(SelectionBatch {
        paths: Vec::new(),
        updated_millis: 0,
    });
    if now.saturating_sub(batch.updated_millis) > 2000 {
        batch.paths.clear();
    }
    for path in paths {
        if batch
            .paths
            .iter()
            .any(|existing| explorer_sel::same_path(existing, &path))
        {
            continue;
        }
        batch.paths.push(path);
    }
    batch.updated_millis = now;
    fs::write(&json_path, serde_json::to_vec(&batch)?)?;
    FileExt::unlock(&data_lock)?;
    Ok(batch)
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
        if data
            .paths
            .iter()
            .any(|existing| explorer_sel::same_path(existing, &path))
        {
            continue;
        }
        data.paths.push(path);
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
pub fn shift_key_down() -> bool {
    unsafe { GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000 != 0 }
}

pub fn clipboard_task(destination: PathBuf, settings: Settings, keep: bool) -> Result<TaskRequest> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let lock = open_lock(&directory.join(CLIPBOARD_LOCK))?;
    lock.lock_exclusive()?;
    let result = take_clipboard_locked(&directory, destination.clone(), settings, keep);
    FileExt::unlock(&lock)?;
    if let Ok(request) = &result {
        let kept = keep && request.kind != OperationKind::Move;
        if !kept {
            let _ = hide_background_verbs();
            notify_shell(Some(&destination));
        }
    }
    result
}

pub fn clipboard_is_link_copy() -> bool {
    matches!(
        clipboard_snapshot().map(|data| data.kind),
        Some(ClipboardKind::CopySymlink | ClipboardKind::CopyHardlink)
    )
}

fn take_clipboard_locked(
    directory: &Path,
    destination: PathBuf,
    settings: Settings,
    keep: bool,
) -> Result<TaskRequest> {
    let path = directory.join(CLIPBOARD_FILE);
    let t = ui_strings();
    let data = read_json::<ClipboardData>(&path).ok_or_else(|| anyhow!(t.clipboard_empty_hint))?;
    if data.paths.is_empty() {
        return Err(anyhow!(t.clipboard_empty));
    }
    let keep = keep && data.kind != ClipboardKind::Move;
    if !keep && path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(TaskRequest {
        kind: match data.kind {
            ClipboardKind::Copy => OperationKind::Copy,
            ClipboardKind::Move => OperationKind::Move,
            ClipboardKind::CopySymlink => OperationKind::CopyAsSymlink,
            ClipboardKind::CopyHardlink => OperationKind::CopyAsHardlink,
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
    let icons_changed = rewrite_menu_icons_if_changed();
    if is_user_registered() && menu_needs_repair(HKEY_CURRENT_USER, r"Software\Classes") {
        let _ = repair_cascade_menu(HKEY_CURRENT_USER, r"Software\Classes");
    }
    if is_machine_registered() && menu_needs_repair(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes") {
        let _ = repair_cascade_menu(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes");
    }
    if clipboard_has_items() {
        if show_background_verbs().is_ok() || icons_changed {
            notify_assoc_changed();
        }
        return;
    }
    sync_background_verbs(false);
    if icons_changed {
        notify_assoc_changed();
    }
}

fn legacy_rename_paths(classes: &str) -> [String; 2] {
    [
        format!(r"{}\{LEGACY_CASCADE_RENAME}", menu_store(classes)),
        format!(r"{classes}\{LEGACY_RENAME_VERB}"),
    ]
}

fn legacy_rename_present(hive: winreg::HKEY, classes: &str) -> bool {
    legacy_rename_paths(classes)
        .iter()
        .any(|path| hive_has(hive, path))
}

fn delete_legacy_rename(root: &RegKey, classes: &str) -> Result<()> {
    for path in legacy_rename_paths(classes) {
        delete_if_exists(root, &path)?;
    }
    Ok(())
}

pub fn try_update_menu_labels() {
    let t = ui_strings();
    let mut updated = false;
    for (hive, classes) in [
        (HKEY_CURRENT_USER, r"Software\Classes"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes"),
    ] {
        let store = menu_store(classes);
        for (sub, label) in menu_label_entries(t) {
            let path = if sub.is_empty() {
                store.clone()
            } else {
                format!(r"{store}\{sub}")
            };
            updated |= set_verb_label(hive, &path, label).is_ok();
        }
    }
    let paste_label = paste_menu_label(t);
    updated |= set_background_verb_label(HKEY_CURRENT_USER, HKCU_PASTE_VERB, &paste_label).is_ok();
    if updated {
        notify_assoc_changed();
    }
}

pub fn is_user_registered() -> bool {
    shell_menu_present(HKEY_CURRENT_USER, r"Software\Classes")
}

pub fn is_machine_registered() -> bool {
    shell_menu_present(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes")
}

fn shell_menu_present(hive: winreg::HKEY, classes: &str) -> bool {
    visible_cascade_present(hive, classes) || com_handler_installed(hive, classes)
}

fn com_handler_installed(hive: winreg::HKEY, classes: &str) -> bool {
    COM_CLEANUP_PROGIDS.iter().any(|progid| {
        hive_has(
            hive,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}"),
        ) || hive_has(
            hive,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\FastCopyRust"),
        )
    })
}

pub fn register() -> Result<()> {
    register_hive(HKEY_CURRENT_USER, r"Software\Classes")
}

fn register_hive(hive: winreg::HKEY, classes: &str) -> Result<()> {
    write_cascade_keys(hive, classes)?;
    write_com_keys(hive, classes)?;
    let root = RegKey::predef(hive);
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyCut"))?;
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyCopy"))?;
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyDelete"))?;
    delete_if_exists(&root, &format!(r"{classes}\Directory\shell\FastCopyPaste"))?;
    delete_if_exists(
        &root,
        &format!(r"{classes}\Directory\Background\shell\FastCopyRust"),
    )?;
    delete_legacy_rename(&root, classes)?;
    apply_background_verbs(clipboard_has_items())?;
    notify_assoc_changed();
    Ok(())
}

fn menu_store(classes: &str) -> String {
    let prefix = classes
        .strip_suffix("\\Classes")
        .or_else(|| classes.strip_suffix("\\classes"))
        .unwrap_or(classes);
    format!(r"{prefix}\FastCopyMenu")
}

fn menu_label_entries(t: &'static crate::i18n::Strings) -> [(&'static str, &'static str); 11] {
    [
        ("", t.menu_cascade),
        (CASCADE_CUT, t.menu_cut),
        (CASCADE_COPY, t.menu_copy),
        (CASCADE_DELETE, t.menu_delete),
        (CASCADE_SYMLINK, t.menu_copy_symlink),
        (CASCADE_HARDLINK, t.menu_copy_hardlink),
        (CASCADE_OPEN_TARGET, t.menu_open_target),
        (CASCADE_SHOW_SOURCE, t.menu_show_source),
        (CASCADE_SIZE, t.menu_size),
        (CASCADE_COPY_PATHS, t.menu_copy_paths),
        (CASCADE_SETTINGS, t.settings_title),
    ]
}

fn write_menu_labels(root: &RegKey, store: &str, t: &'static crate::i18n::Strings) -> Result<()> {
    for (sub, label) in menu_label_entries(t) {
        let path = if sub.is_empty() {
            store.to_string()
        } else {
            format!(r"{store}\{sub}")
        };
        let (key, _) = root.create_subkey(path)?;
        key.set_value("MUIVerb", &label)?;
    }
    Ok(())
}

fn write_cascade_keys(hive: winreg::HKEY, classes: &str) -> Result<()> {
    let t = ui_strings();
    install_menu_icons()?;
    let root = RegKey::predef(hive);
    // Labels stay outside shell\ so Explorer cannot draw a second 快速复制.
    write_menu_labels(&root, &menu_store(classes), t)?;
    for rel in VISIBLE_CASCADES {
        delete_if_exists(&root, &format!(r"{classes}\{rel}"))?;
    }
    Ok(())
}

fn shell_dll_path() -> Result<PathBuf> {
    let executable = env::current_exe().context(ui_strings().cannot_get_exe_path())?;
    let Some(directory) = executable.parent() else {
        return Err(anyhow!("{}", ui_strings().cannot_get_exe_path()));
    };
    let next_to_exe = directory.join(SHELL_DLL_NAME);
    if next_to_exe.is_file() {
        return Ok(next_to_exe);
    }
    if directory
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("deps"))
    {
        if let Some(parent) = directory.parent() {
            let sibling = parent.join(SHELL_DLL_NAME);
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    Ok(next_to_exe)
}

fn write_com_keys(hive: winreg::HKEY, classes: &str) -> Result<()> {
    let dll = shell_dll_path()?;
    if !dll.is_file() {
        return Err(anyhow!("{}", ui_strings().cannot_find_shell_dll()));
    }
    write_com_keys_at(hive, classes, &dll.to_string_lossy())
}

fn write_com_keys_at(hive: winreg::HKEY, classes: &str, dll: &str) -> Result<()> {
    let root = RegKey::predef(hive);
    let (clsid, _) = root.create_subkey(format!(r"{classes}\CLSID\{SHELL_CLSID}"))?;
    clsid.set_value("", &"FastCopy Context Menu")?;
    let (inproc, _) = clsid.create_subkey("InProcServer32")?;
    inproc.set_value("", &dll)?;
    inproc.set_value("ThreadingModel", &"Apartment")?;
    for progid in COM_REGISTER_PROGIDS {
        let (key, _) = root.create_subkey(format!(
            r"{classes}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}"
        ))?;
        key.set_value("", &SHELL_CLSID)?;
    }
    for progid in COM_CLEANUP_PROGIDS {
        if COM_REGISTER_PROGIDS.contains(progid) {
            let _ = delete_if_exists(
                &root,
                &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\FastCopyRust"),
            );
            continue;
        }
        delete_if_exists(
            &root,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}"),
        )?;
        delete_if_exists(
            &root,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\FastCopyRust"),
        )?;
    }
    if classes.eq_ignore_ascii_case(r"Software\Classes")
        || classes.eq_ignore_ascii_case(r"SOFTWARE\Classes")
    {
        write_approved_value(hive)?;
    }
    Ok(())
}

fn write_approved_value(hive: winreg::HKEY) -> Result<()> {
    let path = if hive == HKEY_CURRENT_USER {
        r"Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved"
    } else {
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved"
    };
    let (key, _) = RegKey::predef(hive).create_subkey(path)?;
    key.set_value(SHELL_CLSID, &"FastCopy")?;
    Ok(())
}

fn delete_approved_value(hive: winreg::HKEY) -> Result<()> {
    let path = if hive == HKEY_CURRENT_USER {
        r"Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved"
    } else {
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved"
    };
    let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(path, KEY_SET_VALUE) else {
        return Ok(());
    };
    match key.delete_value(SHELL_CLSID) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn com_handler_ready(hive: winreg::HKEY, classes: &str) -> bool {
    COM_REGISTER_PROGIDS.iter().all(|progid| {
        hive_has(
            hive,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}"),
        )
    }) && COM_CLEANUP_PROGIDS.iter().all(|progid| {
        if COM_REGISTER_PROGIDS.contains(progid) {
            return !hive_has(
                hive,
                &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\FastCopyRust"),
            );
        }
        !hive_has(
            hive,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}"),
        ) && !hive_has(
            hive,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\FastCopyRust"),
        )
    }) && com_dll_path_matches(hive, classes)
}

fn com_dll_path_matches(hive: winreg::HKEY, classes: &str) -> bool {
    let Ok(expected) = shell_dll_path() else {
        return false;
    };
    let Ok(key) =
        RegKey::predef(hive).open_subkey(format!(r"{classes}\CLSID\{SHELL_CLSID}\InProcServer32"))
    else {
        return false;
    };
    let registered: String = key.get_value("").unwrap_or_default();
    registered.eq_ignore_ascii_case(&expected.to_string_lossy())
}

fn delete_cascade_keys(hive: winreg::HKEY, classes: &str) -> Result<()> {
    let root = RegKey::predef(hive);
    for rel in VISIBLE_CASCADES {
        delete_if_exists(&root, &format!(r"{classes}\{rel}"))?;
    }
    for rel in [
        r"Directory\Background\shell\FastCopyPaste",
        r"Directory\Background\shell\FastCopyClear",
        LEGACY_RENAME_VERB,
    ] {
        delete_if_exists(&root, &format!(r"{classes}\{rel}"))?;
    }
    delete_if_exists(&root, &menu_store(classes))?;
    for progid in COM_CLEANUP_PROGIDS {
        delete_if_exists(
            &root,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}"),
        )?;
        delete_if_exists(
            &root,
            &format!(r"{classes}\{progid}\shellex\ContextMenuHandlers\FastCopyRust"),
        )?;
    }
    delete_if_exists(&root, &format!(r"{classes}\CLSID\{SHELL_CLSID}"))?;
    if classes.eq_ignore_ascii_case(r"Software\Classes")
        || classes.eq_ignore_ascii_case(r"SOFTWARE\Classes")
    {
        delete_approved_value(hive)?;
        delete_cached_handler(hive)?;
    }
    Ok(())
}

fn delete_cached_handler(hive: winreg::HKEY) -> Result<()> {
    let path = if hive == HKEY_CURRENT_USER {
        r"Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Cached"
    } else {
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Shell Extensions\Cached"
    };
    let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(path, KEY_SET_VALUE) else {
        return Ok(());
    };
    match key.delete_value(SHELL_CLSID) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
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
    delete_cascade_keys(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes")?;
    delete_command_store(
        &RegKey::predef(HKEY_LOCAL_MACHINE),
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\CommandStore\shell",
    )?;
    Ok(())
}

fn delete_command_store(root: &RegKey, store: &str) -> Result<()> {
    let Ok(command_store) = root.open_subkey_with_flags(store, winreg::enums::KEY_ALL_ACCESS)
    else {
        return Ok(());
    };
    let names: Vec<String> = command_store
        .enum_keys()
        .filter_map(|item| item.ok())
        .filter(|name| name.to_ascii_lowercase().starts_with("fastcopy"))
        .collect();
    for name in names {
        match command_store.delete_subkey_all(&name) {
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
    let _ = elevate_command(argument, SW_SHOWNORMAL)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct ElevateLinkJob {
    items: Vec<ElevateLinkItem>,
}

#[derive(Serialize, Deserialize)]
struct ElevateLinkItem {
    source: PathBuf,
    target: PathBuf,
}

pub fn write_elevate_link_job(items: &[RetryItem]) -> Result<PathBuf> {
    let directory = app_data_directory();
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("elevate-links-{}.json", std::process::id()));
    let job = ElevateLinkJob {
        items: items
            .iter()
            .filter_map(|item| {
                Some(ElevateLinkItem {
                    source: item.source.clone(),
                    target: item.target.clone()?,
                })
            })
            .collect(),
    };
    fs::write(&path, serde_json::to_vec_pretty(&job)?)?;
    Ok(path)
}

pub fn read_elevate_link_job(path: &Path) -> Result<Vec<RetryItem>> {
    let job: ElevateLinkJob = serde_json::from_slice(&fs::read(path)?)?;
    Ok(job
        .items
        .into_iter()
        .map(|item| RetryItem {
            source: item.source,
            target: Some(item.target),
            delete_source: false,
        })
        .collect())
}

pub fn elevate_links(job_path: &Path) -> Result<u32> {
    let path = job_path.to_string_lossy().replace('"', "");
    elevate_command(&format!("--elevated-links \"{path}\""), SW_HIDE)
}

fn elevate_command(parameters: &str, show: i32) -> Result<u32> {
    let executable = env::current_exe()?;
    let executable = wide(executable.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let parameters = wide(OsStr::new(parameters));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: executable.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: show,
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
    if info.hProcess.is_null() {
        return Ok(0);
    }
    unsafe {
        if WaitForSingleObject(info.hProcess, INFINITE) == WAIT_FAILED {
            let code = GetLastError();
            CloseHandle(info.hProcess);
            return Err(anyhow!("{}", ui_strings().uac_wait_failed(code)));
        }
        let mut exit_code = 0u32;
        let ok = GetExitCodeProcess(info.hProcess, &mut exit_code);
        CloseHandle(info.hProcess);
        if ok == 0 {
            let code = GetLastError();
            return Err(anyhow!("{}", ui_strings().uac_wait_failed(code)));
        }
        Ok(exit_code)
    }
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
    if should_show {
        if show_background_verbs().is_ok() {
            notify_assoc_changed();
        }
        return;
    }
    if !background_verbs_visible() && background_keys_ready() {
        return;
    }
    let result = apply_background_verbs(false);
    if result.is_ok() {
        notify_assoc_changed();
    }
}

fn background_keys_ready() -> bool {
    machine_background_verbs_exist() || user_has_paste_command()
}

fn apply_background_verbs(should_show: bool) -> Result<()> {
    show_background_verbs()?;
    if !should_show {
        hide_background_verbs()?;
    }
    Ok(())
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
    let paste_label = paste_menu_label(t);
    upsert_background_verb(
        &user,
        HKCU_PASTE_VERB,
        &paste_label,
        &format!("\"{executable}\" --shell-paste \"%V\""),
        &icons.join("paste.ico"),
    )?;
    upsert_background_verb(
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

fn visible_cascade_present(hive: winreg::HKEY, classes: &str) -> bool {
    VISIBLE_CASCADES
        .iter()
        .any(|rel| hive_has(hive, &format!(r"{classes}\{rel}")))
}

fn menu_labels_ready(hive: winreg::HKEY, classes: &str) -> bool {
    let store = menu_store(classes);
    hive_has(hive, &store)
        && hive_has(hive, &format!(r"{store}\{CASCADE_CUT}"))
        && hive_has(hive, &format!(r"{store}\{CASCADE_COPY}"))
        && hive_has(hive, &format!(r"{store}\{CASCADE_OPEN_TARGET}"))
        && hive_has(hive, &format!(r"{store}\{CASCADE_SHOW_SOURCE}"))
        && hive_has(hive, &format!(r"{store}\{CASCADE_SETTINGS}"))
}

fn menu_needs_repair(hive: winreg::HKEY, classes: &str) -> bool {
    visible_cascade_present(hive, classes)
        || legacy_rename_present(hive, classes)
        || !com_handler_ready(hive, classes)
        || !menu_labels_ready(hive, classes)
}

fn repair_cascade_menu(hive: winreg::HKEY, classes: &str) -> Result<()> {
    register_hive(hive, classes)
}

fn set_verb_label(hive: winreg::HKEY, path: &str, label: &str) -> Result<()> {
    let key = RegKey::predef(hive).open_subkey_with_flags(path, KEY_SET_VALUE)?;
    key.set_value("MUIVerb", &label)?;
    Ok(())
}

fn set_background_verb_label(hive: winreg::HKEY, path: &str, label: &str) -> Result<()> {
    let key = RegKey::predef(hive).open_subkey_with_flags(path, KEY_SET_VALUE)?;
    key.set_value("", &label)?;
    key.set_value("MUIVerb", &label)?;
    Ok(())
}

fn clipboard_snapshot() -> Option<ClipboardData> {
    read_json::<ClipboardData>(&app_data_directory().join(CLIPBOARD_FILE))
        .filter(|data| !data.paths.is_empty())
}

fn paste_menu_label(t: &crate::i18n::Strings) -> String {
    match clipboard_snapshot() {
        Some(data) => match data.kind {
            ClipboardKind::Copy | ClipboardKind::Move => t.menu_paste.to_string(),
            ClipboardKind::CopySymlink => t.menu_paste_as_symlink(data.paths.len()),
            ClipboardKind::CopyHardlink => t.menu_paste_as_hardlink(data.paths.len()),
        },
        None => t.menu_paste.to_string(),
    }
}

fn ui_strings() -> &'static crate::i18n::Strings {
    crate::i18n::strings(
        read_json::<Settings>(&settings_path())
            .map(|settings| settings.language)
            .unwrap_or_default(),
    )
}

fn upsert_background_verb(
    root: &RegKey,
    path: &str,
    label: &str,
    command: &str,
    icon: &Path,
) -> Result<()> {
    let (key, _) = root.create_subkey(path)?;
    key.set_value("", &label)?;
    key.set_value("MUIVerb", &label)?;
    key.set_value("Icon", &icon_value(icon))?;
    delete_value_if_exists(&key, "MultiSelectModel")?;
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

fn rewrite_menu_icons_if_changed() -> bool {
    let directory = app_data_directory().join("icons");
    let stale = menu_icon_files().iter().any(|(name, bytes)| {
        fs::read(directory.join(name)).ok().as_deref() != Some(*bytes)
    });
    if !stale {
        return false;
    }
    install_menu_icons().is_ok()
}

fn menu_icon_files() -> [(&'static str, &'static [u8]); 8] {
    [
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
        (
            "size.ico",
            include_bytes!("../../assets/icons/size.ico").as_slice(),
        ),
        (
            "path.ico",
            include_bytes!("../../assets/icons/path.ico").as_slice(),
        ),
        (
            "settings.ico",
            include_bytes!("../../assets/icons/settings.ico").as_slice(),
        ),
    ]
}

fn install_menu_icons() -> Result<PathBuf> {
    let directory = app_data_directory().join("icons");
    fs::create_dir_all(&directory)?;
    for (name, bytes) in menu_icon_files() {
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

    struct InprocRestore {
        path: String,
        previous: Option<String>,
    }

    impl Drop for InprocRestore {
        fn drop(&mut self) {
            let Some(previous) = self.previous.as_ref() else {
                return;
            };
            let Ok(key) = RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey_with_flags(&self.path, KEY_SET_VALUE)
            else {
                return;
            };
            let _ = key.set_value("", previous);
        }
    }

    const TEST_ROOT: &str = r"Software\FastCopyRustMenuTest";
    const TEST_CLASSES: &str = r"Software\FastCopyRustMenuTest\Classes";

    #[test]
    fn register_and_unregister_test_classes() {
        let _ = delete_if_exists(&RegKey::predef(HKEY_CURRENT_USER), TEST_ROOT);
        let file_key = format!(r"{TEST_CLASSES}\*\shell\FastCopyRust");
        let dir_key = format!(r"{TEST_CLASSES}\Directory\shell\FastCopyRust");
        let (dummy, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(&file_key).unwrap();
        dummy.set_value("MUIVerb", &"old-file-menu").unwrap();
        dummy.set_value("MultiSelectModel", &"Single").unwrap();
        let (old_cut, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(format!(r"{file_key}\{CASCADE_CUT}"))
            .unwrap();
        old_cut.set_value("MUIVerb", &"old-cut").unwrap();
        write_cascade_keys(HKEY_CURRENT_USER, TEST_CLASSES).unwrap();
        let dll = shell_dll_path().expect("shell dll path");
        assert!(dll.is_file(), "missing {}", dll.display());
        write_com_keys_at(HKEY_CURRENT_USER, TEST_CLASSES, &dll.to_string_lossy()).unwrap();
        let store = menu_store(TEST_CLASSES);
        for progid in COM_REGISTER_PROGIDS {
            assert!(hive_has(
                HKEY_CURRENT_USER,
                &format!(r"{TEST_CLASSES}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}")
            ));
        }
        for progid in COM_CLEANUP_PROGIDS {
            if COM_REGISTER_PROGIDS.contains(progid) {
                continue;
            }
            assert!(!hive_has(
                HKEY_CURRENT_USER,
                &format!(r"{TEST_CLASSES}\{progid}\shellex\ContextMenuHandlers\{SHELL_HANDLER}")
            ));
        }
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{TEST_CLASSES}\CLSID\{SHELL_CLSID}\InProcServer32")
        ));
        let model_thread: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!(r"{TEST_CLASSES}\CLSID\{SHELL_CLSID}\InProcServer32"))
            .unwrap()
            .get_value("ThreadingModel")
            .unwrap();
        assert_eq!(model_thread, "Apartment");
        assert!(!hive_has(HKEY_CURRENT_USER, &file_key));
        assert!(!hive_has(HKEY_CURRENT_USER, &dir_key));
        let cascade_label: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&store)
            .unwrap()
            .get_value("MUIVerb")
            .unwrap();
        assert!(!cascade_label.is_empty());
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{store}\{CASCADE_OPEN_TARGET}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{store}\{CASCADE_SHOW_SOURCE}")
        ));
        assert!(!menu_needs_repair(HKEY_CURRENT_USER, TEST_CLASSES));
        let root = RegKey::predef(HKEY_CURRENT_USER);
        for path in legacy_rename_paths(TEST_CLASSES) {
            root.create_subkey(&path).unwrap();
        }
        assert!(menu_needs_repair(HKEY_CURRENT_USER, TEST_CLASSES));
        delete_legacy_rename(&root, TEST_CLASSES).unwrap();
        assert!(!legacy_rename_present(HKEY_CURRENT_USER, TEST_CLASSES));
        assert!(!menu_needs_repair(HKEY_CURRENT_USER, TEST_CLASSES));
        let (legacy, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&file_key)
            .unwrap();
        legacy.set_value("MUIVerb", &"快速复制").unwrap();
        legacy.set_value("SubCommands", &"").unwrap();
        let extra_handler =
            format!(r"{TEST_CLASSES}\*\shellex\ContextMenuHandlers\{SHELL_HANDLER}");
        let (extra, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&extra_handler)
            .unwrap();
        extra.set_value("", &SHELL_CLSID).unwrap();
        let dir_handler =
            format!(r"{TEST_CLASSES}\Directory\shellex\ContextMenuHandlers\{SHELL_HANDLER}");
        let (dir_extra, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&dir_handler)
            .unwrap();
        dir_extra.set_value("", &SHELL_CLSID).unwrap();
        assert!(menu_needs_repair(HKEY_CURRENT_USER, TEST_CLASSES));
        delete_cascade_keys(HKEY_CURRENT_USER, TEST_CLASSES).unwrap();
        assert!(!hive_has(HKEY_CURRENT_USER, &file_key));
        assert!(!hive_has(HKEY_CURRENT_USER, &dir_key));
        assert!(!hive_has(HKEY_CURRENT_USER, &extra_handler));
        assert!(!hive_has(HKEY_CURRENT_USER, &dir_handler));
        assert!(!hive_has(
            HKEY_CURRENT_USER,
            &format!(
                r"{TEST_CLASSES}\AllFilesystemObjects\shellex\ContextMenuHandlers\{SHELL_HANDLER}"
            )
        ));
        assert!(!hive_has(HKEY_CURRENT_USER, &store));
        assert!(!hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{TEST_CLASSES}\CLSID\{SHELL_CLSID}")
        ));
        let _ = delete_if_exists(&RegKey::predef(HKEY_CURRENT_USER), TEST_ROOT);
    }

    #[test]
    fn shell_extension_com_server_loads() {
        let dll = shell_dll_path().expect("shell dll path");
        assert!(dll.is_file(), "missing {}", dll.display());
        let inproc = format!(r"Software\Classes\CLSID\{SHELL_CLSID}\InProcServer32");
        let previous: Option<String> = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&inproc)
            .ok()
            .and_then(|key| key.get_value("").ok());
        write_com_keys(HKEY_CURRENT_USER, r"Software\Classes").unwrap();
        let _restore = InprocRestore {
            path: inproc,
            previous,
        };
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
            let clsid = windows::core::GUID::from_u128(0xB3E8D47A_6C1F_4A92_9E05_8F4C2B17A6D0);
            let created: windows::core::Result<windows::Win32::UI::Shell::IContextMenu> =
                windows::Win32::System::Com::CoCreateInstance(
                    &clsid,
                    None,
                    windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
                );
            match created {
                Ok(menu) => drop(menu),
                Err(error) if error.code() == windows::core::HRESULT(0x80040154u32 as i32) => {}
                Err(error) => panic!("CoCreate FastCopy shell extension: {error}"),
            }
        }
    }

    #[test]
    fn selected_item_menu_shows_fastcopy_once() {
        if !is_user_registered() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        fs::write(&file, b"x").unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        for path in [&file, &nested] {
            let labels = crate::windows::explorer_sel::item_menu_labels(path)
                .unwrap_or_else(|error| panic!("query item menu {}: {error}", path.display()));
            let hits = labels
                .iter()
                .filter(|label| label.contains("快速复制") || label.eq_ignore_ascii_case("FastCopy"))
                .count();
            assert!(
                hits <= 1,
                "{} shows {hits} FastCopy entries: {}",
                path.display(),
                labels.join(" | ")
            );
        }
    }

    #[test]
    fn paste_link_labels_include_count() {
        let zh = crate::i18n::ZH;
        assert_eq!(zh.menu_paste_as_symlink(1), "粘贴为符号链接");
        assert_eq!(zh.menu_paste_as_symlink(3), "粘贴(3个文件)为符号链接");
        assert_eq!(zh.menu_paste_as_hardlink(2), "粘贴(2个文件)为硬链接");
        let en = crate::i18n::EN;
        assert_eq!(en.menu_paste_as_hardlink(1), "Paste as hard link");
        assert_eq!(
            en.menu_paste_as_symlink(4),
            "Paste (4 files) as symbolic link"
        );
    }

    #[test]
    fn background_paste_appears_in_explorer_menu() {
        show_background_verbs().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let labels = crate::windows::explorer_sel::background_menu_labels(dir.path())
            .expect("query Explorer background menu");
        let joined = labels.join(" | ");
        assert!(
            labels
                .iter()
                .any(|label| label.contains("快速粘贴") || label.contains("Quick Paste")),
            "background menu missing paste: {joined}"
        );
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(HKCU_PASTE_VERB)
            .unwrap();
        let model: std::io::Result<String> = key.get_value("MultiSelectModel");
        assert!(model.is_err(), "background paste must not set MultiSelectModel");
        sync_background_verbs(clipboard_has_items());
    }

    #[test]
    fn take_clipboard_keep_leaves_copy_list() {
        let dir = tempfile::tempdir().unwrap();
        let data = ClipboardData {
            kind: ClipboardKind::Copy,
            paths: vec![PathBuf::from(r"C:\a.txt")],
            updated_millis: 1,
        };
        fs::write(
            dir.path().join(CLIPBOARD_FILE),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        let request = take_clipboard_locked(
            dir.path(),
            PathBuf::from(r"D:\dest"),
            Settings::default(),
            true,
        )
        .unwrap();
        assert_eq!(request.kind, OperationKind::Copy);
        assert_eq!(request.sources, vec![PathBuf::from(r"C:\a.txt")]);
        assert!(dir.path().join(CLIPBOARD_FILE).is_file());
    }

    #[test]
    fn take_clipboard_clears_copy_list() {
        let dir = tempfile::tempdir().unwrap();
        let data = ClipboardData {
            kind: ClipboardKind::Copy,
            paths: vec![PathBuf::from(r"C:\a.txt")],
            updated_millis: 1,
        };
        fs::write(
            dir.path().join(CLIPBOARD_FILE),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        take_clipboard_locked(
            dir.path(),
            PathBuf::from(r"D:\dest"),
            Settings::default(),
            false,
        )
        .unwrap();
        assert!(!dir.path().join(CLIPBOARD_FILE).exists());
    }

    #[test]
    fn take_clipboard_move_clears_even_when_keep() {
        let dir = tempfile::tempdir().unwrap();
        let data = ClipboardData {
            kind: ClipboardKind::Move,
            paths: vec![PathBuf::from(r"C:\a.txt")],
            updated_millis: 1,
        };
        fs::write(
            dir.path().join(CLIPBOARD_FILE),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        let request = take_clipboard_locked(
            dir.path(),
            PathBuf::from(r"D:\dest"),
            Settings::default(),
            true,
        )
        .unwrap();
        assert_eq!(request.kind, OperationKind::Move);
        assert!(!dir.path().join(CLIPBOARD_FILE).exists());
    }
}
