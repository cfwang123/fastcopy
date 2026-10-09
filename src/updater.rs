use crate::i18n::Strings;
use crate::windows::shell_menu;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::env;
use std::ffi::{OsStr, c_void};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
use windows_sys::Win32::Networking::WinInet::{
    HTTP_QUERY_CONTENT_LENGTH, HTTP_QUERY_FLAG_NUMBER, HTTP_QUERY_STATUS_CODE, HttpQueryInfoW,
    INTERNET_FLAG_NO_CACHE_WRITE, INTERNET_FLAG_NO_UI, INTERNET_FLAG_RELOAD,
    INTERNET_OPEN_TYPE_PRECONFIG, INTERNET_OPTION_CONNECT_TIMEOUT, INTERNET_OPTION_RECEIVE_TIMEOUT,
    INTERNET_OPTION_SEND_TIMEOUT, InternetCloseHandle, InternetOpenUrlW, InternetOpenW,
    InternetReadFile, InternetSetOptionW,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

const REPO_API: &str = "https://api.github.com/repos/cfwang123/fastcopy/releases/latest";
pub const REPO_PAGE: &str = "https://github.com/cfwang123/fastcopy/releases";
const GITHUB_MIRRORS: [&str; 3] = [
    "https://ghfast.top/",
    "https://mirror.ghproxy.com/",
    "https://ghproxy.net/",
];
const UPDATER_EXE: &str = "fastcopy_updater.exe";
const MAIN_EXE: &str = "fastcopy.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub current: String,
    pub version: String,
    pub asset_name: String,
    pub download_url: String,
    pub size: u64,
    pub has_update: bool,
}

#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[derive(Default)]
pub struct DownloadProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn update_directory() -> PathBuf {
    shell_menu::app_data_directory().join("update")
}

pub fn check_latest(t: &Strings, prefer_mirrors: bool) -> Result<UpdateInfo> {
    let never = AtomicBool::new(false);
    let mut last = None;
    // The API answers fastest directly; mirrors are only a fallback for it.
    for url in mirror_urls(REPO_API, false, prefer_mirrors) {
        let mut body = Vec::new();
        let result = http_fetch(&url, Duration::from_secs(12), &mut body, &mut |_, _| {}, &never)
            .and_then(|()| parse_release(&body, current_version(), t));
        match result {
            Ok(info) => return Ok(info),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no update source")))
}

pub fn download(
    info: &UpdateInfo,
    prefer_mirrors: bool,
    progress: &DownloadProgress,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let directory = update_directory();
    fs::create_dir_all(&directory)?;
    clear_directory(&directory);
    let name: String = info
        .asset_name
        .chars()
        .map(|ch| if "\\/:*?\"<>|".contains(ch) { '_' } else { ch })
        .collect();
    let dest = directory.join(&name);
    let part = directory.join(format!("{name}.part"));
    let mut last = None;
    for url in mirror_urls(&info.download_url, true, prefer_mirrors) {
        progress.done.store(0, Ordering::Relaxed);
        progress.total.store(info.size, Ordering::Relaxed);
        let mut file = File::create(&part)?;
        let mut report = |done: u64, total: u64| {
            progress.done.store(done, Ordering::Relaxed);
            if total > 0 {
                progress.total.store(total, Ordering::Relaxed);
            }
        };
        let result = http_fetch(&url, Duration::from_secs(30), &mut file, &mut report, cancel);
        drop(file);
        match result {
            Ok(()) => {
                let written = fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
                if written < 64 || (info.size > 0 && written != info.size) {
                    last = Some(anyhow!("{url}: size {written}, expected {}", info.size));
                    continue;
                }
                fs::rename(&part, &dest)?;
                return Ok(dest);
            }
            Err(error) if error.is::<Cancelled>() => {
                let _ = fs::remove_file(&part);
                return Err(error);
            }
            Err(error) => last = Some(error.context(url)),
        }
    }
    let _ = fs::remove_file(&part);
    Err(last.unwrap_or_else(|| anyhow!("no download source")))
}

/// Copies this exe out of the install folder, starts it as the updater, and exits.
pub fn launch_updater_and_exit(archive: &Path) -> Result<()> {
    let exe = env::current_exe()?;
    let target = exe
        .parent()
        .ok_or_else(|| anyhow!("cannot get install folder"))?;
    let directory = update_directory();
    fs::create_dir_all(&directory)?;
    let updater = directory.join(UPDATER_EXE);
    fs::copy(&exe, &updater).with_context(|| format!("copy {}", updater.display()))?;
    Command::new(&updater)
        .arg("--apply-update")
        .arg(archive)
        .arg("--target")
        .arg(target)
        .arg("--wait-pid")
        .arg(std::process::id().to_string())
        .arg("--restart")
        .current_dir(&directory)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("start {}", updater.display()))?;
    std::process::exit(0);
}

pub fn run_apply_update(arguments: &[String], t: &Strings) -> Result<i32> {
    let mut archive = None;
    let mut target = None;
    let mut wait_pid = 0u32;
    let mut restart = false;
    let mut items = arguments.iter().skip(1);
    while let Some(argument) = items.next() {
        match argument.as_str() {
            "--apply-update" => archive = items.next().map(PathBuf::from),
            "--target" => target = items.next().map(PathBuf::from),
            "--wait-pid" => wait_pid = items.next().and_then(|pid| pid.parse().ok()).unwrap_or(0),
            "--restart" => restart = true,
            _ => {}
        }
    }
    let archive = archive.ok_or_else(|| anyhow!("missing --apply-update <archive>"))?;
    let target = match target {
        Some(target) => target,
        None => env::current_exe()?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("missing --target"))?,
    };
    log_line(&format!(
        "apply archive={} target={} wait={wait_pid}",
        archive.display(),
        target.display()
    ));
    let result = apply_update(&archive, &target, wait_pid, t);
    match &result {
        Ok(()) => log_line("ok"),
        Err(error) => log_line(&format!("FAIL: {error:#}")),
    }
    result.map_err(|error| anyhow!("{}", t.update_apply_failed(&format!("{error:#}"))))?;
    if restart {
        Command::new(target.join(MAIN_EXE))
            .current_dir(&target)
            .spawn()
            .with_context(|| format!("start {}", target.join(MAIN_EXE).display()))?;
    }
    Ok(0)
}

fn apply_update(archive: &Path, target: &Path, wait_pid: u32, t: &Strings) -> Result<()> {
    if !archive.is_file() {
        bail!("archive not found: {}", archive.display());
    }
    if !target.is_dir() {
        bail!("install folder not found: {}", target.display());
    }
    if wait_pid != 0 {
        wait_process(wait_pid, Duration::from_secs(120))?;
    }
    let extract = update_directory().join("extract");
    let _ = fs::remove_dir_all(&extract);
    fs::create_dir_all(&extract)?;
    extract_archive(archive, &extract, t)?;
    let payload = find_payload(&extract)
        .ok_or_else(|| anyhow!("{MAIN_EXE} not found in {}", archive.display()))?;
    for entry in WalkDir::new(&payload).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(&payload)?;
        let dest = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest)?;
            continue;
        }
        replace_file(entry.path(), &dest)?;
        log_line(&format!("copied {}", relative.display()));
    }
    let _ = fs::remove_dir_all(&extract);
    Ok(())
}

/// Explorer keeps the shell DLL loaded and other instances may run the exe: rename those aside.
fn replace_file(src: &Path, dest: &Path) -> Result<()> {
    for _ in 0..4 {
        if fs::copy(src, dest).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    if dest.exists() {
        let stale = PathBuf::from(format!("{}.{}.old", dest.display(), now_secs()));
        fs::rename(dest, &stale).with_context(|| format!("rename {}", dest.display()))?;
    }
    fs::copy(src, dest).with_context(|| format!("copy {}", dest.display()))?;
    Ok(())
}

/// Removes `*.old` files left by an earlier update; ones still loaded stay until next time.
pub fn remove_stale_files() {
    let Some(directory) = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.starts_with("fastcopy") && name.ends_with(".old") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn clear_directory(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else if !entry.file_name().eq_ignore_ascii_case(UPDATER_EXE) {
            let _ = fs::remove_file(&path);
        }
    }
}

fn find_payload(extract: &Path) -> Option<PathBuf> {
    WalkDir::new(extract)
        .min_depth(1)
        .max_depth(3)
        .into_iter()
        .flatten()
        .find(|entry| {
            entry.file_type().is_file() && entry.file_name().eq_ignore_ascii_case(MAIN_EXE)
        })
        .and_then(|entry| entry.path().parent().map(Path::to_path_buf))
}

fn extract_archive(archive: &Path, dest: &Path, t: &Strings) -> Result<()> {
    let extension = archive
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "zip" => run_tar(archive, dest),
        "7z" => match find_7z() {
            Some(seven) => run_tool(
                Command::new(seven)
                    .arg("x")
                    .arg(archive)
                    .arg(format!("-o{}", dest.display()))
                    .args(["-y", "-bb0"]),
            ),
            // Newer Windows builds of tar.exe can also read 7z.
            None => run_tar(archive, dest).map_err(|_| anyhow!("{}", t.update_need_7z)),
        },
        _ => bail!("unsupported package: {}", archive.display()),
    }
}

fn run_tar(archive: &Path, dest: &Path) -> Result<()> {
    let system = env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let tar = PathBuf::from(system).join("System32").join("tar.exe");
    run_tool(Command::new(tar).arg("-xf").arg(archive).arg("-C").arg(dest))
}

fn run_tool(command: &mut Command) -> Result<()> {
    let output = command
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("start {:?}", command.get_program()))?;
    if !output.status.success() {
        bail!(
            "{:?} exit {}: {}",
            command.get_program(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn find_7z() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(base) = env::var_os(variable) {
            candidates.push(PathBuf::from(base).join("7-Zip").join("7z.exe"));
        }
    }
    if let Some(found) = candidates.into_iter().find(|path| path.is_file()) {
        return Some(found);
    }
    let output = Command::new("where.exe")
        .args(["7z.exe", "7za.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| PathBuf::from(line.trim()))
        .find(|path| path.is_file())
}

fn wait_process(pid: u32, timeout: Duration) -> Result<()> {
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !process.is_null() {
            let state = WaitForSingleObject(process, timeout.as_millis() as u32);
            CloseHandle(process);
            if state != 0 {
                bail!("process {pid} did not exit");
            }
        }
    }
    thread::sleep(Duration::from_millis(800));
    Ok(())
}

fn parse_release(body: &[u8], current: &str, t: &Strings) -> Result<UpdateInfo> {
    let release: Release = serde_json::from_slice(body).context("parse release")?;
    let version = normalize_version(&release.tag_name)
        .or_else(|| release.name.as_deref().and_then(normalize_version))
        .ok_or_else(|| anyhow!("release has no version: {}", release.tag_name))?;
    let packages: Vec<&Asset> = release
        .assets
        .iter()
        .filter(|asset| {
            let name = asset.name.to_ascii_lowercase();
            name.ends_with(".7z") || name.ends_with(".zip")
        })
        .collect();
    let asset = packages
        .iter()
        .find(|asset| asset.name.to_ascii_lowercase().starts_with("fastcopy"))
        .or_else(|| packages.first())
        .ok_or_else(|| anyhow!("{}", t.update_no_package))?;
    Ok(UpdateInfo {
        current: current.to_owned(),
        has_update: is_newer(&version, current),
        version,
        asset_name: asset.name.clone(),
        download_url: asset.browser_download_url.clone(),
        size: asset.size,
    })
}

fn normalize_version(text: &str) -> Option<String> {
    let start = text.find(|ch: char| ch.is_ascii_digit())?;
    let version: String = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    let version = version.trim_end_matches('.');
    (!version.is_empty()).then(|| version.to_owned())
}

fn version_parts(text: &str) -> Option<Vec<u64>> {
    let normalized = normalize_version(text)?;
    normalized.split('.').map(|part| part.parse().ok()).collect()
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    let (Some(mut remote_parts), Some(mut local_parts)) = (version_parts(remote), version_parts(local))
    else {
        return !remote.eq_ignore_ascii_case(local);
    };
    let len = remote_parts.len().max(local_parts.len());
    remote_parts.resize(len, 0);
    local_parts.resize(len, 0);
    remote_parts > local_parts
}

fn mirror_urls(primary: &str, download: bool, prefer_mirrors: bool) -> Vec<String> {
    let mirrors: Vec<String> = GITHUB_MIRRORS
        .iter()
        .map(|mirror| format!("{mirror}{primary}"))
        .collect();
    if download && prefer_mirrors {
        mirrors.into_iter().chain([primary.to_owned()]).collect()
    } else {
        [primary.to_owned()].into_iter().chain(mirrors).collect()
    }
}

struct InternetHandle(*mut c_void);

impl Drop for InternetHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                InternetCloseHandle(self.0);
            }
        }
    }
}

/// WinINet follows the system proxy and redirects (GitHub assets redirect to their CDN).
fn http_fetch(
    url: &str,
    timeout: Duration,
    sink: &mut dyn Write,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let agent = wide(&format!("FastCopy-Updater/{}", current_version()));
    let session = InternetHandle(unsafe {
        InternetOpenW(
            agent.as_ptr(),
            INTERNET_OPEN_TYPE_PRECONFIG,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    });
    if session.0.is_null() {
        bail!("InternetOpenW failed: {}", unsafe { GetLastError() });
    }
    let millis = timeout.as_millis() as u32;
    for option in [
        INTERNET_OPTION_CONNECT_TIMEOUT,
        INTERNET_OPTION_SEND_TIMEOUT,
        INTERNET_OPTION_RECEIVE_TIMEOUT,
    ] {
        unsafe {
            InternetSetOptionW(session.0, option, (&millis as *const u32).cast(), 4);
        }
    }
    let url_wide = wide(url);
    let headers = wide("Accept: application/vnd.github+json, */*\r\n");
    let request = InternetHandle(unsafe {
        InternetOpenUrlW(
            session.0,
            url_wide.as_ptr(),
            headers.as_ptr(),
            u32::MAX,
            INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE | INTERNET_FLAG_NO_UI,
            0,
        )
    });
    if request.0.is_null() {
        bail!("{url}: WinINet error {}", unsafe { GetLastError() });
    }
    let status = query_number(&request, HTTP_QUERY_STATUS_CODE).unwrap_or(0);
    if status >= 400 {
        bail!("{url}: HTTP {status}");
    }
    let total = query_number(&request, HTTP_QUERY_CONTENT_LENGTH).unwrap_or(0) as u64;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let mut read = 0u32;
        let ok = unsafe {
            InternetReadFile(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
        };
        if ok == 0 {
            bail!("{url}: read failed, WinINet error {}", unsafe { GetLastError() });
        }
        if read == 0 {
            break;
        }
        sink.write_all(&buffer[..read as usize])?;
        done += read as u64;
        progress(done, total);
    }
    sink.flush()?;
    Ok(())
}

fn query_number(request: &InternetHandle, level: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut len = 4u32;
    let ok = unsafe {
        HttpQueryInfoW(
            request.0,
            level | HTTP_QUERY_FLAG_NUMBER,
            (&mut value as *mut u32).cast(),
            &mut len,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(value)
}

fn log_line(message: &str) {
    let directory = update_directory();
    let _ = fs::create_dir_all(&directory);
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("update_apply.log"))
    {
        let _ = writeln!(file, "{} {message}", now_secs());
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn compares_versions() {
        assert!(is_newer("1.0.4", "1.0.3"));
        assert!(is_newer("v1.1", "1.0.9"));
        assert!(is_newer("1.0.10", "1.0.9"));
        assert!(!is_newer("1.0.3", "1.0.3"));
        assert!(!is_newer("1.0.3", "1.0.3.0"));
        assert!(!is_newer("1.0.2", "1.0.3"));
        assert_eq!(normalize_version("v1.2.3-beta").as_deref(), Some("1.2.3"));
        assert_eq!(normalize_version("FastCopy 1.0.4").as_deref(), Some("1.0.4"));
    }

    #[test]
    fn parses_release_and_prefers_fastcopy_package() {
        let json = br#"{"tag_name":"1.0.4","assets":[
            {"name":"notes.zip","browser_download_url":"https://x/notes.zip","size":10},
            {"name":"fastcopy_1.0.4.7z","browser_download_url":"https://x/fastcopy_1.0.4.7z","size":1234}
        ]}"#;
        let info = parse_release(json, "1.0.3", &crate::i18n::ZH).unwrap();
        assert!(info.has_update);
        assert_eq!(info.version, "1.0.4");
        assert_eq!(info.asset_name, "fastcopy_1.0.4.7z");
        assert_eq!(info.size, 1234);
        let none = br#"{"tag_name":"1.0.4","assets":[]}"#;
        assert!(parse_release(none, "1.0.3", &crate::i18n::ZH).is_err());
    }

    #[test]
    #[ignore = "needs network access to GitHub"]
    fn checks_and_downloads_latest_release_online() {
        let info = check_latest(&crate::i18n::ZH, true).expect("check latest release");
        println!("{} -> {} {} {}", info.current, info.version, info.asset_name, info.size);
        let progress = DownloadProgress::default();
        let archive = download(&info, true, &progress, &AtomicBool::new(false)).expect("download");
        assert_eq!(fs::metadata(&archive).unwrap().len(), info.size);
    }

    #[test]
    fn mirror_order_depends_on_purpose() {
        let api = mirror_urls(REPO_API, false, true);
        assert_eq!(api[0], REPO_API);
        let download = mirror_urls("https://github.com/a/b.7z", true, true);
        assert!(download[0].starts_with("https://ghfast.top/"));
        assert_eq!(download.last().unwrap(), "https://github.com/a/b.7z");
        let plain = mirror_urls("https://github.com/a/b.7z", true, false);
        assert_eq!(plain[0], "https://github.com/a/b.7z");
    }

    #[test]
    fn applies_zip_package_over_locked_target() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(MAIN_EXE), b"new-exe").unwrap();
        fs::write(source.join("fastcopy_shell.dll"), b"new-dll").unwrap();
        let archive = root.path().join("fastcopy_9.9.9.zip");
        run_tool(
            Command::new(
                PathBuf::from(env::var_os("SystemRoot").unwrap())
                    .join("System32")
                    .join("tar.exe"),
            )
            .arg("-a")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&source)
            .arg(MAIN_EXE)
            .arg("fastcopy_shell.dll"),
        )
        .unwrap();
        let target = root.path().join("install");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join(MAIN_EXE), b"old-exe").unwrap();
        fs::write(target.join("fastcopy_shell.dll"), b"old-dll").unwrap();
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x4)
            .open(target.join("fastcopy_shell.dll"))
            .unwrap();
        let extract = root.path().join("extract");
        fs::create_dir_all(&extract).unwrap();
        extract_archive(&archive, &extract, &crate::i18n::ZH).unwrap();
        let payload = find_payload(&extract).unwrap();
        for name in [MAIN_EXE, "fastcopy_shell.dll"] {
            replace_file(&payload.join(name), &target.join(name)).unwrap();
        }
        drop(locked);
        assert_eq!(fs::read(target.join(MAIN_EXE)).unwrap(), b"new-exe");
        assert_eq!(fs::read(target.join("fastcopy_shell.dll")).unwrap(), b"new-dll");
        let stale = fs::read_dir(&target)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".old"))
            .count();
        assert_eq!(stale, 1);
    }
}
