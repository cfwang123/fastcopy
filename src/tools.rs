use crate::windows::explorer_sel;
use anyhow::{Result, bail};
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

#[derive(Debug, Clone)]
pub struct RenameOptions {
    pub old_pattern: String,
    pub new_pattern: String,
    pub match_case: bool,
    pub use_regex: bool,
    pub ignore_extension: bool,
}

impl Default for RenameOptions {
    fn default() -> Self {
        Self {
            old_pattern: "%1".to_owned(),
            new_pattern: "%1".to_owned(),
            match_case: false,
            use_regex: false,
            ignore_extension: false,
        }
    }
}

pub fn common_rename_pattern(names: &[String], ignore_extension: bool) -> (String, String) {
    if names.is_empty() {
        return ("%1".to_owned(), "%1".to_owned());
    }
    let working: Vec<&str> = names
        .iter()
        .map(|name| {
            if ignore_extension {
                split_extension(name).0
            } else {
                name.as_str()
            }
        })
        .collect();
    let pattern = infer_capture_pattern(&working);
    (pattern.clone(), pattern)
}

fn infer_capture_pattern(names: &[&str]) -> String {
    if names.len() < 2 {
        return "%1".to_owned();
    }
    if names.iter().all(|name| *name == names[0]) {
        return "%1".to_owned();
    }
    let prefix = common_char_prefix(names);
    let prefix_len = prefix.chars().count();
    let suffix = common_char_suffix(names, prefix_len);
    if prefix.is_empty() && suffix.is_empty() {
        return "%1".to_owned();
    }
    format!("{prefix}%1{suffix}")
}

fn common_char_prefix(names: &[&str]) -> String {
    let mut prefix: Vec<char> = names[0].chars().collect();
    for name in &names[1..] {
        let mut len = 0;
        for (left, right) in prefix.iter().copied().zip(name.chars()) {
            if left != right {
                break;
            }
            len += 1;
        }
        prefix.truncate(len);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.into_iter().collect()
}

fn common_char_suffix(names: &[&str], prefix_len: usize) -> String {
    let lists: Vec<Vec<char>> = names.iter().map(|name| name.chars().collect()).collect();
    let max_len = lists
        .iter()
        .map(|chars| chars.len().saturating_sub(prefix_len))
        .min()
        .unwrap_or(0);
    let mut suffix_len = 0;
    while suffix_len < max_len {
        let expected = lists[0][lists[0].len() - 1 - suffix_len];
        if lists
            .iter()
            .all(|chars| chars[chars.len() - 1 - suffix_len] == expected)
        {
            suffix_len += 1;
        } else {
            break;
        }
    }
    if suffix_len == 0 {
        return String::new();
    }
    lists[0][lists[0].len() - suffix_len..].iter().collect()
}

pub fn expand_new_name(template: &str, slots: &[String], index: u32) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("{nnn}") {
            out.push_str(&format!("{index:03}"));
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("{nn}") {
            out.push_str(&format!("{index:02}"));
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("{n}") {
            out.push_str(&format!("{index}"));
            rest = after;
            continue;
        }
        if rest.starts_with('#') {
            let width = rest.find(|ch| ch != '#').unwrap_or(rest.len());
            let width = width.max(1);
            out.push_str(&format!("{index:0width$}"));
            rest = &rest[width..];
            continue;
        }
        if rest.starts_with('%') {
            let digits: String = rest[1..].chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(slot) = digits.parse::<usize>() {
                    if let Some(value) = slots.get(slot) {
                        out.push_str(value);
                    }
                }
                rest = &rest[1 + digits.len()..];
                continue;
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameKind {
    Ready,
    Unchanged,
    Invalid,
    Conflict,
}

#[derive(Debug, Clone)]
pub struct RenamePlan {
    pub source: PathBuf,
    pub from: String,
    pub to: String,
    pub dest: PathBuf,
    pub kind: RenameKind,
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameItem {
    pub path: PathBuf,
    pub from: String,
}

pub fn split_name_lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
        return vec![String::new()];
    }
    normalized.split('\n').map(|line| line.to_string()).collect()
}

pub fn join_name_lines(lines: &[String]) -> String {
    lines.join("\n")
}

pub fn align_new_name_text(old_text: &str, new_names: &[String]) -> String {
    let mut names = new_names.iter();
    let lines: Vec<String> = split_name_lines(old_text)
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                names.next().cloned().unwrap_or_default()
            }
        })
        .collect();
    join_name_lines(&lines)
}

fn mark_path(used: &mut [bool], items: &[RenameItem], path: &Path) {
    if let Some(index) = items.iter().position(|item| item.path == path) {
        used[index] = true;
    }
}

#[cfg(test)]
pub fn reconcile_old_names(items: &[RenameItem], text: &str) -> Vec<RenameItem> {
    reconcile_selection(items, items, text)
}

pub fn reconcile_selection(
    source_items: &[RenameItem],
    working: &[RenameItem],
    text: &str,
) -> Vec<RenameItem> {
    let lines: Vec<String> = split_name_lines(text)
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect();
    if source_items.is_empty() && working.is_empty() {
        return Vec::new();
    }
    let mut used_working = vec![false; working.len()];
    let mut used_source = vec![false; source_items.len()];
    let mut assigned: Vec<Option<RenameItem>> = vec![None; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        if let Some(item_index) = (0..working.len()).find(|&item_index| {
            !used_working[item_index] && working[item_index].from == *line
        }) {
            used_working[item_index] = true;
            mark_path(&mut used_source, source_items, &working[item_index].path);
            assigned[index] = Some(working[item_index].clone());
        }
    }
    for (index, line) in lines.iter().enumerate() {
        if assigned[index].is_some() {
            continue;
        }
        if let Some(item_index) = (0..source_items.len()).find(|&item_index| {
            !used_source[item_index] && source_items[item_index].from == *line
        }) {
            used_source[item_index] = true;
            assigned[index] = Some(source_items[item_index].clone());
        }
    }
    let leftovers: Vec<RenameItem> = working
        .iter()
        .enumerate()
        .filter(|(index, _)| !used_working[*index])
        .map(|(_, item)| item.clone())
        .collect();
    let mut leftover_index = 0;
    for (index, line) in lines.iter().enumerate() {
        if assigned[index].is_some() {
            continue;
        }
        if leftover_index >= leftovers.len() {
            continue;
        }
        let mut item = leftovers[leftover_index].clone();
        leftover_index += 1;
        item.from = line.clone();
        assigned[index] = Some(item);
    }
    assigned.into_iter().flatten().collect()
}

pub fn name_list_from_text(text: &str, count: usize, fallback: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
    while lines.len() < count {
        let index = lines.len();
        lines.push(
            fallback
                .get(index)
                .cloned()
                .unwrap_or_default(),
        );
    }
    lines.truncate(count);
    lines
}

fn make_plan(
    source: &Path,
    from: String,
    to: String,
    taken: &mut HashSet<String>,
    invalid_pattern: bool,
) -> RenamePlan {
    let parent = source.parent().unwrap_or_else(|| Path::new(""));
    let dest = parent.join(&to);
    let key = to.to_ascii_lowercase();
    let kind = if invalid_pattern || !is_valid_file_name(&to) {
        RenameKind::Invalid
    } else if to == from {
        RenameKind::Unchanged
    } else if dest.exists() && !same_item(source, &dest) {
        RenameKind::Conflict
    } else if !taken.insert(key) {
        RenameKind::Conflict
    } else {
        RenameKind::Ready
    };
    RenamePlan {
        source: source.to_path_buf(),
        from,
        to,
        dest,
        kind,
    }
}

#[cfg(test)]
pub fn plan_renames(paths: &[PathBuf], options: &RenameOptions) -> Vec<RenamePlan> {
    let from_names: Vec<String> = paths.iter().map(|path| file_name_of(path)).collect();
    plan_renames_from(paths, &from_names, options)
}

pub fn plan_renames_from(
    paths: &[PathBuf],
    from_names: &[String],
    options: &RenameOptions,
) -> Vec<RenamePlan> {
    let compiled = compile_old_pattern(options);
    let invalid_pattern = compiled.is_err();
    let mut taken = HashSet::new();
    let mut plans = Vec::with_capacity(paths.len());
    for (index, source) in paths.iter().enumerate() {
        let from = from_names
            .get(index)
            .cloned()
            .unwrap_or_else(|| file_name_of(source));
        let to = match &compiled {
            Ok(regex) => rename_one(&from, regex, options, (index as u32) + 1),
            Err(_) => from.clone(),
        };
        plans.push(make_plan(source, from, to, &mut taken, invalid_pattern));
    }
    plans
}

pub fn plan_renames_to(
    paths: &[PathBuf],
    from_names: &[String],
    to_names: &[String],
) -> Vec<RenamePlan> {
    let mut taken = HashSet::new();
    let mut plans = Vec::with_capacity(paths.len());
    for (index, source) in paths.iter().enumerate() {
        let from = from_names
            .get(index)
            .cloned()
            .unwrap_or_else(|| file_name_of(source));
        let to = to_names.get(index).cloned().unwrap_or_else(|| from.clone());
        plans.push(make_plan(source, from, to, &mut taken, false));
    }
    plans
}

fn compile_old_pattern(options: &RenameOptions) -> Result<regex::Regex, ()> {
    let pattern = if options.use_regex {
        let raw = options.old_pattern.trim();
        if raw.is_empty() || raw == "%1" {
            "^(.*)$".to_owned()
        } else {
            options.old_pattern.clone()
        }
    } else {
        let glob = if options.old_pattern.trim().is_empty() || options.old_pattern.trim() == "%1" {
            "*".to_owned()
        } else {
            apply_old_wildcards(&options.old_pattern)
        };
        glob_to_regex(&glob)
    };
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!options.match_case)
        .dot_matches_new_line(true)
        .build()
        .map_err(|_| ())
}

fn apply_old_wildcards(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '%' && index + 1 < chars.len() {
            let digit = chars[index + 1];
            if ('1'..='9').contains(&digit) {
                let wider = index + 2 < chars.len() && chars[index + 2].is_ascii_digit();
                if !wider {
                    out.push('*');
                    index += 2;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("^");
    for ch in glob.chars() {
        match ch {
            '*' => out.push_str("(.*)"),
            '?' => out.push_str("(.)"),
            _ => out.push_str(&regex::escape(&ch.to_string())),
        }
    }
    out.push('$');
    out
}

fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

fn rename_one(from: &str, regex: &regex::Regex, options: &RenameOptions, index: u32) -> String {
    let (working, extension) = if options.ignore_extension {
        split_extension(from)
    } else {
        (from, "")
    };
    let Some(captures) = regex.captures(working) else {
        return from.to_owned();
    };
    let mut slots = vec![String::new(), working.to_owned()];
    for group in 1..captures.len() {
        let value = captures
            .get(group)
            .map(|mat| mat.as_str().to_owned())
            .unwrap_or_default();
        if group < slots.len() {
            slots[group] = value;
        } else {
            slots.push(value);
        }
    }
    let mut new_working = expand_new_name(&options.new_pattern, &slots, index);
    if new_working.is_empty() {
        return from.to_owned();
    }
    new_working.push_str(extension);
    new_working
}

pub fn apply_rename(plan: &RenamePlan) -> Result<()> {
    if plan.kind != RenameKind::Ready {
        bail!("not ready");
    }
    if same_item(&plan.source, &plan.dest) && plan.from != plan.to {
        let parent = plan.source.parent().unwrap_or_else(|| Path::new(""));
        let temporary = parent.join(format!(
            ".fastcopy-rename-{}-{}",
            std::process::id(),
            plan.from
        ));
        fs::rename(&plan.source, &temporary)?;
        if let Err(error) = fs::rename(&temporary, &plan.dest) {
            let _ = fs::rename(&temporary, &plan.source);
            return Err(error.into());
        }
        return Ok(());
    }
    fs::rename(&plan.source, &plan.dest)?;
    Ok(())
}

fn is_valid_file_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return false;
    }
    !name.chars().any(|ch| {
        matches!(
            ch,
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
        )
    })
}

fn same_item(left: &Path, right: &Path) -> bool {
    let left_abs = std::path::absolute(left).unwrap_or_else(|_| left.to_path_buf());
    let right_abs = std::path::absolute(right).unwrap_or_else(|_| right.to_path_buf());
    left_abs
        .to_string_lossy()
        .eq_ignore_ascii_case(&right_abs.to_string_lossy())
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
    fn reconcile_deletes_and_reorders() {
        let items = vec![
            RenameItem {
                path: PathBuf::from(r"C:\a\one.txt"),
                from: "one.txt".to_owned(),
            },
            RenameItem {
                path: PathBuf::from(r"C:\a\two.txt"),
                from: "two.txt".to_owned(),
            },
            RenameItem {
                path: PathBuf::from(r"C:\a\three.txt"),
                from: "three.txt".to_owned(),
            },
        ];
        let reordered = reconcile_old_names(&items, "three.txt\none.txt");
        assert_eq!(reordered.len(), 2);
        assert_eq!(reordered[0].from, "three.txt");
        assert_eq!(reordered[0].path, PathBuf::from(r"C:\a\three.txt"));
        assert_eq!(reordered[1].from, "one.txt");
        assert_eq!(reordered[1].path, PathBuf::from(r"C:\a\one.txt"));
        let edited = reconcile_old_names(&items, "one-x.txt\ntwo.txt\nthree.txt");
        assert_eq!(edited[0].from, "one-x.txt");
        assert_eq!(edited[0].path, PathBuf::from(r"C:\a\one.txt"));
        assert_eq!(edited[1].from, "two.txt");
        let restored = reconcile_selection(&items, &reordered, "one.txt\nthree.txt");
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].from, "one.txt");
        assert_eq!(restored[0].path, PathBuf::from(r"C:\a\one.txt"));
    }

    #[test]
    fn align_new_names_keeps_empty_and_trailing_lines() {
        assert_eq!(split_name_lines("a.txt\nb.txt\n"), vec!["a.txt", "b.txt", ""]);
        let aligned = align_new_name_text("a.txt\n\nb.txt\n", &["A".to_owned(), "B".to_owned()]);
        assert_eq!(aligned, "A\n\nB\n");
    }

    #[test]
    fn expands_new_name_tokens() {
        let slots = vec![String::new(), "photo.jpg".to_owned(), "photo".to_owned()];
        assert_eq!(expand_new_name("%1", &slots, 1), "photo.jpg");
        assert_eq!(expand_new_name("x_%1", &slots, 1), "x_photo.jpg");
        assert_eq!(expand_new_name("%2-#", &slots, 3), "photo-3");
        assert_eq!(expand_new_name("f-##", &slots, 4), "f-04");
        assert_eq!(expand_new_name("{nn}", &slots, 7), "07");
    }

    #[test]
    fn builds_new_names() {
        let mut options = RenameOptions::default();
        options.new_pattern = "x_%1".to_owned();
        let root = PathBuf::from(r"C:\tmp\photo.jpg");
        let plans = plan_renames(&[root], &options);
        assert_eq!(plans[0].to, "x_photo.jpg");
        options.ignore_extension = true;
        options.new_pattern = "%1-2".to_owned();
        let plans = plan_renames(&[PathBuf::from(r"C:\tmp\photo.jpg")], &options);
        assert_eq!(plans[0].to, "photo-2.jpg");
        options.old_pattern = "nope".to_owned();
        options.new_pattern = "x".to_owned();
        options.ignore_extension = false;
        let plans = plan_renames(&[PathBuf::from(r"C:\tmp\a.txt")], &options);
        assert_eq!(plans[0].to, "a.txt");
        assert_eq!(plans[0].kind, RenameKind::Unchanged);
        options.old_pattern = "%1".to_owned();
        options.new_pattern = "###. %1".to_owned();
        let plans = plan_renames(
            &[
                PathBuf::from(r"C:\tmp\a.txt"),
                PathBuf::from(r"C:\tmp\b.txt"),
            ],
            &options,
        );
        assert_eq!(plans[0].to, "001. a.txt");
        assert_eq!(plans[1].to, "002. b.txt");
        assert_eq!(plans[0].kind, RenameKind::Ready);
    }

    #[test]
    fn everything_style_percent_capture() {
        let mut options = RenameOptions::default();
        options.old_pattern = "node-gyp-build-optional-packages%1".to_owned();
        options.new_pattern = "%1".to_owned();
        let plans = plan_renames(
            &[
                PathBuf::from(r"C:\t\node-gyp-build-optional-packages.cmd"),
                PathBuf::from(r"C:\t\node-gyp-build-optional-packages-optional"),
                PathBuf::from(r"C:\t\other.txt"),
            ],
            &options,
        );
        assert_eq!(plans[0].to, ".cmd");
        assert_eq!(plans[1].to, "-optional");
        assert_eq!(plans[2].to, "other.txt");
        assert_eq!(plans[2].kind, RenameKind::Unchanged);
        options.new_pattern = "test%1".to_owned();
        let plans = plan_renames(
            &[PathBuf::from(r"C:\t\node-gyp-build-optional-packages.cmd")],
            &options,
        );
        assert_eq!(plans[0].to, "test.cmd");
        options.old_pattern = "%1-%2".to_owned();
        options.new_pattern = "%2_%1".to_owned();
        let plans = plan_renames(&[PathBuf::from(r"C:\t\a-b.txt")], &options);
        assert_eq!(plans[0].to, "b.txt_a");
    }

    #[test]
    fn infers_common_prefix_and_suffix() {
        let names = vec![
            "node-gyp-build-optional-packages.cmd".to_owned(),
            "node-gyp-build-optional-packages.ps1".to_owned(),
            "node-gyp-build-optional-packages-optional".to_owned(),
            "node-gyp-build-optional-packages-optional.cmd".to_owned(),
            "node-gyp-build-optional-packages-test".to_owned(),
        ];
        let (old, new) = common_rename_pattern(&names, false);
        assert_eq!(old, "node-gyp-build-optional-packages%1");
        assert_eq!(new, old);
        let names = vec!["abc-one.txt".to_owned(), "abc-two.txt".to_owned()];
        let (old, _) = common_rename_pattern(&names, false);
        assert_eq!(old, "abc-%1.txt");
        let (old, _) = common_rename_pattern(&names, true);
        assert_eq!(old, "abc-%1");
        let (old, new) = common_rename_pattern(&["only.txt".to_owned()], false);
        assert_eq!(old, "%1");
        assert_eq!(new, "%1");
        let (old, _) = common_rename_pattern(
            &["abc.txt".to_owned(), "xyz.md".to_owned()],
            false,
        );
        assert_eq!(old, "%1");
    }

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

    #[test]
    fn plan_and_apply_rename() {
        let root = tempdir().unwrap();
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        fs::write(&a, b"1").unwrap();
        fs::write(&b, b"2").unwrap();
        let mut options = RenameOptions::default();
        options.new_pattern = "p_%1".to_owned();
        let plans = plan_renames(&[a.clone(), b.clone()], &options);
        assert_eq!(plans[0].to, "p_a.txt");
        assert_eq!(plans[0].kind, RenameKind::Ready);
        apply_rename(&plans[0]).unwrap();
        apply_rename(&plans[1]).unwrap();
        assert!(root.path().join("p_a.txt").is_file());
        assert!(root.path().join("p_b.txt").is_file());
    }

    #[test]
    fn plan_detects_conflict() {
        let root = tempdir().unwrap();
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        fs::write(&a, b"1").unwrap();
        fs::write(&b, b"2").unwrap();
        let mut options = RenameOptions::default();
        options.old_pattern = "a.txt".to_owned();
        options.new_pattern = "b.txt".to_owned();
        let plans = plan_renames(&[a, b], &options);
        assert_eq!(plans[0].to, "b.txt");
        assert_eq!(plans[0].kind, RenameKind::Conflict);
    }
}
