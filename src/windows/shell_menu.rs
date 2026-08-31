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
const CASCADE_RENAME: &str = r"shell\9rename";
const CASCADE_SETTINGS: &str = r"shell\zsettings";
const CASCADE_SEPARATOR_BEFORE: u32 = 0x20;
const LINK_APPLIES_TO: &str = "System.FileExtension:.lnk OR System.FileAttributes:1024";

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
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_SYMLINK}"),
                t.menu_copy_symlink,
            )
            .is_ok();
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_HARDLINK}"),
                t.menu_copy_hardlink,
            )
            .is_ok();
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_OPEN_TARGET}"),
                t.menu_open_target,
            )
            .is_ok();
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_SHOW_SOURCE}"),
                t.menu_show_source,
            )
            .is_ok();
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_SIZE}"),
                t.menu_size,
            )
            .is_ok();
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_COPY_PATHS}"),
                t.menu_copy_paths,
            )
            .is_ok();
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_RENAME}"),
                t.menu_rename,
            )
            .is_ok();
            updated |= set_verb_label(
                hive,
                &format!(r"{parent}\{CASCADE_SETTINGS}"),
                t.settings_title,
            )
            .is_ok();
        }
    }
    let paste_label = paste_menu_label(t);
    updated |= set_background_verb_label(HKEY_CURRENT_USER, HKCU_PASTE_VERB, &paste_label).is_ok();
    if updated {
        notify_assoc_changed();
    }
}

pub fn is_user_registered() -> bool {
    hive_has(
        HKEY_CURRENT_USER,
        r"Software\Classes\Directory\shell\FastCopyRust",
    ) || hive_has(HKEY_CURRENT_USER, r"Software\Classes\*\shell\FastCopyRust")
}

pub fn is_machine_registered() -> bool {
    hive_has(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Classes\Directory\shell\FastCopyRust",
    ) || hive_has(HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes\*\shell\FastCopyRust")
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
    apply_background_verbs(clipboard_has_items())?;
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

fn create_cascade(
    root: &RegKey,
    parent: &str,
    executable: &str,
    icons: &Path,
) -> Result<()> {
    let t = ui_strings();
    let (key, _) = root.create_subkey(parent)?;
    key.set_value("MUIVerb", &t.menu_cascade)?;
    key.set_value("SubCommands", &"")?;
    key.set_value("MultiSelectModel", &"Document")?;
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
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_SYMLINK}"),
        t.menu_copy_symlink,
        &format!("\"{executable}\" --shell-copy-symlink \"%1\""),
        &icons.join("copy.ico"),
    )?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_HARDLINK}"),
        t.menu_copy_hardlink,
        &format!("\"{executable}\" --shell-copy-hardlink \"%1\""),
        &icons.join("copy.ico"),
    )?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_OPEN_TARGET}"),
        t.menu_open_target,
        &format!("\"{executable}\" --shell-open-target \"%1\""),
        &icons.join("app.ico"),
    )?;
    set_link_only_verb(root, &format!(r"{parent}\{CASCADE_OPEN_TARGET}"))?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_SHOW_SOURCE}"),
        t.menu_show_source,
        &format!("\"{executable}\" --shell-show-source \"%1\""),
        &icons.join("app.ico"),
    )?;
    set_link_only_verb(root, &format!(r"{parent}\{CASCADE_SHOW_SOURCE}"))?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_SIZE}"),
        t.menu_size,
        &format!("\"{executable}\" --shell-size \"%1\""),
        &icons.join("size.ico"),
    )?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_COPY_PATHS}"),
        t.menu_copy_paths,
        &format!("\"{executable}\" --shell-copy-path \"%1\""),
        &icons.join("path.ico"),
    )?;
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_RENAME}"),
        t.menu_rename,
        &format!("\"{executable}\" --shell-rename \"%1\""),
        &icons.join("rename.ico"),
    )?;
    let _ = delete_if_exists(root, &format!(r"{parent}\shell\6settings"));
    upsert_user_verb(
        root,
        &format!(r"{parent}\{CASCADE_SETTINGS}"),
        t.settings_title,
        &format!("\"{executable}\" --settings"),
        &icons.join("settings.ico"),
    )?;
    let settings_key = root.open_subkey_with_flags(
        &format!(r"{parent}\{CASCADE_SETTINGS}"),
        KEY_SET_VALUE,
    )?;
    settings_key.set_value("CommandFlags", &CASCADE_SEPARATOR_BEFORE)?;
    Ok(())
}

fn menu_needs_repair(hive: winreg::HKEY, classes: &str) -> bool {
    hive_has(hive, &format!(r"{classes}\Directory\shell\FastCopyCopy"))
        || hive_has(hive, &format!(r"{classes}\Directory\shell\FastCopyCut"))
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_COPY}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_SYMLINK}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SYMLINK}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_CUT}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_COPY}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_DELETE}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SETTINGS}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_SETTINGS}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_OPEN_TARGET}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_OPEN_TARGET}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SHOW_SOURCE}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_SHOW_SOURCE}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SIZE}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_SIZE}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_COPY_PATHS}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_COPY_PATHS}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_RENAME}"),
        )
        || !hive_has(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_RENAME}"),
        )
        || hive_has(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\shell\6settings"),
        )
        || !verb_icon_ends_with(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SIZE}"),
            "size.ico",
        )
        || !verb_icon_ends_with(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_COPY_PATHS}"),
            "path.ico",
        )
        || !verb_icon_ends_with(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_RENAME}"),
            "rename.ico",
        )
        || !verb_icon_ends_with(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SETTINGS}"),
            "settings.ico",
        )
        || !verb_is_single(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SHOW_SOURCE}"),
        )
        || !verb_is_single(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_SHOW_SOURCE}"),
        )
        || !verb_is_single(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_OPEN_TARGET}"),
        )
        || !verb_is_single(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_OPEN_TARGET}"),
        )
        || !verb_applies_to_links(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_SHOW_SOURCE}"),
        )
        || !verb_applies_to_links(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_SHOW_SOURCE}"),
        )
        || !verb_applies_to_links(
            hive,
            &format!(r"{classes}\*\shell\FastCopyRust\{CASCADE_OPEN_TARGET}"),
        )
        || !verb_applies_to_links(
            hive,
            &format!(r"{classes}\Directory\shell\FastCopyRust\{CASCADE_OPEN_TARGET}"),
        )
        || !verb_is_document(hive, &format!(r"{classes}\Directory\shell\FastCopyRust"))
        || !verb_is_document(hive, &format!(r"{classes}\*\shell\FastCopyRust"))
}

fn verb_icon_ends_with(hive: winreg::HKEY, path: &str, suffix: &str) -> bool {
    let Ok(key) = RegKey::predef(hive).open_subkey(path) else {
        return false;
    };
    let icon: String = key.get_value("Icon").unwrap_or_default();
    icon.to_ascii_lowercase()
        .ends_with(&suffix.to_ascii_lowercase())
}

fn verb_is_document(hive: winreg::HKEY, path: &str) -> bool {
    verb_multi_select_model(hive, path) == "Document"
}

fn verb_is_single(hive: winreg::HKEY, path: &str) -> bool {
    verb_multi_select_model(hive, path) == "Single"
}

fn verb_multi_select_model(hive: winreg::HKEY, path: &str) -> String {
    let Ok(key) = RegKey::predef(hive).open_subkey(path) else {
        return String::new();
    };
    key.get_value("MultiSelectModel").unwrap_or_default()
}

fn verb_applies_to_links(hive: winreg::HKEY, path: &str) -> bool {
    let Ok(key) = RegKey::predef(hive).open_subkey(path) else {
        return false;
    };
    let applies: String = key.get_value("AppliesTo").unwrap_or_default();
    applies == LINK_APPLIES_TO
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
    key.set_value("MultiSelectModel", &"Document")?;
    delete_value_if_exists(&key, "LegacyDisable")?;
    delete_value_if_exists(&key, "ProgrammaticAccessOnly")?;
    delete_value_if_exists(&key, "AppliesTo")?;
    let (command_key, _) = key.create_subkey("command")?;
    command_key.set_value("", &command)?;
    Ok(())
}

fn set_link_only_verb(root: &RegKey, path: &str) -> Result<()> {
    let key = root.open_subkey_with_flags(path, KEY_SET_VALUE)?;
    key.set_value("MultiSelectModel", &"Single")?;
    key.set_value("AppliesTo", &LINK_APPLIES_TO)?;
    Ok(())
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

fn menu_icon_files() -> [(&'static str, &'static [u8]); 9] {
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
            "rename.ico",
            include_bytes!("../../assets/icons/rename.ico").as_slice(),
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
        assert!(hive_has(HKEY_CURRENT_USER, &file_key));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_CUT}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_COPY}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_DELETE}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_SYMLINK}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_HARDLINK}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_OPEN_TARGET}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_SHOW_SOURCE}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_SIZE}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_COPY_PATHS}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_RENAME}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{dir_key}\{CASCADE_SHOW_SOURCE}")
        ));
        let show_key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!(r"{file_key}\{CASCADE_SHOW_SOURCE}"))
            .unwrap();
        let show_model: String = show_key.get_value("MultiSelectModel").unwrap();
        assert_eq!(show_model, "Single");
        let applies: String = show_key.get_value("AppliesTo").unwrap();
        assert_eq!(applies, LINK_APPLIES_TO);
        let dir_show: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!(r"{dir_key}\{CASCADE_SHOW_SOURCE}"))
            .unwrap()
            .get_value("MultiSelectModel")
            .unwrap();
        assert_eq!(dir_show, "Single");
        let open_key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!(r"{file_key}\{CASCADE_OPEN_TARGET}"))
            .unwrap();
        let open_model: String = open_key.get_value("MultiSelectModel").unwrap();
        assert_eq!(open_model, "Single");
        let open_applies: String = open_key.get_value("AppliesTo").unwrap();
        assert_eq!(open_applies, LINK_APPLIES_TO);
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{file_key}\{CASCADE_SETTINGS}")
        ));
        assert!(hive_has(HKEY_CURRENT_USER, &dir_key));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{dir_key}\{CASCADE_COPY}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{dir_key}\{CASCADE_SYMLINK}")
        ));
        assert!(hive_has(
            HKEY_CURRENT_USER,
            &format!(r"{dir_key}\{CASCADE_SETTINGS}")
        ));
        let model: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&dir_key)
            .unwrap()
            .get_value("MultiSelectModel")
            .unwrap();
        assert_eq!(model, "Document");
        let file_model: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&file_key)
            .unwrap()
            .get_value("MultiSelectModel")
            .unwrap();
        assert_eq!(file_model, "Document");
        assert!(!menu_needs_repair(HKEY_CURRENT_USER, TEST_CLASSES));
        let dir = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(&dir_key, KEY_SET_VALUE)
            .unwrap();
        dir.set_value("MultiSelectModel", &"Single").unwrap();
        assert!(menu_needs_repair(HKEY_CURRENT_USER, TEST_CLASSES));
        delete_cascade_keys(HKEY_CURRENT_USER, TEST_CLASSES).unwrap();
        assert!(!hive_has(HKEY_CURRENT_USER, &file_key));
        assert!(!hive_has(HKEY_CURRENT_USER, &dir_key));
        let _ = delete_if_exists(&RegKey::predef(HKEY_CURRENT_USER), TEST_ROOT);
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
