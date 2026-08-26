use crate::model::{ConflictPolicy, DeleteMode, Language, LinkPolicy, OperationKind};
use std::path::{Path, PathBuf};

pub struct Strings {
    pub app_title: &'static str,
    pub settings_title: &'static str,
    pub settings_subtitle: &'static str,
    pub language: &'static str,
    pub workers: &'static str,
    pub workers_suffix: &'static str,
    pub conflict: &'static str,
    pub overwrite: &'static str,
    pub skip: &'static str,
    pub rename: &'static str,
    pub verify_size: &'static str,
    pub skip_unchanged: &'static str,
    pub use_ignore_file: &'static str,
    pub ignore_file_name: &'static str,
    pub link_policy: &'static str,
    pub link_ignore: &'static str,
    pub link_follow: &'static str,
    pub link_preserve: &'static str,
    pub delete_mode: &'static str,
    pub recycle_bin: &'static str,
    pub permanent: &'static str,
    pub shell_menu: &'static str,
    pub registered: &'static str,
    pub registered_machine: &'static str,
    pub unregistered: &'static str,
    pub register: &'static str,
    pub unregister: &'static str,
    pub unregister_admin: &'static str,
    pub refresh_status: &'static str,
    pub register_ok: &'static str,
    pub register_incomplete: &'static str,
    pub unregister_ok: &'static str,
    pub unregister_incomplete: &'static str,
    pub save_settings: &'static str,
    pub settings_saved: &'static str,
    pub close: &'static str,
    pub copy: &'static str,
    pub move_kind: &'static str,
    pub delete: &'static str,
    pub scanning: &'static str,
    pub completed: &'static str,
    pub speed: &'static str,
    pub file_speed: &'static str,
    pub eta: &'static str,
    pub resume: &'static str,
    pub pause: &'static str,
    pub cancel_task: &'static str,
    pub retry_failed: &'static str,
    pub export_errors: &'static str,
    pub export_ok: &'static str,
    pub menu_cascade: &'static str,
    pub menu_cut: &'static str,
    pub menu_copy: &'static str,
    pub menu_delete: &'static str,
    pub menu_paste: &'static str,
    pub menu_clear: &'static str,
    pub items_unit: &'static str,
    pub clipboard_empty: &'static str,
    pub clipboard_empty_hint: &'static str,
}

pub const ZH: Strings = Strings {
    app_title: "快速复制",
    settings_title: "参数设置",
    settings_subtitle: "调整复制性能、冲突处理和右键菜单",
    language: "界面语言",
    workers: "并发线程数",
    workers_suffix: " 个",
    conflict: "文件冲突",
    overwrite: "覆盖",
    skip: "跳过",
    rename: "自动重命名",
    verify_size: "复制后校验文件长度",
    skip_unchanged: "大小和修改时间相同则跳过",
    use_ignore_file: "启用 ignore 文件（仅复制文件夹时有效）",
    ignore_file_name: "ignore 文件名",
    link_policy: "硬链接/符号链接/目录联接",
    link_ignore: "忽略",
    link_follow: "跟随目标",
    link_preserve: "保留为链接",
    delete_mode: "默认删除方式",
    recycle_bin: "移入回收站",
    permanent: "永久删除",
    shell_menu: "资源管理器右键菜单",
    registered: "已注册（当前用户）",
    registered_machine: "已注册（全机，需管理员卸载）",
    unregistered: "未注册",
    register: "注册",
    unregister: "卸载",
    unregister_admin: "卸载全机菜单（需管理员）",
    refresh_status: "刷新状态",
    register_ok: "注册成功",
    register_incomplete: "注册未完成",
    unregister_ok: "已卸载",
    unregister_incomplete: "卸载未完成",
    save_settings: "保存设置",
    settings_saved: "设置已保存",
    close: "关闭",
    copy: "复制",
    move_kind: "移动",
    delete: "删除",
    scanning: "正在扫描",
    completed: "已完成",
    speed: "实时速度",
    file_speed: "文件速度",
    eta: "预计剩余",
    resume: "继续",
    pause: "暂停",
    cancel_task: "取消任务",
    retry_failed: "重试失败项",
    export_errors: "导出失败列表",
    export_ok: "已导出失败列表",
    menu_cascade: "快速复制",
    menu_cut: "快速剪切",
    menu_copy: "快速复制",
    menu_delete: "快速删除",
    menu_paste: "快速粘贴",
    menu_clear: "取消剪切/复制",
    items_unit: "项/s",
    clipboard_empty: "快速复制剪贴板为空",
    clipboard_empty_hint: "快速复制剪贴板为空，请先使用“快速复制”或“快速剪切”",
};

pub const EN: Strings = Strings {
    app_title: "FastCopy",
    settings_title: "Settings",
    settings_subtitle: "Copy performance, conflicts, and Explorer menu",
    language: "Language",
    workers: "Worker threads",
    workers_suffix: " threads",
    conflict: "File conflicts",
    overwrite: "Overwrite",
    skip: "Skip",
    rename: "Rename",
    verify_size: "Verify file size after copy",
    skip_unchanged: "Skip if size and modified time match",
    use_ignore_file: "Use ignore file (copy folders only)",
    ignore_file_name: "Ignore file name",
    link_policy: "Hard links / symlinks / junctions",
    link_ignore: "Ignore",
    link_follow: "Follow target",
    link_preserve: "Preserve as links",
    delete_mode: "Default delete mode",
    recycle_bin: "Recycle Bin",
    permanent: "Permanent delete",
    shell_menu: "Explorer context menu",
    registered: "Registered (this user)",
    registered_machine: "Registered (all users; admin to uninstall)",
    unregistered: "Not registered",
    register: "Register",
    unregister: "Unregister",
    unregister_admin: "Unregister all-users menu (admin)",
    refresh_status: "Refresh",
    register_ok: "Registered",
    register_incomplete: "Registration incomplete",
    unregister_ok: "Unregistered",
    unregister_incomplete: "Unregister incomplete",
    save_settings: "Save settings",
    settings_saved: "Settings saved",
    close: "Close",
    copy: "Copy",
    move_kind: "Move",
    delete: "Delete",
    scanning: "Scanning",
    completed: "Completed",
    speed: "Speed",
    file_speed: "Files/s",
    eta: "Time left",
    resume: "Resume",
    pause: "Pause",
    cancel_task: "Cancel",
    retry_failed: "Retry failed items",
    export_errors: "Export failed list",
    export_ok: "Failed list exported",
    menu_cascade: "FastCopy",
    menu_cut: "Quick Cut",
    menu_copy: "Quick Copy",
    menu_delete: "Quick Delete",
    menu_paste: "Quick Paste",
    menu_clear: "Cancel Cut/Copy",
    items_unit: "items/s",
    clipboard_empty: "FastCopy clipboard is empty",
    clipboard_empty_hint: "FastCopy clipboard is empty. Use Quick Copy or Quick Cut first.",
};

impl Language {
    pub fn strings(self) -> &'static Strings {
        match self {
            Self::Zh => &ZH,
            Self::En => &EN,
        }
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Self::Zh => "中文",
            Self::En => "English",
        }
    }
}

impl Strings {
    fn en(&self) -> bool {
        self.app_title == "FastCopy"
    }

    pub fn operation(&self, kind: OperationKind) -> &'static str {
        match kind {
            OperationKind::Copy => self.copy,
            OperationKind::Move => self.move_kind,
            OperationKind::Delete => self.delete,
        }
    }

    pub fn conflict_policy(&self, policy: ConflictPolicy) -> &'static str {
        match policy {
            ConflictPolicy::Overwrite => self.overwrite,
            ConflictPolicy::Skip => self.skip,
            ConflictPolicy::Rename => self.rename,
        }
    }

    pub fn delete_mode_label(&self, mode: DeleteMode) -> &'static str {
        match mode {
            DeleteMode::RecycleBin => self.recycle_bin,
            DeleteMode::Permanent => self.permanent,
        }
    }

    pub fn link_policy_label(&self, policy: LinkPolicy) -> &'static str {
        match policy {
            LinkPolicy::Ignore => self.link_ignore,
            LinkPolicy::Follow => self.link_follow,
            LinkPolicy::Preserve => self.link_preserve,
        }
    }

    pub fn confirm_permanent_body(&self, paths: &[PathBuf]) -> String {
        let count = paths.len();
        let mut lines: Vec<String> = paths
            .iter()
            .take(8)
            .map(|path| path.display().to_string())
            .collect();
        if count > 8 {
            if self.en() {
                lines.push(format!("... and {} more", count - 8));
            } else {
                lines.push(format!("……另有 {} 项", count - 8));
            }
        }
        let list = lines.join("\n");
        if self.en() {
            format!(
                "Permanently delete {count} item(s)?\nThis cannot be undone and will not use the Recycle Bin.\n\n{list}"
            )
        } else {
            format!("将永久删除 {count} 项（不进入回收站），无法撤销。\n\n{list}")
        }
    }

    pub fn in_progress(&self, kind: OperationKind) -> String {
        if self.en() {
            format!("{} in progress", self.operation(kind))
        } else {
            format!("正在{}", self.operation(kind))
        }
    }

    pub fn queued(&self, count: usize) -> String {
        if self.en() {
            format!("{count} more task(s) queued")
        } else {
            format!("另有 {count} 个任务等待")
        }
    }

    pub fn files_elapsed(&self, done: u64, total: u64, elapsed: &str) -> String {
        if self.en() {
            format!("Files: {done} / {total}    Elapsed: {elapsed}")
        } else {
            format!("文件：{done} / {total} 项　　耗时：{elapsed}")
        }
    }

    pub fn scanned(&self, items: u64, bytes: &str) -> String {
        if self.en() {
            format!("Found {items} item(s), {bytes}")
        } else {
            format!("已找到 {items} 项，{bytes}")
        }
    }

    pub fn current_file(&self, path: &str) -> String {
        if self.en() {
            format!("Current: {path}")
        } else {
            format!("当前：{path}")
        }
    }

    pub fn error_details(&self, count: usize) -> String {
        if self.en() {
            format!("Failed items ({count})")
        } else {
            format!("失败列表（{count}）")
        }
    }

    pub fn result_title(&self, kind: OperationKind, cancelled: bool, errors: usize) -> String {
        let name = self.operation(kind);
        if cancelled {
            return if self.en() {
                format!("{name} cancelled")
            } else {
                format!("{name}已取消")
            };
        }
        if errors == 0 {
            return if self.en() {
                format!("{name} finished")
            } else {
                format!("{name}完成")
            };
        }
        if self.en() {
            format!("{name} finished with {errors} error(s)")
        } else {
            format!("{name}完成，有 {errors} 个错误")
        }
    }

    pub fn processed(&self, bytes: &str, items: u64) -> String {
        if self.en() {
            format!("Processed {bytes}, {items} item(s)")
        } else {
            format!("处理 {bytes}，共 {items} 项")
        }
    }

    pub fn notify_done(&self, kind: OperationKind) -> String {
        if self.en() {
            format!("{} finished", self.operation(kind))
        } else {
            format!("{}完成", self.operation(kind))
        }
    }

    pub fn notify_done_errors(&self, kind: OperationKind, errors: usize) -> String {
        if self.en() {
            format!("{} finished with {errors} error(s)", self.operation(kind))
        } else {
            format!("{}完成，有 {errors} 个错误", self.operation(kind))
        }
    }

    pub fn export_failed(&self, error: &str) -> String {
        if self.en() {
            format!("Failed to export: {error}")
        } else {
            format!("导出失败：{error}")
        }
    }

    pub fn register_failed(&self, error: &str) -> String {
        if self.en() {
            format!("Register failed: {error}")
        } else {
            format!("注册失败：{error}")
        }
    }

    pub fn unregister_failed(&self, error: &str) -> String {
        if self.en() {
            format!("Unregister failed: {error}")
        } else {
            format!("卸载失败：{error}")
        }
    }

    pub fn settings_save_failed(&self, error: &str) -> String {
        if self.en() {
            format!("Failed to save settings: {error}")
        } else {
            format!("设置保存失败：{error}")
        }
    }

    pub fn pending_failed(&self, error: &str) -> String {
        if self.en() {
            format!("Failed to read Explorer request: {error}")
        } else {
            format!("读取右键菜单请求失败：{error}")
        }
    }

    pub fn shell_status(&self, user: bool, machine: bool) -> String {
        let state = if machine {
            self.registered_machine
        } else if user {
            self.registered
        } else {
            self.unregistered
        };
        format!("{}: {state}", self.shell_menu)
    }

    pub fn items_per_sec(&self, items_per_sec: f64) -> String {
        if items_per_sec >= 100.0 {
            format!("{:.0} {}", items_per_sec, self.items_unit)
        } else if items_per_sec >= 10.0 {
            format!("{:.1} {}", items_per_sec, self.items_unit)
        } else {
            format!("{:.2} {}", items_per_sec, self.items_unit)
        }
    }

    pub fn cancelled(&self) -> &'static str {
        if self.en() {
            "Operation cancelled"
        } else {
            "操作已取消"
        }
    }

    pub fn no_sources(&self) -> &'static str {
        if self.en() {
            "No source files or folders selected"
        } else {
            "没有选择源文件或文件夹"
        }
    }

    pub fn source_missing(&self, path: &Path) -> String {
        if self.en() {
            format!("Source does not exist: {}", path.display())
        } else {
            format!("源路径不存在：{}", path.display())
        }
    }

    pub fn no_destination(&self) -> &'static str {
        if self.en() {
            "Destination folder not specified"
        } else {
            "未指定目标文件夹"
        }
    }

    pub fn cannot_create_destination(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Cannot create destination {}: {error}", path.display())
        } else {
            format!("无法创建目标文件夹：{}：{error}", path.display())
        }
    }

    pub fn destination_inside_source(&self, path: &Path) -> String {
        if self.en() {
            format!(
                "Destination cannot be inside the source folder: {}",
                path.display()
            )
        } else {
            format!("目标文件夹不能位于源文件夹内部：{}", path.display())
        }
    }

    pub fn source_name_unknown(&self, path: &Path) -> String {
        if self.en() {
            format!("Cannot determine source name: {}", path.display())
        } else {
            format!("无法确定源路径名称：{}", path.display())
        }
    }

    pub fn scan_failed(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Scan failed for {}: {error}", path.display())
        } else {
            format!("扫描失败：{}：{error}", path.display())
        }
    }

    pub fn move_fallback(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!(
                "Fast move failed, copying then deleting: {} ({error})",
                path.display()
            )
        } else {
            format!("快速移动失败，改用复制后删除：{} ({error})", path.display())
        }
    }

    pub fn cannot_create_dir(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Cannot create folder {}: {error}", path.display())
        } else {
            format!("无法创建目录 {}：{error}", path.display())
        }
    }

    pub fn cannot_create_link(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Cannot create link {}: {error}", path.display())
        } else {
            format!("无法创建链接 {}：{error}", path.display())
        }
    }

    pub fn copied_but_cannot_delete_source(
        &self,
        path: &Path,
        error: &impl std::fmt::Display,
    ) -> String {
        if self.en() {
            format!(
                "Copied, but cannot delete source {}: {error}",
                path.display()
            )
        } else {
            format!("目标已复制，但无法删除源文件 {}：{error}", path.display())
        }
    }

    pub fn file_error(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        format!("{}：{error}", path.display())
    }

    pub fn file_locked(&self, path: &Path) -> String {
        if self.en() {
            format!("Skipped (in use): {}", path.display())
        } else {
            format!("已跳过（文件被占用）：{}", path.display())
        }
    }

    pub fn verify_size_failed(&self) -> &'static str {
        if self.en() {
            "File size check failed after copy"
        } else {
            "复制后文件长度校验失败"
        }
    }

    pub fn cannot_recycle(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Cannot move to Recycle Bin {}: {error}", path.display())
        } else {
            format!("无法移入回收站 {}：{error}", path.display())
        }
    }

    pub fn cannot_delete(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Cannot delete {}: {error}", path.display())
        } else {
            format!("无法删除 {}：{error}", path.display())
        }
    }

    pub fn cannot_delete_dir(&self, path: &Path, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Cannot delete folder {}: {error}", path.display())
        } else {
            format!("无法删除目录 {}：{error}", path.display())
        }
    }

    pub fn missing_cli_source(&self) -> &'static str {
        if self.en() {
            "Missing source path"
        } else {
            "命令行缺少源路径"
        }
    }

    pub fn missing_cli_destination(&self) -> &'static str {
        if self.en() {
            "Missing destination folder"
        } else {
            "命令行缺少目标文件夹"
        }
    }

    pub fn missing_cli_delete_path(&self) -> &'static str {
        if self.en() {
            "Missing path to delete"
        } else {
            "命令行缺少删除路径"
        }
    }

    pub fn missing_cli_path(&self) -> &'static str {
        if self.en() {
            "Missing path argument"
        } else {
            "命令行缺少路径参数"
        }
    }

    pub fn unknown_cli_argument(&self, argument: &str) -> String {
        if self.en() {
            format!("Unknown argument: {argument}")
        } else {
            format!("未知命令行参数：{argument}")
        }
    }

    pub fn workers_missing_value(&self) -> &'static str {
        if self.en() {
            "--workers needs a value"
        } else {
            "--workers 缺少数值"
        }
    }

    pub fn workers_not_integer(&self) -> &'static str {
        if self.en() {
            "--workers must be a positive integer"
        } else {
            "--workers 必须是正整数"
        }
    }

    pub fn workers_not_positive(&self) -> &'static str {
        if self.en() {
            "--workers must be greater than 0"
        } else {
            "--workers 必须大于 0"
        }
    }

    pub fn finished_with_errors(&self, error_count: usize) -> String {
        if self.en() {
            format!("Finished with {error_count} error(s)")
        } else {
            format!("操作完成但有 {error_count} 个错误")
        }
    }

    pub fn engine_stopped(&self) -> &'static str {
        if self.en() {
            "Engine stopped unexpectedly"
        } else {
            "引擎意外结束"
        }
    }

    pub fn cannot_load_icon(&self, error: &impl std::fmt::Display) -> String {
        if self.en() {
            format!("Cannot load app icon: {error}")
        } else {
            format!("无法加载程序图标：{error}")
        }
    }

    pub fn cannot_get_exe_path(&self) -> &'static str {
        if self.en() {
            "Cannot get program path"
        } else {
            "无法取得程序路径"
        }
    }

    pub fn uac_cancelled(&self) -> &'static str {
        if self.en() {
            "Administrator approval cancelled"
        } else {
            "已取消管理员授权"
        }
    }

    pub fn uac_start_failed(&self, code: u32) -> String {
        if self.en() {
            format!("Cannot start elevated process, error {code}")
        } else {
            format!("无法启动管理员权限进程，错误码 {code}")
        }
    }

    pub fn uac_wait_failed(&self, code: u32) -> String {
        if self.en() {
            format!("Failed waiting for elevated process, error {code}")
        } else {
            format!("等待管理员进程结束失败，错误码 {code}")
        }
    }
}

pub fn strings(language: Language) -> &'static Strings {
    language.strings()
}
