use crate::windows::explorer_sel;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SizeStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub errors: u64,
}

pub fn scan_size(
    paths: &[PathBuf],
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(&SizeStats, &Path),
) -> SizeStats {
    let mut stats = SizeStats::default();
    let mut last = Instant::now() - Duration::from_millis(80);
    let mut emit = |stats: &SizeStats, path: &Path| {
        if last.elapsed() >= Duration::from_millis(80) {
            on_progress(stats, path);
            last = Instant::now();
        }
    };
    let mut seen = HashSet::new();
    for path in paths {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        let Ok(canonical) = std::path::absolute(path) else {
            stats.errors += 1;
            continue;
        };
        if !seen.insert(explorer_sel::path_key(&canonical)) {
            continue;
        }
        add_path(&canonical, cancelled, &mut stats, &mut emit);
    }
    on_progress(&stats, Path::new(""));
    stats
}

fn add_path(
    path: &Path,
    cancelled: &AtomicBool,
    stats: &mut SizeStats,
    emit: &mut impl FnMut(&SizeStats, &Path),
) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            stats.errors += 1;
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        stats.files += 1;
        emit(stats, path);
        return;
    }
    if metadata.is_dir() {
        stats.dirs += 1;
        emit(stats, path);
        for entry in WalkDir::new(path).follow_links(false).min_depth(1) {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            match entry {
                Ok(entry) if entry.file_type().is_dir() => {
                    stats.dirs += 1;
                    emit(stats, entry.path());
                }
                Ok(entry) if entry.file_type().is_symlink() => {
                    stats.files += 1;
                    emit(stats, entry.path());
                }
                Ok(entry) => {
                    let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                    stats.files += 1;
                    stats.bytes += size;
                    emit(stats, entry.path());
                }
                Err(_) => stats.errors += 1,
            }
        }
        return;
    }
    stats.files += 1;
    stats.bytes += metadata.len();
    emit(stats, path);
}

pub fn path_lines(paths: &[PathBuf], relative: bool) -> String {
    let displayed: Vec<PathBuf> = paths.iter().map(|path| explorer_sel::display_path(path)).collect();
    if displayed.is_empty() {
        return String::new();
    }
    if relative {
        if let Some(base) = common_parent(&displayed) {
            let lines: Vec<String> = displayed
                .iter()
                .map(|path| {
                    path.strip_prefix(&base)
                        .map(|rest| rest.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
                })
                .collect();
            return lines.join("\r\n");
        }
    }
    displayed
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut prefix = paths.first()?.parent()?.to_path_buf();
    for path in &paths[1..] {
        while !path.starts_with(&prefix) {
            prefix = prefix.parent()?.to_path_buf();
        }
    }
    Some(prefix)
}

pub fn format_bytes(bytes: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn path_lines_are_multiline() {
        let text = path_lines(
            &[
                PathBuf::from(r"C:\a\one.txt"),
                PathBuf::from(r"C:\a\two.txt"),
            ],
            false,
        );
        assert_eq!(text, "C:\\a\\one.txt\r\nC:\\a\\two.txt");
        let relative = path_lines(
            &[
                PathBuf::from(r"C:\a\one.txt"),
                PathBuf::from(r"C:\a\two.txt"),
            ],
            true,
        );
        assert_eq!(relative, "one.txt\r\ntwo.txt");
    }

    #[test]
    fn scans_files_and_dirs() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("sub")).unwrap();
        fs::write(root.path().join("a.txt"), b"hello").unwrap();
        fs::write(root.path().join("sub").join("b.txt"), b"xy").unwrap();
        let stats = scan_size(
            &[root.path().to_path_buf()],
            &AtomicBool::new(false),
            |_, _| {},
        );
        assert_eq!(stats.files, 2);
        assert_eq!(stats.dirs, 2);
        assert_eq!(stats.bytes, 7);
        assert_eq!(stats.errors, 0);
        let mixed_case = root.path().join("A.TXT");
        let stats = scan_size(
            &[
                root.path().join("a.txt"),
                mixed_case,
                root.path().join("a.txt"),
            ],
            &AtomicBool::new(false),
            |_, _| {},
        );
        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, 5);
    }
}
