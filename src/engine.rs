use crate::i18n::Strings;
use crate::model::{
    ConflictPolicy, DeleteMode, LinkPolicy, OperationKind, RetryItem, TaskRequest,
};
use anyhow::{Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use ignore::WalkBuilder;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use walkdir::WalkDir;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, ERROR_LOCK_VIOLATION, ERROR_MORE_DATA,
    ERROR_NOT_SUPPORTED, ERROR_OPERATION_ABORTED, ERROR_PATH_NOT_FOUND, ERROR_REQUEST_ABORTED,
    ERROR_SHARING_VIOLATION, FILETIME, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, COPY_FILE_ALLOW_DECRYPTED_DESTINATION, COPY_FILE_FAIL_IF_EXISTS,
    COPY_FILE_NO_BUFFERING, CREATE_NEW, CopyFileExW, CreateFileW, CreateHardLinkW,
    CreateSymbolicLinkW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_SPARSE_FILE, FILE_ATTRIBUTE_TAG_INFO,
    FILE_BEGIN, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_SEQUENTIAL_SCAN,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileAttributeTagInfo, GetFileAttributesW, GetFileInformationByHandle,
    GetFileInformationByHandleEx, GetFileSizeEx, GetFileTime, INVALID_FILE_ATTRIBUTES,
    OPEN_EXISTING, PROGRESS_CANCEL, PROGRESS_CONTINUE, ReadFile, SetEndOfFile, SetFileAttributesW,
    SetFilePointerEx, SetFileTime, SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE,
    SYMBOLIC_LINK_FLAG_DIRECTORY, WriteFile,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES, FSCTL_SET_SPARSE,
};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

const LARGE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const SCAN_EMIT_INTERVAL: Duration = Duration::from_millis(80);
const MTIME_SLACK: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum EngineEvent {
    Scanning {
        total_bytes: u64,
        total_items: u64,
        current: PathBuf,
    },
    Started {
        total_bytes: u64,
        total_items: u64,
    },
    Current(PathBuf),
    BytesDone(u64),
    ItemsDone(u64),
    Failed {
        item: RetryItem,
        message: String,
    },
    Error(String),
    Finished {
        cancelled: bool,
        error_count: usize,
        skip_count: usize,
    },
}

pub struct EngineHandle {
    receiver: Receiver<EngineEvent>,
    control: Arc<Control>,
}

impl EngineHandle {
    pub fn try_recv(&self) -> Option<EngineEvent> {
        self.receiver.try_recv().ok()
    }

    pub fn recv(&self) -> Option<EngineEvent> {
        self.receiver.recv().ok()
    }

    pub fn pause(&self) {
        self.control.paused.store(true, Ordering::Release);
    }

    pub fn resume(&self) {
        self.control.paused.store(false, Ordering::Release);
        self.control.pause_changed.notify_all();
    }

    pub fn cancel(&self) {
        self.control.cancelled.store(true, Ordering::Release);
        self.control.pause_changed.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        self.control.paused.load(Ordering::Acquire)
    }
}

struct Control {
    paused: AtomicBool,
    cancelled: AtomicBool,
    pause_lock: Mutex<()>,
    pause_changed: Condvar,
}

impl Control {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            pause_lock: Mutex::new(()),
            pause_changed: Condvar::new(),
        }
    }

    fn wait(&self) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        if self.paused.load(Ordering::Acquire) {
            let mut guard = self.pause_lock.lock().expect("pause mutex poisoned");
            while self.paused.load(Ordering::Acquire) && !self.cancelled.load(Ordering::Acquire) {
                guard = self
                    .pause_changed
                    .wait(guard)
                    .expect("pause mutex poisoned");
            }
        }
        !self.cancelled.load(Ordering::Acquire)
    }
}

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;

#[derive(Debug, Clone)]
struct FileWork {
    source: PathBuf,
    target: PathBuf,
    size: u64,
    delete_source: bool,
}

#[derive(Debug, Clone)]
struct LinkJob {
    source: PathBuf,
    dest: PathBuf,
    kind: LinkJobKind,
    delete_source: bool,
}

#[derive(Debug, Clone)]
enum LinkJobKind {
    HardLink { existing: PathBuf },
    Symlink { target: PathBuf, is_dir: bool },
    Junction { target: PathBuf },
}

enum EntryClass {
    Normal,
    HardLink { id: (u32, u64) },
    Reparse {
        is_dir: bool,
        is_junction: bool,
        target: Option<PathBuf>,
    },
}

#[derive(Debug)]
struct RootPlan {
    source: PathBuf,
    target: PathBuf,
    directories: Vec<PathBuf>,
    files: Vec<FileWork>,
    links: Vec<LinkJob>,
    bytes: u64,
    items: u64,
}

struct ScanEmitter<'a> {
    sender: &'a Sender<EngineEvent>,
    last: Instant,
    bytes: u64,
    items: u64,
}

impl<'a> ScanEmitter<'a> {
    fn new(sender: &'a Sender<EngineEvent>) -> Self {
        Self {
            sender,
            last: Instant::now() - SCAN_EMIT_INTERVAL,
            bytes: 0,
            items: 0,
        }
    }

    fn add(&mut self, bytes: u64, items: u64, current: &Path, force: bool) -> Result<()> {
        self.bytes += bytes;
        self.items += items;
        if force || self.last.elapsed() >= SCAN_EMIT_INTERVAL {
            self.sender.send(EngineEvent::Scanning {
                total_bytes: self.bytes,
                total_items: self.items,
                current: current.to_path_buf(),
            })?;
            self.last = Instant::now();
        }
        Ok(())
    }
}

fn strings(request: &TaskRequest) -> &'static Strings {
    request.settings.language.strings()
}

pub fn start(request: TaskRequest) -> EngineHandle {
    let (sender, receiver) = unbounded();
    let control = Arc::new(Control::new());
    let worker_control = Arc::clone(&control);
    thread::spawn(move || {
        if let Err(error) = run_task(&request, &sender, &worker_control) {
            let _ = sender.send(EngineEvent::Error(format!("{error:#}")));
            let _ = sender.send(EngineEvent::Finished {
                cancelled: worker_control.cancelled.load(Ordering::Acquire),
                error_count: 1,
                skip_count: 0,
            });
        }
    });
    EngineHandle { receiver, control }
}

fn run_task(
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<()> {
    if matches!(
        request.kind,
        OperationKind::CopyAsSymlink | OperationKind::CopyAsHardlink
    ) {
        return run_create_links(request, sender, control);
    }
    if !request.retry_items.is_empty() {
        return run_retry(request, sender, control);
    }
    validate_request(request)?;
    if request.kind == OperationKind::Delete {
        return run_delete(request, sender, control);
    }

    let t = strings(request);
    let plans = build_plans(request, sender, control)?;
    if control.cancelled.load(Ordering::Acquire) {
        sender.send(EngineEvent::Finished {
            cancelled: true,
            error_count: 0,
            skip_count: 0,
        })?;
        return Ok(());
    }
    let total_bytes = plans.iter().map(|plan| plan.bytes).sum();
    let total_items = plans.iter().map(|plan| plan.items).sum();
    sender.send(EngineEvent::Started {
        total_bytes,
        total_items,
    })?;

    let mut error_count = 0;
    let mut skip_count = 0;
    let mut remaining_plans = Vec::new();
    for plan in plans {
        if !control.wait() {
            break;
        }
        if request.kind == OperationKind::Move
            && !plan.target.exists()
            && same_volume(&plan.source, &plan.target)
        {
            sender.send(EngineEvent::Current(plan.source.clone()))?;
            match fs::rename(&plan.source, &plan.target) {
                Ok(()) => {
                    sender.send(EngineEvent::BytesDone(plan.bytes))?;
                    sender.send(EngineEvent::ItemsDone(plan.items))?;
                    continue;
                }
                Err(error) => {
                    sender.send(EngineEvent::Error(t.move_fallback(&plan.source, &error)))?;
                }
            }
        }
        remaining_plans.push(plan);
    }

    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut links = Vec::new();
    for plan in &remaining_plans {
        directories.extend(plan.directories.iter().cloned());
        files.extend(plan.files.iter().cloned());
        links.extend(plan.links.iter().cloned());
    }
    directories.sort_by_key(|path| path.components().count());
    directories.dedup();
    for directory in &directories {
        if !control.wait() {
            break;
        }
        match fs::create_dir_all(directory) {
            Ok(()) => sender.send(EngineEvent::ItemsDone(1))?,
            Err(error) => {
                error_count += 1;
                sender.send(EngineEvent::Error(t.cannot_create_dir(directory, &error)))?;
            }
        }
    }

    let (file_errors, file_skips) = run_file_workers(files, request, sender, control);
    error_count += file_errors;
    skip_count += file_skips;
    error_count += create_link_jobs(links, request, sender, control);

    if request.kind == OperationKind::Move && !control.cancelled.load(Ordering::Acquire) {
        let mut source_directories: Vec<PathBuf> = remaining_plans
            .iter()
            .flat_map(|plan| {
                WalkDir::new(&plan.source)
                    .contents_first(true)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_dir())
                    .map(|entry| entry.into_path())
                    .collect::<Vec<_>>()
            })
            .collect();
        source_directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for directory in source_directories {
            let _ = fs::remove_dir(&directory);
        }
    }

    sender.send(EngineEvent::Finished {
        cancelled: control.cancelled.load(Ordering::Acquire),
        error_count,
        skip_count,
    })?;
    Ok(())
}

fn run_create_links(
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<()> {
    let t = strings(request);
    let as_symlink = request.kind == OperationKind::CopyAsSymlink;
    let jobs = if request.retry_items.is_empty() {
        validate_request(request)?;
        let dest_root = request.destination.as_ref().unwrap();
        let mut jobs = Vec::new();
        let mut taken = HashSet::new();
        for source in &request.sources {
            let Some(name) = source.file_name() else {
                continue;
            };
            let target = numbered_link_path(
                &dest_root.join(name),
                is_directory_nofollow(source),
                &taken,
            );
            taken.insert(target.clone());
            jobs.push((source.clone(), target));
        }
        jobs
    } else {
        request
            .retry_items
            .iter()
            .filter_map(|item| Some((item.source.clone(), item.target.clone()?)))
            .collect()
    };
    if jobs.is_empty() {
        sender.send(EngineEvent::Started {
            total_bytes: 0,
            total_items: 0,
        })?;
        sender.send(EngineEvent::Finished {
            cancelled: control.cancelled.load(Ordering::Acquire),
            error_count: 0,
            skip_count: 0,
        })?;
        return Ok(());
    }

    let mut total_items = 0u64;
    for (source, _) in &jobs {
        if !control.wait() {
            sender.send(EngineEvent::Finished {
                cancelled: true,
                error_count: 0,
                skip_count: 0,
            })?;
            return Ok(());
        }
        total_items += if as_symlink {
            1
        } else {
            count_hardlink_items(source)
        };
        sender.send(EngineEvent::Scanning {
            total_bytes: 0,
            total_items,
            current: source.clone(),
        })?;
    }
    sender.send(EngineEvent::Started {
        total_bytes: 0,
        total_items,
    })?;

    let mut error_count = 0;
    for (source, dest) in jobs {
        if !control.wait() {
            break;
        }
        sender.send(EngineEvent::Current(source.clone()))?;
        let result = if as_symlink {
            paste_as_symlink(&source, &dest, sender)
        } else {
            paste_as_hardlink(&source, &dest, request, sender, control)
        };
        match result {
            Ok(errors) => error_count += errors,
            Err(error) => {
                error_count += 1;
                send_failed(
                    sender,
                    RetryItem {
                        source: source.clone(),
                        target: Some(dest.clone()),
                        delete_source: false,
                    },
                    t.cannot_create_link(&dest, &error),
                );
            }
        }
    }
    sender.send(EngineEvent::Finished {
        cancelled: control.cancelled.load(Ordering::Acquire),
        error_count,
        skip_count: 0,
    })?;
    Ok(())
}

fn numbered_link_path(path: &Path, as_directory: bool, taken: &HashSet<PathBuf>) -> PathBuf {
    let available = |candidate: &Path| !path_exists(candidate) && !taken.contains(candidate);
    if available(path) {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let (base, suffix) = if as_directory {
        (file_name.to_owned(), String::new())
    } else if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(file_name);
        (stem.to_owned(), format!(".{extension}"))
    } else {
        (file_name.to_owned(), String::new())
    };
    for index in 2..=u32::MAX {
        let candidate = parent.join(format!("{base} {index}{suffix}"));
        if available(&candidate) {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn paste_as_symlink(source: &Path, dest: &Path, sender: &Sender<EngineEvent>) -> Result<usize> {
    let is_dir = is_directory_nofollow(source);
    let target = absolute_link_target(source);
    create_symbolic_link(dest, &target, is_dir)?;
    sender.send(EngineEvent::ItemsDone(1))?;
    Ok(0)
}

fn paste_as_hardlink(
    source: &Path,
    dest: &Path,
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<usize> {
    let t = strings(request);
    if is_directory_nofollow(source) && is_reparse_path(source) {
        bail!("cannot hardlink a reparse directory");
    }
    if !is_directory_nofollow(source) {
        if !same_volume(source, dest) {
            bail!("CreateHardLinkW requires the same volume");
        }
        create_hard_link(dest, source)?;
        sender.send(EngineEvent::ItemsDone(1))?;
        return Ok(0);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if path_exists(dest) {
        if !is_directory_nofollow(dest) {
            fs::remove_file(dest)?;
            fs::create_dir_all(dest)?;
        }
    } else {
        fs::create_dir_all(dest)?;
    }
    sender.send(EngineEvent::ItemsDone(1))?;
    let empty_taken = HashSet::new();
    let mut errors = 0;
    let walker = WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0 || !(entry.file_type().is_dir() && is_reparse_path(entry.path()))
        });
    for entry in walker.filter_map(Result::ok) {
        if !control.wait() {
            break;
        }
        if entry.depth() == 0 {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(source) else {
            continue;
        };
        let mut dest_path = dest.join(relative);
        sender.send(EngineEvent::Current(entry.path().to_path_buf()))?;
        if entry.file_type().is_dir() {
            if let Err(error) = fs::create_dir_all(&dest_path) {
                errors += 1;
                send_failed(
                    sender,
                    RetryItem {
                        source: entry.path().to_path_buf(),
                        target: Some(dest_path),
                        delete_source: false,
                    },
                    t.cannot_create_dir(entry.path(), &error),
                );
                continue;
            }
            sender.send(EngineEvent::ItemsDone(1))?;
            continue;
        }
        dest_path = numbered_link_path(&dest_path, false, &empty_taken);
        if !same_volume(entry.path(), &dest_path) {
            errors += 1;
            send_failed(
                sender,
                RetryItem {
                    source: entry.path().to_path_buf(),
                    target: Some(dest_path.clone()),
                    delete_source: false,
                },
                t.cannot_create_link(&dest_path, &"CreateHardLinkW requires the same volume"),
            );
            continue;
        }
        match create_hard_link(&dest_path, entry.path()) {
            Ok(()) => {
                sender.send(EngineEvent::ItemsDone(1))?;
            }
            Err(error) => {
                errors += 1;
                send_failed(
                    sender,
                    RetryItem {
                        source: entry.path().to_path_buf(),
                        target: Some(dest_path.clone()),
                        delete_source: false,
                    },
                    t.cannot_create_link(&dest_path, &error),
                );
            }
        }
    }
    Ok(errors)
}

fn count_hardlink_items(source: &Path) -> u64 {
    if !is_directory_nofollow(source) || is_reparse_path(source) {
        return 1;
    }
    WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0 || !(entry.file_type().is_dir() && is_reparse_path(entry.path()))
        })
        .filter_map(Result::ok)
        .count()
        .max(1) as u64
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn is_directory_nofollow(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0)
        .unwrap_or(false)
}

fn is_reparse_path(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

fn absolute_link_target(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    });
    strip_extended_prefix(canonical)
}

fn strip_extended_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
        path
    }
}

fn run_retry(
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<()> {
    let t = strings(request);
    if request.kind == OperationKind::Delete {
        let mut delete_request = request.clone();
        delete_request.sources = request
            .retry_items
            .iter()
            .map(|item| item.source.clone())
            .collect();
        delete_request.retry_items.clear();
        return run_delete(&delete_request, sender, control);
    }
    let mut files = Vec::new();
    let mut bytes = 0;
    for item in &request.retry_items {
        let Some(target) = &item.target else {
            continue;
        };
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let size = fs::metadata(&item.source)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        bytes += size;
        files.push(FileWork {
            source: item.source.clone(),
            target: target.clone(),
            size,
            delete_source: item.delete_source,
        });
    }
    sender.send(EngineEvent::Started {
        total_bytes: bytes,
        total_items: files.len() as u64,
    })?;
    if files.is_empty() {
        sender.send(EngineEvent::Error(t.no_sources().to_owned()))?;
        sender.send(EngineEvent::Finished {
            cancelled: false,
            error_count: 1,
            skip_count: 0,
        })?;
        return Ok(());
    }
    let (error_count, skip_count) = run_file_workers(files, request, sender, control);
    sender.send(EngineEvent::Finished {
        cancelled: control.cancelled.load(Ordering::Acquire),
        error_count,
        skip_count,
    })?;
    Ok(())
}

fn validate_request(request: &TaskRequest) -> Result<()> {
    let t = strings(request);
    if request.sources.is_empty() {
        bail!("{}", t.no_sources());
    }
    for source in &request.sources {
        if !source.exists() {
            bail!("{}", t.source_missing(source));
        }
    }
    if request.kind != OperationKind::Delete {
        let destination = request
            .destination
            .as_ref()
            .ok_or_else(|| anyhow!("{}", t.no_destination()))?;
        fs::create_dir_all(destination)
            .map_err(|error| anyhow!("{}", t.cannot_create_destination(destination, &error)))?;
        let destination = destination
            .canonicalize()
            .unwrap_or_else(|_| destination.clone());
        for source in &request.sources {
            if source.is_dir() {
                let source = source.canonicalize().unwrap_or_else(|_| source.clone());
                if destination.starts_with(&source) {
                    bail!("{}", t.destination_inside_source(&source));
                }
            }
        }
    }
    Ok(())
}

fn build_plans(
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<Vec<RootPlan>> {
    let t = strings(request);
    let destination = request.destination.as_ref().expect("validated destination");
    let mut plans = Vec::new();
    let mut scan = ScanEmitter::new(sender);
    for source in &request.sources {
        if !control.wait() {
            break;
        }
        let name = source
            .file_name()
            .ok_or_else(|| anyhow!("{}", t.source_name_unknown(source)))?;
        let mut target = destination.join(name);
        if request.settings.conflict_policy == ConflictPolicy::Rename && target.exists() {
            target = unique_path(&target);
        }

        let mut directories = Vec::new();
        let mut files = Vec::new();
        let mut links = Vec::new();
        let mut hardlinks = HashMap::new();
        let mut skip_reparse = Vec::new();
        let mut bytes = 0;
        let mut items = 0;
        let delete_source = request.kind == OperationKind::Move;
        let follow = request.settings.link_policy == LinkPolicy::Follow;
        if source.is_dir() {
            directories.push(target.clone());
            scan.add(0, 1, source, false)?;
            items += 1;
            for entry in walk_copy_source(source, request) {
                if !control.wait() {
                    break;
                }
                let (path, is_dir) =
                    entry.map_err(|error| anyhow!("{}", t.scan_failed(source, &error)))?;
                if path == *source {
                    continue;
                }
                if skip_reparse
                    .iter()
                    .any(|prefix: &PathBuf| path.starts_with(prefix))
                {
                    continue;
                }
                if !follow {
                    if let EntryClass::Reparse { is_dir: true, .. } = classify_path(&path) {
                        skip_reparse.push(path.clone());
                    }
                }
                let relative = path.strip_prefix(source)?;
                let entry_target = target.join(relative);
                plan_entry(
                    request.settings.link_policy,
                    &path,
                    is_dir,
                    entry_target,
                    delete_source,
                    &mut hardlinks,
                    &mut directories,
                    &mut files,
                    &mut links,
                    &mut bytes,
                    &mut items,
                    &mut scan,
                )?;
            }
        } else {
            let entry_target = target.clone();
            plan_entry(
                request.settings.link_policy,
                source,
                source.is_dir(),
                entry_target,
                delete_source,
                &mut hardlinks,
                &mut directories,
                &mut files,
                &mut links,
                &mut bytes,
                &mut items,
                &mut scan,
            )?;
        }
        plans.push(RootPlan {
            source: source.clone(),
            target,
            directories,
            files,
            links,
            bytes,
            items,
        });
    }
    scan.add(0, 0, destination, true)?;
    Ok(plans)
}

fn run_file_workers(
    files: Vec<FileWork>,
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> (usize, usize) {
    let mut small = Vec::new();
    let mut large = Vec::new();
    for file in files {
        if file.size >= LARGE_FILE_BYTES {
            large.push(file);
        } else {
            small.push(file);
        }
    }
    let errors = Arc::new(AtomicUsize::new(0));
    let skips = Arc::new(AtomicUsize::new(0));
    thread::scope(|scope| {
        if !small.is_empty() {
            spawn_copy_workers(
                scope,
                small,
                request.settings.worker_count,
                false,
                request,
                sender,
                control,
                &errors,
                &skips,
            );
        }
        if !large.is_empty() {
            spawn_copy_workers(
                scope,
                large,
                1,
                true,
                request,
                sender,
                control,
                &errors,
                &skips,
            );
        }
    });
    (
        errors.load(Ordering::Relaxed),
        skips.load(Ordering::Relaxed),
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_copy_workers<'scope>(
    scope: &'scope thread::Scope<'scope, '_>,
    files: Vec<FileWork>,
    worker_count: usize,
    unbuffered: bool,
    request: &'scope TaskRequest,
    sender: &'scope Sender<EngineEvent>,
    control: &'scope Arc<Control>,
    errors: &'scope Arc<AtomicUsize>,
    skips: &'scope Arc<AtomicUsize>,
) {
    let (work_sender, work_receiver) = unbounded::<FileWork>();
    for work in files {
        if work_sender.send(work).is_err() {
            break;
        }
    }
    drop(work_sender);
    for _ in 0..worker_count.clamp(1, 64) {
        let receiver = work_receiver.clone();
        let event_sender = sender.clone();
        let worker_control = Arc::clone(control);
        let errors = Arc::clone(errors);
        let skips = Arc::clone(skips);
        let conflict_policy = request.settings.conflict_policy;
        let verify_size = request.settings.verify_file_size;
        let skip_unchanged = request.settings.skip_unchanged;
        let t = strings(request);
        scope.spawn(move || {
            while let Ok(work) = receiver.recv() {
                if !worker_control.wait() {
                    break;
                }
                let _ = event_sender.send(EngineEvent::Current(work.source.clone()));
                match copy_one(
                    &work,
                    conflict_policy,
                    verify_size,
                    skip_unchanged,
                    unbuffered,
                    t,
                    &event_sender,
                    &worker_control,
                ) {
                    Ok(CopyOutcome::Copied) => {
                        if work.delete_source
                            && let Err(error) = fs::remove_file(&work.source)
                        {
                            errors.fetch_add(1, Ordering::Relaxed);
                            send_failed(
                                &event_sender,
                                RetryItem {
                                    source: work.source.clone(),
                                    target: Some(work.target.clone()),
                                    delete_source: true,
                                },
                                t.copied_but_cannot_delete_source(&work.source, &error),
                            );
                        }
                        let _ = event_sender.send(EngineEvent::ItemsDone(1));
                    }
                    Ok(CopyOutcome::Skipped) => {
                        skips.fetch_add(1, Ordering::Relaxed);
                        let _ = event_sender.send(EngineEvent::BytesDone(work.size));
                        let _ = event_sender.send(EngineEvent::ItemsDone(1));
                    }
                    Err(error) => {
                        if worker_control.cancelled.load(Ordering::Acquire)
                            || is_cancel_error(&error)
                        {
                            break;
                        }
                        errors.fetch_add(1, Ordering::Relaxed);
                        let message = if is_lock_error(&error) {
                            t.file_locked(&work.source)
                        } else {
                            t.file_error(&work.source, &error)
                        };
                        send_failed(
                            &event_sender,
                            RetryItem {
                                source: work.source.clone(),
                                target: Some(work.target.clone()),
                                delete_source: work.delete_source,
                            },
                            message,
                        );
                    }
                }
            }
        });
    }
}

fn send_failed(sender: &Sender<EngineEvent>, item: RetryItem, message: String) {
    let _ = sender.send(EngineEvent::Failed { item, message });
}

enum CopyOutcome {
    Copied,
    Skipped,
}

#[allow(clippy::too_many_arguments)]
fn copy_one(
    work: &FileWork,
    conflict_policy: ConflictPolicy,
    verify_size: bool,
    skip_unchanged: bool,
    unbuffered: bool,
    strings: &Strings,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<CopyOutcome> {
    if !control.wait() {
        bail!("{}", strings.cancelled());
    }
    let mut target = work.target.clone();
    let mut replace_existing = false;
    if target.exists() {
        if skip_unchanged && is_unchanged(&work.source, &target) {
            return Ok(CopyOutcome::Skipped);
        }
        match conflict_policy {
            ConflictPolicy::Skip => return Ok(CopyOutcome::Skipped),
            ConflictPolicy::Rename => target = unique_path(&target),
            ConflictPolicy::Overwrite => replace_existing = true,
        }
    }

    if replace_existing {
        let temporary = next_temporary_path(&target);
        if let Err(error) = copy_file_os(
            &work.source,
            &temporary,
            work.size,
            unbuffered,
            strings,
            sender,
            control,
        ) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        verify_copied_size(&work.source, &temporary, verify_size, strings)?;
        if target.exists() {
            fs::remove_file(&target)?;
        }
        fs::rename(&temporary, &target)?;
    } else if let Err(error) = copy_file_os(
        &work.source,
        &target,
        work.size,
        unbuffered,
        strings,
        sender,
        control,
    ) {
        let _ = fs::remove_file(&target);
        return Err(error);
    } else {
        verify_copied_size(&work.source, &target, verify_size, strings)?;
    }
    Ok(CopyOutcome::Copied)
}

fn is_unchanged(source: &Path, target: &Path) -> bool {
    let Ok(source_meta) = fs::metadata(source) else {
        return false;
    };
    let Ok(target_meta) = fs::metadata(target) else {
        return false;
    };
    if source_meta.len() != target_meta.len() {
        return false;
    }
    let Ok(source_time) = source_meta.modified() else {
        return false;
    };
    let Ok(target_time) = target_meta.modified() else {
        return false;
    };
    target_time + MTIME_SLACK >= source_time
}

struct CopyCallbackState {
    sender: Sender<EngineEvent>,
    control: Arc<Control>,
    last_bytes: AtomicU64,
}

unsafe extern "system" fn copy_progress(
    _total_file_size: i64,
    total_bytes_transferred: i64,
    _stream_size: i64,
    _stream_bytes_transferred: i64,
    _stream_number: u32,
    _callback_reason: u32,
    _source_file: windows_sys::Win32::Foundation::HANDLE,
    _destination_file: windows_sys::Win32::Foundation::HANDLE,
    lpdata: *const core::ffi::c_void,
) -> u32 {
    let Some(state) = (unsafe { lpdata.cast::<CopyCallbackState>().as_ref() }) else {
        return PROGRESS_CONTINUE;
    };
    if !state.control.wait() {
        return PROGRESS_CANCEL;
    }
    let transferred = total_bytes_transferred.max(0) as u64;
    let last = state.last_bytes.swap(transferred, Ordering::Relaxed);
    if transferred > last {
        let _ = state
            .sender
            .send(EngineEvent::BytesDone(transferred - last));
    }
    PROGRESS_CONTINUE
}

fn copy_file_os(
    source: &Path,
    target: &Path,
    expected_size: u64,
    unbuffered: bool,
    strings: &Strings,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<()> {
    if is_sparse_file(source) {
        match copy_sparse_file(source, target, expected_size, strings, sender, control) {
            Ok(()) => return Ok(()),
            Err(error) if os_error_code(&error) == Some(ERROR_PATH_NOT_FOUND) => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                copy_sparse_file(source, target, expected_size, strings, sender, control)?;
                return Ok(());
            }
            Err(error) if sparse_unsupported(&error) => {
                let _ = fs::remove_file(target);
            }
            Err(error) => return Err(error),
        }
    }
    let mut flags = COPY_FILE_ALLOW_DECRYPTED_DESTINATION | COPY_FILE_FAIL_IF_EXISTS;
    if unbuffered {
        flags |= COPY_FILE_NO_BUFFERING;
    }
    match copy_file_ex(
        source,
        target,
        expected_size,
        flags,
        strings,
        sender,
        control,
    ) {
        Ok(()) => Ok(()),
        Err(error) if os_error_code(&error) == Some(ERROR_PATH_NOT_FOUND) => {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            copy_file_ex(
                source,
                target,
                expected_size,
                flags,
                strings,
                sender,
                control,
            )
        }
        Err(error) if unbuffered && !is_cancel_error(&error) && !is_lock_error(&error) => {
            copy_file_ex(
                source,
                target,
                expected_size,
                COPY_FILE_ALLOW_DECRYPTED_DESTINATION | COPY_FILE_FAIL_IF_EXISTS,
                strings,
                sender,
                control,
            )
        }
        Err(error) => Err(error),
    }
}

fn is_sparse_file(path: &Path) -> bool {
    let wide = path_to_wide(path);
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    attributes != INVALID_FILE_ATTRIBUTES && attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0
}

fn sparse_unsupported(error: &anyhow::Error) -> bool {
    matches!(
        os_error_code(error),
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)
    )
}

struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn open_file(
    path: &Path,
    access: u32,
    share: u32,
    disposition: u32,
    flags: u32,
) -> Result<OwnedHandle> {
    let wide = path_to_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            share,
            std::ptr::null(),
            disposition,
            flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into());
    }
    Ok(OwnedHandle(handle))
}

fn last_os_error() -> anyhow::Error {
    std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into()
}

fn copy_sparse_file(
    source: &Path,
    target: &Path,
    expected_size: u64,
    strings: &Strings,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<()> {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let source_handle = open_file(
        source,
        FILE_GENERIC_READ,
        share,
        OPEN_EXISTING,
        FILE_FLAG_SEQUENTIAL_SCAN,
    )?;
    let target_handle = open_file(
        target,
        FILE_GENERIC_WRITE,
        share,
        CREATE_NEW,
        FILE_ATTRIBUTE_NORMAL,
    )?;
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            target_handle.0,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    let mut file_size: i64 = 0;
    let ok = unsafe { GetFileSizeEx(source_handle.0, &mut file_size) };
    if ok == 0 {
        return Err(last_os_error());
    }
    let ok = unsafe { SetFilePointerEx(target_handle.0, file_size, std::ptr::null_mut(), FILE_BEGIN) };
    if ok == 0 {
        return Err(last_os_error());
    }
    let ok = unsafe { SetEndOfFile(target_handle.0) };
    if ok == 0 {
        return Err(last_os_error());
    }
    let copied = copy_allocated_ranges(
        &source_handle,
        &target_handle,
        file_size,
        strings,
        sender,
        control,
    )?;
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut accessed = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut written = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let ok = unsafe {
        GetFileTime(
            source_handle.0,
            &mut created,
            &mut accessed,
            &mut written,
        )
    };
    if ok != 0 {
        let _ = unsafe {
            SetFileTime(
                target_handle.0,
                &created,
                &accessed,
                &written,
            )
        };
    }
    drop(source_handle);
    drop(target_handle);
    let source_wide = path_to_wide(source);
    let attributes = unsafe { GetFileAttributesW(source_wide.as_ptr()) };
    if attributes != INVALID_FILE_ATTRIBUTES {
        let target_wide = path_to_wide(target);
        let _ = unsafe { SetFileAttributesW(target_wide.as_ptr(), attributes) };
    }
    if expected_size > copied {
        sender.send(EngineEvent::BytesDone(expected_size - copied))?;
    }
    Ok(())
}

fn copy_allocated_ranges(
    source: &OwnedHandle,
    target: &OwnedHandle,
    file_size: i64,
    strings: &Strings,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<u64> {
    if file_size <= 0 {
        return Ok(0);
    }
    let mut query = FILE_ALLOCATED_RANGE_BUFFER {
        FileOffset: 0,
        Length: file_size,
    };
    let mut output = vec![FILE_ALLOCATED_RANGE_BUFFER::default(); 16];
    let mut copied = 0u64;
    loop {
        if !control.wait() {
            bail!("{}", strings.cancelled());
        }
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                source.0,
                FSCTL_QUERY_ALLOCATED_RANGES,
                std::ptr::from_ref(&query).cast(),
                mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32,
                output.as_mut_ptr().cast(),
                (output.len() * mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>()) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_HANDLE_EOF {
                break;
            }
            if code != ERROR_MORE_DATA {
                return Err(std::io::Error::from_raw_os_error(code as i32).into());
            }
        }
        let count = (returned as usize) / mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>();
        if count == 0 {
            break;
        }
        for range in output.iter().take(count) {
            copied += copy_file_range_bytes(
                source,
                target,
                range.FileOffset as u64,
                range.Length as u64,
                strings,
                sender,
                control,
            )?;
        }
        if ok != 0 {
            break;
        }
        let last = output[count - 1];
        let next = last.FileOffset.saturating_add(last.Length);
        let end = query.FileOffset.saturating_add(query.Length);
        if next >= end {
            break;
        }
        query.Length = end - next;
        query.FileOffset = next;
    }
    Ok(copied)
}

fn copy_file_range_bytes(
    source: &OwnedHandle,
    target: &OwnedHandle,
    offset: u64,
    length: u64,
    strings: &Strings,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<u64> {
    const CHUNK: usize = 1024 * 1024;
    let mut buffer = vec![0u8; CHUNK];
    let mut remaining = length;
    let mut position = offset;
    let mut copied = 0u64;
    while remaining > 0 {
        if !control.wait() {
            bail!("{}", strings.cancelled());
        }
        let want = remaining.min(CHUNK as u64) as u32;
        let ok = unsafe {
            SetFilePointerEx(source.0, position as i64, std::ptr::null_mut(), FILE_BEGIN)
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                source.0,
                buffer.as_mut_ptr(),
                want,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        if read == 0 {
            break;
        }
        let ok = unsafe {
            SetFilePointerEx(target.0, position as i64, std::ptr::null_mut(), FILE_BEGIN)
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                target.0,
                buffer.as_ptr(),
                read,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        if written != read {
            bail!("short sparse write");
        }
        sender.send(EngineEvent::BytesDone(written as u64))?;
        copied += written as u64;
        position += written as u64;
        remaining -= written as u64;
    }
    Ok(copied)
}

fn copy_file_ex(
    source: &Path,
    target: &Path,
    expected_size: u64,
    flags: u32,
    strings: &Strings,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<()> {
    let source_wide = path_to_wide(source);
    let target_wide = path_to_wide(target);
    let state = CopyCallbackState {
        sender: sender.clone(),
        control: Arc::clone(control),
        last_bytes: AtomicU64::new(0),
    };
    let ok = unsafe {
        CopyFileExW(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            Some(copy_progress),
            std::ptr::from_ref(&state).cast(),
            std::ptr::null_mut(),
            flags,
        )
    };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_REQUEST_ABORTED || code == ERROR_OPERATION_ABORTED {
            bail!("{}", strings.cancelled());
        }
        return Err(std::io::Error::from_raw_os_error(code as i32).into());
    }
    let copied = state.last_bytes.load(Ordering::Relaxed);
    if expected_size > copied {
        sender.send(EngineEvent::BytesDone(expected_size - copied))?;
    }
    Ok(())
}

fn verify_copied_size(
    source: &Path,
    target: &Path,
    verify_size: bool,
    strings: &Strings,
) -> Result<()> {
    if !verify_size {
        return Ok(());
    }
    let source_len = fs::metadata(source)?.len();
    let target_len = fs::metadata(target)?.len();
    if source_len != target_len {
        bail!("{}", strings.verify_size_failed());
    }
    Ok(())
}

fn os_error_code(error: &anyhow::Error) -> Option<u32> {
    error
        .downcast_ref::<std::io::Error>()
        .and_then(|io_error| io_error.raw_os_error())
        .map(|code| code as u32)
}

fn is_lock_error(error: &anyhow::Error) -> bool {
    matches!(
        os_error_code(error),
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    )
}

fn is_cancel_error(error: &anyhow::Error) -> bool {
    os_error_code(error) == Some(ERROR_REQUEST_ABORTED)
        || os_error_code(error) == Some(ERROR_OPERATION_ABORTED)
        || error.to_string().contains("cancelled")
        || error.to_string().contains("已取消")
}

fn next_temporary_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    loop {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{name}.fastcopy-part-{}-{sequence}",
            std::process::id()
        ));
        if !temporary.exists() {
            return temporary;
        }
    }
}

fn path_to_wide_literal(path: &Path) -> Vec<u16> {
    let stripped = strip_extended_prefix(path.to_path_buf());
    stripped
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn path_to_wide(path: &Path) -> Vec<u16> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    if wide.starts_with(&prefix) {
        return wide.into_iter().chain(std::iter::once(0)).collect();
    }
    if wide.len() >= 2 && wide[0] == b'\\' as u16 && wide[1] == b'\\' as u16 {
        let mut prefixed: Vec<u16> = OsStr::new(r"\\?\UNC\").encode_wide().collect();
        prefixed.extend_from_slice(&wide[2..]);
        prefixed.push(0);
        return prefixed;
    }
    let mut prefixed: Vec<u16> = OsStr::new(r"\\?\").encode_wide().collect();
    prefixed.extend(wide);
    prefixed.push(0);
    prefixed
}

fn run_delete(
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> Result<()> {
    let t = strings(request);
    let mut scan = ScanEmitter::new(sender);
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut recycle_sources = Vec::new();
    for source in &request.sources {
        if !control.wait() {
            break;
        }
        if request.settings.delete_mode == DeleteMode::RecycleBin {
            let (bytes, items) = scan_path_totals(source, &mut scan, control)?;
            recycle_sources.push((source.clone(), bytes, items));
            continue;
        }
        if source.is_dir() {
            for entry in WalkDir::new(source).contents_first(true) {
                if !control.wait() {
                    break;
                }
                match entry {
                    Ok(entry) if entry.file_type().is_dir() => {
                        scan.add(0, 1, entry.path(), false)?;
                        directories.push(entry.into_path());
                    }
                    Ok(entry) => {
                        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                        scan.add(size, 1, entry.path(), false)?;
                        files.push((entry.into_path(), size));
                    }
                    Err(error) => {
                        sender.send(EngineEvent::Error(t.scan_failed(source, &error)))?;
                    }
                }
            }
        } else {
            let size = source
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            scan.add(size, 1, source, false)?;
            files.push((source.clone(), size));
        }
    }
    scan.add(0, 0, Path::new(""), true)?;
    if control.cancelled.load(Ordering::Acquire) {
        sender.send(EngineEvent::Finished {
            cancelled: true,
            error_count: 0,
            skip_count: 0,
        })?;
        return Ok(());
    }
    sender.send(EngineEvent::Started {
        total_bytes: scan.bytes,
        total_items: scan.items,
    })?;

    let mut error_count = 0;
    if request.settings.delete_mode == DeleteMode::RecycleBin {
        for (source, bytes, items) in recycle_sources {
            if !control.wait() {
                break;
            }
            sender.send(EngineEvent::Current(source.clone()))?;
            match trash::delete(&source) {
                Ok(()) => {
                    sender.send(EngineEvent::BytesDone(bytes))?;
                    sender.send(EngineEvent::ItemsDone(items))?;
                }
                Err(error) => {
                    error_count += 1;
                    send_failed(
                        sender,
                        RetryItem {
                            source: source.clone(),
                            target: None,
                            delete_source: false,
                        },
                        t.cannot_recycle(&source, &error),
                    );
                }
            }
        }
    } else {
        let (file_sender, file_receiver) = unbounded();
        for file in files {
            let _ = file_sender.send(file);
        }
        drop(file_sender);
        let errors = Arc::new(Mutex::new(0usize));
        thread::scope(|scope| {
            for _ in 0..request.settings.worker_count.clamp(1, 64) {
                let receiver = file_receiver.clone();
                let event_sender = sender.clone();
                let worker_control = Arc::clone(control);
                let errors = Arc::clone(&errors);
                scope.spawn(move || {
                    while let Ok((path, size)) = receiver.recv() {
                        if !worker_control.wait() {
                            break;
                        }
                        let _ = event_sender.send(EngineEvent::Current(path.clone()));
                        match fs::remove_file(&path) {
                            Ok(()) => {
                                let _ = event_sender.send(EngineEvent::BytesDone(size));
                                let _ = event_sender.send(EngineEvent::ItemsDone(1));
                            }
                            Err(error) => {
                                if worker_control.cancelled.load(Ordering::Acquire) {
                                    break;
                                }
                                *errors.lock().expect("error mutex poisoned") += 1;
                                let io_error = anyhow::Error::from(error);
                                let message = if is_lock_error(&io_error) {
                                    t.file_locked(&path)
                                } else {
                                    t.cannot_delete(&path, &io_error)
                                };
                                send_failed(
                                    &event_sender,
                                    RetryItem {
                                        source: path,
                                        target: None,
                                        delete_source: false,
                                    },
                                    message,
                                );
                            }
                        }
                    }
                });
            }
        });
        error_count += *errors.lock().expect("error mutex poisoned");
        directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for directory in directories {
            if !control.wait() {
                break;
            }
            match fs::remove_dir(&directory) {
                Ok(()) => sender.send(EngineEvent::ItemsDone(1))?,
                Err(error) => {
                    error_count += 1;
                    sender.send(EngineEvent::Error(t.cannot_delete_dir(&directory, &error)))?;
                }
            }
        }
    }

    sender.send(EngineEvent::Finished {
        cancelled: control.cancelled.load(Ordering::Acquire),
        error_count,
        skip_count: 0,
    })?;
    Ok(())
}

fn scan_path_totals(
    path: &Path,
    scan: &mut ScanEmitter<'_>,
    control: &Arc<Control>,
) -> Result<(u64, u64)> {
    let start_bytes = scan.bytes;
    let start_items = scan.items;
    if path.is_dir() {
        for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
            if !control.wait() {
                break;
            }
            let size = if entry.file_type().is_file() {
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            } else {
                0
            };
            scan.add(size, 1, entry.path(), false)?;
        }
    } else {
        let size = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        scan.add(size, 1, path, false)?;
    }
    Ok((scan.bytes - start_bytes, scan.items - start_items))
}

pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..=u32::MAX {
        let name = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn walk_copy_source(source: &Path, request: &TaskRequest) -> SourceWalk {
    let follow = request.settings.link_policy == LinkPolicy::Follow;
    if request.kind == OperationKind::Copy && request.settings.use_ignore_file {
        let name = crate::model::sanitize_ignore_file_name(&request.settings.ignore_file_name);
        SourceWalk::Ignore(
            WalkBuilder::new(source)
                .standard_filters(false)
                .hidden(false)
                .follow_links(follow)
                .require_git(false)
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .parents(false)
                .add_custom_ignore_filename(name)
                .build(),
        )
    } else {
        SourceWalk::Dir(WalkDir::new(source).follow_links(follow).into_iter())
    }
}

enum SourceWalk {
    Ignore(ignore::Walk),
    Dir(walkdir::IntoIter),
}

impl Iterator for SourceWalk {
    type Item = Result<(PathBuf, bool), String>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Ignore(walk) => walk.next().map(|result| {
                result
                    .map(|entry| {
                        let is_dir = entry.file_type().map(|file_type| file_type.is_dir()).unwrap_or(false);
                        (entry.into_path(), is_dir)
                    })
                    .map_err(|error| error.to_string())
            }),
            Self::Dir(walk) => walk.next().map(|result| {
                result
                    .map(|entry| {
                        let is_dir = entry.file_type().is_dir();
                        (entry.into_path(), is_dir)
                    })
                    .map_err(|error| error.to_string())
            }),
        }
    }
}

fn classify_path(path: &Path) -> EntryClass {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return EntryClass::Normal;
    };
    let attributes = meta.file_attributes();
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        return EntryClass::Reparse {
            is_dir,
            is_junction: is_dir && reparse_tag(path) == Some(IO_REPARSE_TAG_MOUNT_POINT),
            target: fs::read_link(path).ok(),
        };
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        if let Some((volume, index, nlinks)) = file_index_info(path) {
            if nlinks > 1 {
                return EntryClass::HardLink { id: (volume, index) };
            }
        }
    }
    EntryClass::Normal
}

fn file_index_info(path: &Path) -> Option<(u32, u64, u32)> {
    unsafe {
        let wide = path_to_wide(path);
        let handle = CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info = std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>();
        let ok = GetFileInformationByHandle(handle, &mut info);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
        Some((info.dwVolumeSerialNumber, index, info.nNumberOfLinks))
    }
}

fn reparse_tag(path: &Path) -> Option<u32> {
    unsafe {
        let wide = path_to_wide(path);
        let handle = CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
        let ok = GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        );
        CloseHandle(handle);
        if ok == 0 {
            None
        } else {
            Some(info.ReparseTag)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_entry(
    policy: LinkPolicy,
    path: &Path,
    is_dir: bool,
    entry_target: PathBuf,
    delete_source: bool,
    hardlinks: &mut HashMap<(u32, u64), PathBuf>,
    directories: &mut Vec<PathBuf>,
    files: &mut Vec<FileWork>,
    links: &mut Vec<LinkJob>,
    bytes: &mut u64,
    items: &mut u64,
    scan: &mut ScanEmitter,
) -> Result<()> {
    match (policy, classify_path(path)) {
        (LinkPolicy::Ignore, EntryClass::HardLink { .. } | EntryClass::Reparse { .. }) => {
            return Ok(());
        }
        (
            LinkPolicy::Preserve,
            EntryClass::Reparse {
                is_dir: reparse_dir,
                is_junction,
                target: Some(link_target),
            },
        ) => {
            *items += 1;
            scan.add(0, 1, path, false)?;
            let kind = if is_junction {
                LinkJobKind::Junction {
                    target: link_target,
                }
            } else {
                LinkJobKind::Symlink {
                    target: link_target,
                    is_dir: reparse_dir,
                }
            };
            links.push(LinkJob {
                source: path.to_path_buf(),
                dest: entry_target,
                kind,
                delete_source,
            });
            return Ok(());
        }
        (LinkPolicy::Preserve, EntryClass::HardLink { id }) => {
            if let Some(existing) = hardlinks.get(&id) {
                *items += 1;
                scan.add(0, 1, path, false)?;
                links.push(LinkJob {
                    source: path.to_path_buf(),
                    dest: entry_target,
                    kind: LinkJobKind::HardLink {
                        existing: existing.clone(),
                    },
                    delete_source,
                });
                return Ok(());
            }
            hardlinks.insert(id, entry_target.clone());
        }
        _ => {}
    }
    if is_dir {
        directories.push(entry_target);
        *items += 1;
        scan.add(0, 1, path, false)?;
        return Ok(());
    }
    let size = fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
    *bytes += size;
    *items += 1;
    scan.add(size, 1, path, false)?;
    files.push(FileWork {
        source: path.to_path_buf(),
        target: entry_target,
        size,
        delete_source,
    });
    Ok(())
}

fn create_link_jobs(
    links: Vec<LinkJob>,
    request: &TaskRequest,
    sender: &Sender<EngineEvent>,
    control: &Arc<Control>,
) -> usize {
    let t = strings(request);
    let mut errors = 0;
    for job in links {
        if !control.wait() {
            break;
        }
        let _ = sender.send(EngineEvent::Current(job.dest.clone()));
        let result = match &job.kind {
            LinkJobKind::HardLink { existing } => create_hard_link(&job.dest, existing),
            LinkJobKind::Symlink { target, is_dir } => {
                create_symbolic_link(&job.dest, target, *is_dir)
            }
            LinkJobKind::Junction { target } => create_junction(&job.dest, target),
        };
        match result {
            Ok(()) => {
                if job.delete_source {
                    let _ = if matches!(
                        job.kind,
                        LinkJobKind::Symlink { is_dir: true, .. } | LinkJobKind::Junction { .. }
                    ) {
                        fs::remove_dir(&job.source)
                    } else {
                        fs::remove_file(&job.source)
                    };
                }
                let _ = sender.send(EngineEvent::ItemsDone(1));
            }
            Err(error) => {
                errors += 1;
                let _ = sender.send(EngineEvent::Error(t.cannot_create_link(&job.dest, &error)));
            }
        }
    }
    errors
}

fn create_hard_link(dest: &Path, existing: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        fs::remove_file(dest)?;
    }
    let dest_wide = path_to_wide(dest);
    let existing_wide = path_to_wide(existing);
    let ok = unsafe { CreateHardLinkW(dest_wide.as_ptr(), existing_wide.as_ptr(), std::ptr::null()) };
    if ok == 0 {
        bail!("CreateHardLinkW {}", unsafe { GetLastError() });
    }
    Ok(())
}

fn create_symbolic_link(dest: &Path, target: &Path, is_dir: bool) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        if is_dir {
            let _ = fs::remove_dir(dest);
        } else {
            let _ = fs::remove_file(dest);
        }
    }
    let dest_wide = path_to_wide(dest);
    let target_wide = path_to_wide_literal(target);
    let mut flags = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
    if is_dir {
        flags |= SYMBOLIC_LINK_FLAG_DIRECTORY;
    }
    let ok = unsafe { CreateSymbolicLinkW(dest_wide.as_ptr(), target_wide.as_ptr(), flags) };
    if !ok {
        let flags = if is_dir {
            SYMBOLIC_LINK_FLAG_DIRECTORY
        } else {
            0
        };
        let retry = unsafe { CreateSymbolicLinkW(dest_wide.as_ptr(), target_wide.as_ptr(), flags) };
        if !retry {
            bail!("CreateSymbolicLinkW {}", unsafe { GetLastError() });
        }
    }
    Ok(())
}

fn create_junction(dest: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        let _ = fs::remove_dir(dest);
        let _ = fs::remove_file(dest);
    }
    fs::create_dir(dest)?;
    let (buffer, size) = mount_point_reparse_buffer(target)?;
    unsafe {
        let dest_wide = path_to_wide(dest);
        let handle = CreateFileW(
            dest_wide.as_ptr(),
            FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            let code = GetLastError();
            let _ = fs::remove_dir(dest);
            bail!("CreateFileW junction {code}");
        }
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            handle,
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            size,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
        CloseHandle(handle);
        if ok == 0 {
            let code = GetLastError();
            let _ = fs::remove_dir(dest);
            bail!("FSCTL_SET_REPARSE_POINT {code}");
        }
    }
    Ok(())
}

fn mount_point_reparse_buffer(target: &Path) -> Result<(Vec<u8>, u32)> {
    let (subst, print) = junction_names(target)?;
    let mut path = Vec::new();
    path.extend_from_slice(&subst);
    path.push(0);
    let print_offset = (subst.len() + 1) * 2;
    path.extend_from_slice(&print);
    path.push(0);
    let subst_len = subst.len() * 2;
    let print_len = print.len() * 2;
    let reparse_data_len = 8 + path.len() * 2;
    let mut buf = Vec::with_capacity(8 + reparse_data_len);
    buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buf.extend_from_slice(&(reparse_data_len as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&(subst_len as u16).to_le_bytes());
    buf.extend_from_slice(&(print_offset as u16).to_le_bytes());
    buf.extend_from_slice(&(print_len as u16).to_le_bytes());
    for unit in path {
        buf.extend_from_slice(&unit.to_le_bytes());
    }
    let size = buf.len() as u32;
    Ok((buf, size))
}

fn junction_names(target: &Path) -> Result<(Vec<u16>, Vec<u16>)> {
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()?.join(target)
    };
    let absolute = fs::canonicalize(&absolute).unwrap_or(absolute);
    let mut display = absolute.to_string_lossy().into_owned();
    if let Some(stripped) = display.strip_prefix(r"\\?\") {
        display = stripped.to_owned();
    }
    if let Some(stripped) = display.strip_prefix(r"\??\") {
        display = stripped.to_owned();
    }
    let (nt, print) = if let Some(rest) = display.strip_prefix("UNC\\") {
        (format!(r"\??\UNC\{rest}"), format!(r"\\{rest}"))
    } else if let Some(rest) = display.strip_prefix(r"\\") {
        (format!(r"\??\UNC\{rest}"), format!(r"\\{rest}"))
    } else {
        (format!(r"\??\{display}"), display)
    };
    Ok((wide_chars(&nt), wide_chars(&print)))
}

fn wide_chars(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().collect()
}

fn same_volume(left: &Path, right: &Path) -> bool {
    fn prefix(path: &Path) -> Option<String> {
        path.components().find_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().to_lowercase()),
            _ => None,
        })
    }
    prefix(left) == prefix(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Settings;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    #[test]
    fn different_drive_is_not_same_volume() {
        assert!(!same_volume(Path::new("C:\\a"), Path::new("D:\\b")));
        assert!(same_volume(Path::new("C:\\a"), Path::new("c:\\b")));
    }

    #[test]
    fn unique_path_returns_original_when_available() {
        let path = PathBuf::from("definitely-not-existing-fastcopy-test.txt");
        assert_eq!(unique_path(&path), path);
    }

    #[test]
    fn numbered_link_path_appends_space_index() {
        let root = tempdir().unwrap();
        let file = root.path().join("a.txt");
        fs::write(&file, b"1").unwrap();
        let empty = HashSet::new();
        assert_eq!(
            numbered_link_path(&file, false, &empty),
            root.path().join("a 2.txt")
        );
        fs::write(root.path().join("a 2.txt"), b"2").unwrap();
        assert_eq!(
            numbered_link_path(&file, false, &empty),
            root.path().join("a 3.txt")
        );
        let dir = root.path().join("my.folder");
        fs::create_dir(&dir).unwrap();
        assert_eq!(
            numbered_link_path(&dir, true, &empty),
            root.path().join("my.folder 2")
        );
    }

    #[test]
    fn numbered_link_path_avoids_taken_names() {
        let path = PathBuf::from(r"C:\fastcopy-link-test\a.txt");
        let mut taken = HashSet::new();
        taken.insert(path.clone());
        taken.insert(PathBuf::from(r"C:\fastcopy-link-test\a 2.txt"));
        assert_eq!(
            numbered_link_path(&path, false, &taken),
            PathBuf::from(r"C:\fastcopy-link-test\a 3.txt")
        );
    }

    #[test]
    fn copies_directory_tree() {
        let root = tempdir().unwrap();
        let source = root.path().join("源目录");
        let destination = root.path().join("目标");
        fs::create_dir_all(source.join("子目录")).unwrap();
        fs::write(source.join("子目录").join("文件.txt"), b"fastcopy").unwrap();
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert_eq!(
            fs::read(destination.join("源目录").join("子目录").join("文件.txt")).unwrap(),
            b"fastcopy"
        );
        assert!(source.exists());
    }

    #[test]
    fn copies_multiple_sources() {
        let root = tempdir().unwrap();
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        let destination = root.path().join("目标");
        fs::write(&a, b"A").unwrap();
        fs::write(&b, b"B").unwrap();
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![a, b],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"A");
        assert_eq!(fs::read(destination.join("b.txt")).unwrap(), b"B");
    }

    #[test]
    fn copies_sparse_file_layout() {
        let root = tempdir().unwrap();
        let source = root.path().join("sparse.bin");
        let payload = b"payload!";
        let hole = 2 * 1024 * 1024u64;
        if !write_sparse_file(&source, hole, payload) {
            return;
        }
        assert!(is_sparse_file(&source));
        let destination = root.path().join("目标");
        fs::create_dir_all(&destination).unwrap();
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let copied = destination.join("sparse.bin");
        assert!(is_sparse_file(&copied), "destination should stay sparse");
        assert_eq!(
            fs::metadata(&copied).unwrap().len(),
            hole + payload.len() as u64
        );
        let on_disk = compressed_size(&copied).expect("allocated size");
        assert!(
            on_disk < hole,
            "allocated {on_disk} should be far below logical {hole}"
        );
        let data = fs::read(&copied).unwrap();
        assert_eq!(&data[hole as usize..], payload);
        assert!(data[..hole as usize].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn moves_file() {
        let root = tempdir().unwrap();
        let source = root.path().join("move.txt");
        let destination = root.path().join("目标");
        fs::write(&source, b"move").unwrap();
        let request = TaskRequest {
            kind: OperationKind::Move,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("move.txt")).unwrap(), b"move");
    }

    #[test]
    fn permanently_deletes_tree() {
        let root = tempdir().unwrap();
        let source = root.path().join("delete");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.bin"), [1, 2, 3]).unwrap();
        let mut settings = test_settings();
        settings.delete_mode = DeleteMode::Permanent;
        let request = TaskRequest {
            kind: OperationKind::Delete,
            sources: vec![source.clone()],
            destination: None,
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert!(!source.exists());
    }

    #[test]
    fn skips_unchanged_file() {
        let root = tempdir().unwrap();
        let source = root.path().join("same.txt");
        let destination = root.path().join("目标");
        fs::create_dir_all(&destination).unwrap();
        fs::write(&source, b"same-bytes").unwrap();
        fs::copy(&source, destination.join("same.txt")).unwrap();
        let mut settings = test_settings();
        settings.skip_unchanged = true;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        let (errors, skips, failed) = wait_for_finish(start(request));
        assert_eq!(errors, 0);
        assert_eq!(skips, 1);
        assert!(failed.is_empty());
        assert_eq!(fs::read(destination.join("same.txt")).unwrap(), b"same-bytes");
    }

    #[test]
    fn copy_directory_skips_gitignore_matches() {
        let root = tempdir().unwrap();
        let source = root.path().join("源目录");
        let destination = root.path().join("目标");
        fs::create_dir_all(source.join("keep")).unwrap();
        fs::create_dir_all(source.join("build")).unwrap();
        fs::write(source.join("keep").join("ok.txt"), b"keep").unwrap();
        fs::write(source.join("skip.log"), b"log").unwrap();
        fs::write(source.join("build").join("out.bin"), b"bin").unwrap();
        fs::write(source.join(".gitignore"), "*.log\nbuild/\n").unwrap();
        let mut settings = test_settings();
        settings.use_ignore_file = true;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let copied = destination.join("源目录");
        assert_eq!(fs::read(copied.join("keep").join("ok.txt")).unwrap(), b"keep");
        assert_eq!(fs::read(copied.join(".gitignore")).unwrap(), b"*.log\nbuild/\n");
        assert!(!copied.join("skip.log").exists());
        assert!(!copied.join("build").exists());
    }

    #[test]
    fn copy_single_file_ignores_ignore_setting() {
        let root = tempdir().unwrap();
        let source = root.path().join("skip.log");
        let destination = root.path().join("目标");
        fs::write(&source, b"log").unwrap();
        fs::write(root.path().join(".gitignore"), "*.log\n").unwrap();
        let mut settings = test_settings();
        settings.use_ignore_file = true;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert_eq!(fs::read(destination.join("skip.log")).unwrap(), b"log");
    }

    #[test]
    fn copy_directory_skips_hard_links_by_default() {
        let root = tempdir().unwrap();
        let source = root.path().join("源目录");
        let destination = root.path().join("目标");
        fs::create_dir_all(&source).unwrap();
        let original = source.join("a.txt");
        fs::write(&original, b"data").unwrap();
        fs::hard_link(&original, source.join("b.txt")).unwrap();
        fs::write(source.join("c.txt"), b"plain").unwrap();
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let copied = destination.join("源目录");
        assert_eq!(fs::read(copied.join("c.txt")).unwrap(), b"plain");
        assert!(!copied.join("a.txt").exists());
        assert!(!copied.join("b.txt").exists());
    }

    #[test]
    fn conflict_skip_keeps_existing() {
        let root = tempdir().unwrap();
        let source = root.path().join("file.txt");
        let destination = root.path().join("目标");
        fs::create_dir_all(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("file.txt"), b"old").unwrap();
        let mut settings = test_settings();
        settings.conflict_policy = ConflictPolicy::Skip;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        let (errors, skips, failed) = wait_for_finish(start(request));
        assert_eq!(errors, 0);
        assert_eq!(skips, 1);
        assert!(failed.is_empty());
        assert_eq!(fs::read(destination.join("file.txt")).unwrap(), b"old");
    }

    #[test]
    fn conflict_overwrite_replaces_existing() {
        let root = tempdir().unwrap();
        let source = root.path().join("file.txt");
        let destination = root.path().join("目标");
        fs::create_dir_all(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("file.txt"), b"old").unwrap();
        let mut settings = test_settings();
        settings.conflict_policy = ConflictPolicy::Overwrite;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert_eq!(fs::read(destination.join("file.txt")).unwrap(), b"new");
    }

    #[test]
    fn conflict_rename_keeps_both() {
        let root = tempdir().unwrap();
        let source = root.path().join("file.txt");
        let destination = root.path().join("目标");
        fs::create_dir_all(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("file.txt"), b"old").unwrap();
        let mut settings = test_settings();
        settings.conflict_policy = ConflictPolicy::Rename;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert_eq!(fs::read(destination.join("file.txt")).unwrap(), b"old");
        assert_eq!(fs::read(destination.join("file (1).txt")).unwrap(), b"new");
    }

    #[test]
    fn locked_file_is_skipped_then_retry_succeeds() {
        let root = tempdir().unwrap();
        let source = root.path().join("locked.txt");
        let destination = root.path().join("目标");
        fs::write(&source, b"payload").unwrap();
        let _lock = exclusive_open(&source).expect("exclusive lock");
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        let (errors, _skips, failed) = wait_for_finish(start(request));
        assert_eq!(errors, 1);
        assert_eq!(failed.len(), 1);
        assert!(!destination.join("locked.txt").exists());
        drop(_lock);
        let retry = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: failed,
        };
        assert_eq!(wait_for_task(start(retry)), 0);
        assert_eq!(fs::read(destination.join("locked.txt")).unwrap(), b"payload");
    }

    #[test]
    fn copy_follow_copies_junction_contents() {
        let root = tempdir().unwrap();
        let source = root.path().join("源目录");
        let destination = root.path().join("目标");
        let real = source.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("inside.txt"), b"via-junction").unwrap();
        create_junction(&source.join("link"), &real).unwrap();
        let mut settings = test_settings();
        settings.link_policy = LinkPolicy::Follow;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let copied = destination.join("源目录");
        assert_eq!(fs::read(copied.join("real").join("inside.txt")).unwrap(), b"via-junction");
        assert_eq!(fs::read(copied.join("link").join("inside.txt")).unwrap(), b"via-junction");
        assert_ne!(
            reparse_tag(&copied.join("link")),
            Some(IO_REPARSE_TAG_MOUNT_POINT)
        );
    }

    #[test]
    fn copy_preserve_recreates_junction() {
        let root = tempdir().unwrap();
        let source = root.path().join("源目录");
        let destination = root.path().join("目标");
        let real = source.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("inside.txt"), b"keep").unwrap();
        create_junction(&source.join("link"), &real).unwrap();
        let mut settings = test_settings();
        settings.link_policy = LinkPolicy::Preserve;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let copied = destination.join("源目录");
        assert_eq!(fs::read(copied.join("real").join("inside.txt")).unwrap(), b"keep");
        assert_eq!(
            reparse_tag(&copied.join("link")),
            Some(IO_REPARSE_TAG_MOUNT_POINT)
        );
        assert_eq!(
            fs::read(copied.join("link").join("inside.txt")).unwrap(),
            b"keep"
        );
        let copied_index = file_index_info(&copied.join("real").join("inside.txt"));
        let through_link = file_index_info(&copied.join("link").join("inside.txt"));
        let original = file_index_info(&real.join("inside.txt"));
        assert_eq!(through_link, original);
        assert_ne!(copied_index, original);
    }

    #[test]
    fn copy_preserve_recreates_hard_links() {
        let root = tempdir().unwrap();
        let source = root.path().join("源目录");
        let destination = root.path().join("目标");
        fs::create_dir_all(&source).unwrap();
        let original = source.join("a.txt");
        fs::write(&original, b"shared").unwrap();
        fs::hard_link(&original, source.join("b.txt")).unwrap();
        let mut settings = test_settings();
        settings.link_policy = LinkPolicy::Preserve;
        let request = TaskRequest {
            kind: OperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            settings,
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let copied = destination.join("源目录");
        let a = file_index_info(&copied.join("a.txt")).expect("a.txt");
        let b = file_index_info(&copied.join("b.txt")).expect("b.txt");
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
        assert_eq!(a.2, 2);
        assert_eq!(fs::read(copied.join("b.txt")).unwrap(), b"shared");
    }

    #[test]
    fn copy_as_hardlink_file_shares_index() {
        let root = tempdir().unwrap();
        let source = root.path().join("源.txt");
        let destination = root.path().join("目标");
        fs::write(&source, b"hard").unwrap();
        let request = TaskRequest {
            kind: OperationKind::CopyAsHardlink,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let dest = destination.join("源.txt");
        assert_eq!(fs::read(&dest).unwrap(), b"hard");
        let src_info = file_index_info(&source).expect("source");
        let dest_info = file_index_info(&dest).expect("dest");
        assert_eq!(src_info.0, dest_info.0);
        assert_eq!(src_info.1, dest_info.1);
        assert_eq!(src_info.2, 2);
        assert_eq!(dest_info.2, 2);
    }

    #[test]
    fn copy_as_hardlink_directory_tree() {
        let root = tempdir().unwrap();
        let source = root.path().join("源目录");
        let destination = root.path().join("目标");
        fs::create_dir_all(source.join("子")).unwrap();
        fs::write(source.join("a.txt"), b"root").unwrap();
        fs::write(source.join("子").join("b.txt"), b"nested").unwrap();
        let request = TaskRequest {
            kind: OperationKind::CopyAsHardlink,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let copied = destination.join("源目录");
        assert_eq!(fs::read(copied.join("a.txt")).unwrap(), b"root");
        assert_eq!(fs::read(copied.join("子").join("b.txt")).unwrap(), b"nested");
        let a = file_index_info(&source.join("a.txt")).expect("a");
        let copied_a = file_index_info(&copied.join("a.txt")).expect("copied a");
        assert_eq!(a.0, copied_a.0);
        assert_eq!(a.1, copied_a.1);
        let b = file_index_info(&source.join("子").join("b.txt")).expect("b");
        let copied_b = file_index_info(&copied.join("子").join("b.txt")).expect("copied b");
        assert_eq!(b.1, copied_b.1);
    }

    #[test]
    fn copy_as_symlink_file_when_supported() {
        let root = tempdir().unwrap();
        let source = root.path().join("源.txt");
        let destination = root.path().join("目标");
        fs::write(&source, b"sym").unwrap();
        let probe = root.path().join("probe.lnk");
        if create_symbolic_link(&probe, &source, false).is_err() {
            return;
        }
        let _ = fs::remove_file(&probe);
        let request = TaskRequest {
            kind: OperationKind::CopyAsSymlink,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        let dest = destination.join("源.txt");
        let meta = fs::symlink_metadata(&dest).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(fs::read(&dest).unwrap(), b"sym");
    }

    #[test]
    fn copy_as_hardlink_numbers_existing_name() {
        let root = tempdir().unwrap();
        let source = root.path().join("源.txt");
        let destination = root.path().join("目标");
        fs::create_dir_all(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("源.txt"), b"old").unwrap();
        fs::write(destination.join("源 2.txt"), b"also").unwrap();
        let request = TaskRequest {
            kind: OperationKind::CopyAsHardlink,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            settings: test_settings(),
            retry_items: Vec::new(),
        };
        assert_eq!(wait_for_task(start(request)), 0);
        assert_eq!(fs::read(destination.join("源.txt")).unwrap(), b"old");
        assert_eq!(fs::read(destination.join("源 2.txt")).unwrap(), b"also");
        let numbered = destination.join("源 3.txt");
        assert_eq!(fs::read(&numbered).unwrap(), b"new");
        let src_info = file_index_info(&source).expect("source");
        let dest_info = file_index_info(&numbered).expect("numbered");
        assert_eq!(src_info.0, dest_info.0);
        assert_eq!(src_info.1, dest_info.1);
    }

    fn test_settings() -> Settings {
        Settings {
            worker_count: 2,
            verify_file_size: true,
            ..Settings::default()
        }
    }

    fn wait_for_task(handle: EngineHandle) -> usize {
        wait_for_finish(handle).0
    }

    fn wait_for_finish(handle: EngineHandle) -> (usize, usize, Vec<RetryItem>) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut failed = Vec::new();
        loop {
            while let Some(event) = handle.try_recv() {
                match event {
                    EngineEvent::Failed { item, .. } => failed.push(item),
                    EngineEvent::Finished {
                        error_count,
                        skip_count,
                        ..
                    } => return (error_count, skip_count, failed),
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "task timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn write_sparse_file(path: &Path, hole: u64, payload: &[u8]) -> bool {
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        let Ok(handle) = open_file(
            path,
            FILE_GENERIC_WRITE,
            share,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
        ) else {
            return false;
        };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return false;
        }
        let size = (hole + payload.len() as u64) as i64;
        let ok = unsafe { SetFilePointerEx(handle.0, size, std::ptr::null_mut(), FILE_BEGIN) };
        if ok == 0 {
            return false;
        }
        if unsafe { SetEndOfFile(handle.0) } == 0 {
            return false;
        }
        let ok = unsafe { SetFilePointerEx(handle.0, hole as i64, std::ptr::null_mut(), FILE_BEGIN) };
        if ok == 0 {
            return false;
        }
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                handle.0,
                payload.as_ptr(),
                payload.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        ok != 0 && written == payload.len() as u32
    }

    fn compressed_size(path: &Path) -> Option<u64> {
        let wide = path_to_wide(path);
        let mut high = 0u32;
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low == u32::MAX {
            let code = unsafe { GetLastError() };
            if code != 0 {
                return None;
            }
        }
        Some(((high as u64) << 32) | u64::from(low))
    }

    struct ExclusiveLock(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for ExclusiveLock {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn exclusive_open(path: &Path) -> Result<ExclusiveLock> {
        let wide = path_to_wide(path);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_READ,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            bail!("exclusive open {}", unsafe { GetLastError() });
        }
        Ok(ExclusiveLock(handle))
    }
}
