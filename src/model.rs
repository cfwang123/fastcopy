use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind {
    Copy,
    Move,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictPolicy {
    Overwrite,
    Skip,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteMode {
    RecycleBin,
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkPolicy {
    #[default]
    Ignore,
    Follow,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Zh,
    En,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub worker_count: usize,
    pub conflict_policy: ConflictPolicy,
    pub verify_file_size: bool,
    pub skip_unchanged: bool,
    pub use_ignore_file: bool,
    #[serde(default = "default_ignore_file_name")]
    pub ignore_file_name: String,
    pub link_policy: LinkPolicy,
    pub delete_mode: DeleteMode,
    pub language: Language,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            worker_count: std::thread::available_parallelism()
                .map(|count| count.get().clamp(2, 16))
                .unwrap_or(4),
            conflict_policy: ConflictPolicy::Overwrite,
            verify_file_size: true,
            skip_unchanged: false,
            use_ignore_file: false,
            ignore_file_name: default_ignore_file_name(),
            link_policy: LinkPolicy::Ignore,
            delete_mode: DeleteMode::RecycleBin,
            language: Language::default(),
        }
    }
}

pub fn default_ignore_file_name() -> String {
    ".gitignore".to_owned()
}

pub fn sanitize_ignore_file_name(name: &str) -> String {
    let trimmed = name.trim();
    let candidate = if trimmed.is_empty() {
        default_ignore_file_name()
    } else {
        trimmed.to_owned()
    };
    std::path::Path::new(&candidate)
        .file_name()
        .and_then(|part| part.to_str())
        .filter(|part| *part != "." && *part != "..")
        .unwrap_or(".gitignore")
        .to_owned()
}

#[derive(Debug, Clone)]
pub struct RetryItem {
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub delete_source: bool,
}

#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub kind: OperationKind,
    pub sources: Vec<PathBuf>,
    pub destination: Option<PathBuf>,
    pub settings: Settings,
    pub retry_items: Vec<RetryItem>,
}

#[derive(Debug, Clone, Default)]
pub struct ProgressSnapshot {
    pub scanning: bool,
    pub total_bytes: u64,
    pub completed_bytes: u64,
    pub total_items: u64,
    pub completed_items: u64,
    pub current_path: String,
    pub errors: Vec<String>,
    pub failed: Vec<RetryItem>,
}
