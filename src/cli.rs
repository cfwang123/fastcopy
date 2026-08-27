use crate::i18n::Strings;
use crate::model::Settings;
use anyhow::{Result, anyhow};
use std::path::PathBuf;

pub const EXIT_OK: i32 = 0;
pub const EXIT_SKIPPED: i32 = 1;
pub const EXIT_FAILED: i32 = 2;
pub const EXIT_CANCELLED: i32 = 3;
pub const EXIT_USAGE: i32 = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOptions {
    pub workers: Option<usize>,
    pub ignore: bool,
    pub permanent: bool,
    pub recycle_bin: bool,
    pub yes: bool,
}

pub fn parse_options(arguments: &[String], t: &Strings) -> Result<(CliOptions, Vec<PathBuf>)> {
    let mut options = CliOptions::default();
    let mut paths = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.as_str() {
            "--workers" => {
                let raw = arguments
                    .get(index + 1)
                    .ok_or_else(|| anyhow!("{}", t.workers_missing_value()))?;
                let count = raw
                    .parse::<usize>()
                    .map_err(|_| anyhow!("{}", t.workers_not_integer()))?;
                if count == 0 {
                    return Err(anyhow!("{}", t.workers_not_positive()));
                }
                options.workers = Some(count.clamp(1, 64));
                index += 2;
            }
            "--ignore" => {
                options.ignore = true;
                index += 1;
            }
            "--permanent" => {
                options.permanent = true;
                index += 1;
            }
            "--recycle-bin" => {
                options.recycle_bin = true;
                index += 1;
            }
            "--yes" => {
                options.yes = true;
                index += 1;
            }
            _ if argument.starts_with("--") => {
                return Err(anyhow!("{}", t.unknown_cli_argument(argument)));
            }
            _ => {
                paths.push(PathBuf::from(argument));
                index += 1;
            }
        }
    }
    Ok((options, paths))
}

pub fn apply_options(settings: &mut Settings, options: &CliOptions) {
    if let Some(count) = options.workers {
        settings.worker_count = count;
    }
    if options.ignore {
        settings.use_ignore_file = true;
    }
    if options.permanent {
        settings.delete_mode = crate::model::DeleteMode::Permanent;
    } else if options.recycle_bin {
        settings.delete_mode = crate::model::DeleteMode::RecycleBin;
    }
}

pub fn exit_code(cancelled: bool, error_count: usize, skip_count: usize) -> i32 {
    if cancelled {
        EXIT_CANCELLED
    } else if error_count > 0 {
        EXIT_FAILED
    } else if skip_count > 0 {
        EXIT_SKIPPED
    } else {
        EXIT_OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::ZH;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn parses_two_paths_and_flags() {
        let (options, paths) =
            parse_options(&args(&["src", "dest", "--workers", "8", "--ignore"]), &ZH).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(options.workers, Some(8));
        assert!(options.ignore);
    }

    #[test]
    fn parses_multiple_sources_with_mixed_flags() {
        let (options, paths) = parse_options(
            &args(&["--ignore", "a", "b", "--workers", "4", "c", "dest"]),
            &ZH,
        )
        .unwrap();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("a"),
                PathBuf::from("b"),
                PathBuf::from("c"),
                PathBuf::from("dest")
            ]
        );
        assert_eq!(options.workers, Some(4));
        assert!(options.ignore);
        assert!(!options.yes);
    }

    #[test]
    fn parses_delete_flags() {
        let (options, paths) =
            parse_options(&args(&["one", "--permanent", "two", "--yes"]), &ZH).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(options.permanent);
        assert!(options.yes);
    }

    #[test]
    fn rejects_unknown_flag() {
        let error = parse_options(&args(&["src", "--nope", "dest"]), &ZH).unwrap_err();
        assert!(error.to_string().contains("未知命令行参数"));
    }

    #[test]
    fn rejects_missing_workers_value() {
        let error = parse_options(&args(&["src", "dest", "--workers"]), &ZH).unwrap_err();
        assert!(error.to_string().contains("--workers"));
    }

    #[test]
    fn exit_code_distinguishes_skip_and_fail() {
        assert_eq!(exit_code(false, 0, 0), EXIT_OK);
        assert_eq!(exit_code(false, 0, 3), EXIT_SKIPPED);
        assert_eq!(exit_code(false, 2, 5), EXIT_FAILED);
        assert_eq!(exit_code(true, 0, 0), EXIT_CANCELLED);
    }
}
