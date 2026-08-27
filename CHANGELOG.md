# Changelog / 更新日志

Each item is English, then Chinese.
每条先写英文，再写中文。

## v1.0.1 (2026-08-26 ~ 27)

- Refresh README / README.zh.md for v1.0.1 (CLI section, settings path, changelog link). Ignore Windows shortcuts and IDE junk in `.gitignore`.
  按 v1.0.1 更新 README / README.zh.md（命令行、设置路径、更新日志链接）。`.gitignore` 忽略快捷方式和 IDE 临时文件。
- Show the package version in the main window title bar (e.g. `快速复制 1.0.1` / `FastCopy 1.0.1`).
  主窗口标题栏显示版本号（如 `快速复制 1.0.1` / `FastCopy 1.0.1`）。
- Read the current Explorer selection (and invoke the menu once) so context-menu copy/cut/delete can include more than ~100 top-level items. Existing menus are repaired on the next launch.
  右键复制/剪切/删除改为读取资源管理器当前选中项并只启动一次，可超过约 100 个顶层选中项。已注册菜单会在下次启动时自动修复。
- Add a setting for hard links, symbolic links, and directory junctions (ignore by default; follow or preserve).
  增加硬链接/符号链接/目录联接处理策略（默认忽略，可跟随或保留为链接）。
- Replace placeholder app and context-menu icons.
  替换程序图标与右键菜单占位图标。
- Version 1.0.1, with `node pack.js` producing `release/fastcopy_x.x.x.7z`.
  版本号 1.0.1；`node pack.js` 一键打包为 `release/fastcopy_x.x.x.7z`。
- Recreate directory junctions as junctions when the link policy is Preserve (not as directory symbolic links).
  「保留为链接」时目录联接重建为 junction，而不是目录符号链接。
- Add tests for conflict policy, locked-file retry, Explorer menu register/unregister, and link Follow/Preserve.
  补测试：冲突策略、占用重试、右键注册/卸载、链接 Follow/Preserve。
- Show the Explorer FastCopy submenu only when folders are selected, not when files are selected. Existing file-item menus are removed on the next launch.
  右键「快速复制」仅在选中文件夹时出现，选中文件时不出现。已注册的文件菜单会在下次启动时清除。
- Restore Quick Paste / Cancel on folder background: do not delete those verbs during cascade repair, and do not set MultiSelectModel on background verbs (Explorer hides them).
  修复文件夹空白处「快速粘贴」「取消剪切/复制」：级联修复时不再删除它们；背景菜单不再设置 MultiSelectModel（否则资源管理器会隐藏）。
- Allow Quick Cut / Quick Copy / Quick Delete when multiple folders are selected (`MultiSelectModel=Document`).
  选中多个文件夹时也可以快速剪切、复制、删除（级联菜单改为 `MultiSelectModel=Document`）。
- Add Copy as symbolic link and Copy as hard link for selected files and/or folders. Paste on a folder background shows “Paste as symbolic/hard link”, or “Paste (N files) as …” when more than one item was copied.
  选中文件和/或文件夹时，子菜单增加「复制为符号链接」「复制为硬链接」。粘贴时显示「粘贴为符号链接/硬链接」，多于 1 项时为「粘贴(x个文件)为符号链接/硬链接」。
- Add Settings toggles for the finish toast (copy/move/delete vs symbolic/hard-link paste). Raise the Settings window so all controls fit.
  设置中增加「完成时提示」和「符号/硬链接完成提示」；设置窗口加高以显示全部控件。
- Send finish toasts with FastCopy’s own AppUserModelID so PowerShell does not flash (noticeable on fast symbolic/hard-link paste).
  完成通知改为使用本程序 AppID，避免弹出/闪现 PowerShell（粘贴符号/硬链接时尤其明显）。
- When pasting as a symbolic or hard link, if the destination name exists, append ` 2`, ` 3`, ` 4`, … instead of overwriting or skipping.
  粘贴为符号链接/硬链接时，若目标文件名已存在，自动附加 ` 2`、` 3`、` 4`……，不覆盖也不跳过。
- Run symbolic/hard-link paste in the background without opening the progress window (it finished so fast that the window only flashed).
  粘贴符号/硬链接改为后台执行，不打开进度窗口（原先完成太快，窗口只会闪一下）。
- Preserve NTFS sparse files (holes stay holes). Fall back to a normal copy if the destination volume does not support sparse files.
  复制 NTFS 稀疏文件时保留空洞。目标卷不支持稀疏文件时回退为普通复制。
- CLI `--copy` / `--move` / `--delete` accept multiple paths; `--ignore` turns on the ignore file; exit codes are 0 success, 1 skipped, 2 failed, 3 cancelled, 64 usage error.
  命令行 `--copy` / `--move` / `--delete` 支持多个路径；`--ignore` 启用 ignore 文件；退出码：0 成功、1 有跳过、2 有失败、3 取消、64 参数错误。
- Show Quick Cut / Quick Copy / Quick Delete for selected files as well as folders. Mixed selections show the full submenu. Existing menus are repaired on the next launch.
  选中文件时也可以快速剪切、复制、删除。文件与文件夹混选时显示完整子菜单。已注册菜单会在下次启动时自动补全。
- Add Settings to the Explorer FastCopy submenu (separator above it). Existing menus are repaired on the next launch.
  右键「快速复制」子菜单增加「参数设置」（上方有分隔线）。已注册菜单会在下次启动时自动补上。
- Add Open link target to the FastCopy submenu. Clicking it reveals the real target of a symbolic link, junction, or `.lnk` in Explorer.
  「快速复制」子菜单增加「打开链接目标」。点击后在资源管理器中定位符号链接、目录联接或快捷方式的真实目标。
- Add View source path to the FastCopy submenu. A small window shows the path (the real target for a symbolic link, junction, or `.lnk`) with Copy path and Open path.
  「快速复制」子菜单增加「查看源路径」。弹窗显示路径（符号链接、目录联接或快捷方式为真实目标），可复制、可在资源管理器中打开。

## v0.1.0 (2026-08-26)

- Add settings to honor an ignore file when copying folders (default name `.gitignore`).
  增加设置：复制文件夹时可启用 ignore 文件（默认文件名 `.gitignore`）。
- Always open the window in the center of the current screen.
  打开窗口时总是显示在当前屏幕正中。
- Shrink UI text and controls to WinForms 9pt (12px) size, with tighter padding.
  界面字号和控件按 WinForm 常用 9pt（约 12px）缩小，间距一并收紧。
- Keep Save settings and Close fully visible at the bottom of the Settings page; the form scrolls if the window is short.
  设置页底部「保存设置」「关闭」始终完整显示，窗口较矮时上方表单可滚动。
- Ask for confirmation before permanent delete (GUI and `--delete --permanent`; skip with `--yes`). Recycle Bin deletes are not confirmed.
  永久删除前弹出确认（界面和 `--delete --permanent`；可用 `--yes` 跳过）。移入回收站不确认。
- Keep Quick Cut/Copy/Delete inside the FastCopy submenu for selected files and folders; do not show those three as top-level items.
  选中文件或文件夹时，快速剪切/复制/删除放在「快速复制」子菜单中，不直接显示在右键菜单里。
- Remove the unused per-thread buffer setting. Copy already uses CopyFileEx.
  去掉无效的「每线程缓冲区」设置。复制已使用 CopyFileEx。
- Show scan progress (item count, bytes, current path) before copy/move/delete starts.
  复制/移动/删除开始前显示扫描进度（文件数、字节数、当前路径）。
- Localize engine and CLI errors to match the UI language.
  引擎与命令行错误跟随界面语言。
- Register the Explorer menu for the current user without administrator rights. Changing language updates cascade labels immediately.
  右键菜单改为当前用户注册，无需管理员。切换语言会立即更新级联菜单文案。
- Add a setting to skip files whose size and modified time already match the destination.
  增加设置项：大小和修改时间相同则跳过。
- Copy files ≥ 64 MiB on a dedicated worker with COPY_FILE_NO_BUFFERING, falling back if the flag is rejected.
  大于等于 64 MiB 的文件单独占用 1 个线程，并尝试无缓冲复制，失败则回退。
- Skip locked/in-use files, list them when the task ends, and allow retry or export.
  占用中的文件自动跳过，任务结束显示失败列表，可重试或导出。
- Add headless `--move`, and show a toast when a task finishes (the window still closes on success).
  增加无界面 `--move`；任务完成时弹出系统通知（成功时窗口仍会自动关闭）。
- Add Chinese/English UI language switch in Settings.
  设置页增加中英文界面切换。
- Split docs: English `README.md`, Chinese `README.zh.md`, and one bilingual `CHANGELOG.md`.
  文档改为英文 `README.md`、中文 `README.zh.md`，以及单一双语 `CHANGELOG.md`。
- Create a Rust/egui Windows desktop app.
  创建 Rust/egui Windows 桌面程序。
- Add multithreaded file copy, same-volume fast move, and copy-then-delete across volumes.
  增加多线程文件复制、同卷快速移动和跨卷复制后删除。
- Add Recycle Bin delete, permanent delete, and confirmation for dangerous operations.
  增加回收站删除、永久删除及危险操作确认。
- Add a task queue, drag-and-drop, pause, resume, cancel, speed, and remaining-time display.
  增加任务队列、拖放、暂停、继续、取消、速度和剩余时间显示。
- Add conflict policy, buffer size, worker count, and verify options.
  增加冲突策略、缓冲区、并发数和校验选项。
- Add a machine-wide Explorer cascade menu with UAC register/unregister.
  增加全机资源管理器级联右键菜单和 UAC 注册/卸载。
- Add a single-instance request queue to aggregate Explorer multi-select.
  增加单实例请求队列，用于聚合资源管理器多选操作。
- Rebuild as a modern light card UI with better contrast, hierarchy, and progress layout.
  重构为现代浅色卡片界面，改善文字对比度、信息层级和进度展示。
- Opening the app shows Settings; Explorer menu tasks show only the progress window.
  直接打开程序即显示参数设置；右键菜单任务仅显示进度窗口。
- Settings is a full page and enlarges the window on open so content is not clipped.
  参数设置改为独立整页，打开时自动放大窗口，避免内容被裁切。
- Add an app icon and context-menu icons for cut, copy, paste, and delete.
  增加程序图标，以及剪切、复制、粘贴、删除的右键菜单图标。
- Shrink the Release binary: glow/OpenGL, no wgpu or default fonts, LTO, and symbol stripping.
  精简 Release 体积：改用 glow/OpenGL，关闭 wgpu 与默认字体，并启用 LTO 与符号剥离。
- Close the progress window after a successful copy, move, or delete; keep it open when there are errors.
  复制、移动、删除成功完成后自动关闭进度窗口；有错误时保留窗口。
- Fix black background showing at the bottom of the progress window.
  修复进度窗口底部露出黑色背景的问题。
- Show files processed per second on the progress page.
  进度界面增加每秒处理文件数。
- Show the four live progress metrics in two rows.
  进度窗口四个实时数据改为两行显示。
- Add headless copy: `fastcopy.exe --copy SOURCE DEST_FOLDER [--workers N]`.
  增加无界面复制命令：`fastcopy.exe --copy 源路径 目标文件夹 [--workers 线程数]`。
- Add headless delete: `fastcopy.exe --delete PATH [--permanent|--recycle-bin]`.
  增加无界面删除命令：`fastcopy.exe --delete 路径 [--permanent|--recycle-bin]`。
- Copy many files with concurrent Windows CopyFileEx; drop per-file fsync and write-temp-first for new files.
  大量文件复制改用 Windows CopyFileEx 并发，去掉每文件 fsync 与先写临时文件。
- Move Quick Paste on folders and folder backgrounds to a top-level menu item, out of the submenu.
  文件夹及文件夹空白处的「快速粘贴」改为顶级菜单项，不再放在子菜单中。
- Hide Quick Paste when files/folders are selected; add Cancel Cut/Copy on folder backgrounds.
  选中文件/文件夹时不再显示「快速粘贴」；文件夹空白处增加「取消剪切/复制」。
- Hide Quick Paste and Cancel Cut/Copy on folder backgrounds when nothing has been cut or copied.
  未剪切或复制时，文件夹空白处不显示「快速粘贴」和「取消剪切/复制」。
- Hide those two items immediately after Cancel Cut/Copy so Explorer does not keep cached verbs.
  取消剪切/复制后立即隐藏这两项菜单，避免资源管理器继续显示缓存项。
- Clear the internal cut/copy list when Quick Paste starts, to avoid pasting twice.
  快速粘贴开始后清空内部剪切/复制列表，避免重复粘贴。
- Refresh Explorer menu status automatically after register or unregister, without clicking Refresh.
  注册或卸载右键菜单后自动刷新状态，无需再点「刷新状态」。
