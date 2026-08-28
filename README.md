# FastCopy

Version **1.0.2**. [中文说明](README.zh.md) · [Changelog](CHANGELOG.md) · [Benchmark](benchmark.md)

A Windows 10/11 Rust file tool for copying, moving, and deleting large numbers of files. It uses a file-level concurrent queue, which works well for directories with many small files. Moves on the same volume prefer a filesystem rename.

## Features

- Copy, move, and delete files and folders
- Opening the app shows Settings; Explorer menu tasks show only the progress window
- Chinese/English UI language switch in Settings (labels in the language dropdown stay `中文` / `English`)
- Window title bar shows the package version (e.g. `FastCopy 1.0.2`)
- Scan progress before copy/move/delete starts
- Choose files and destinations from the Explorer context menu
- Preserve NTFS sparse-file holes instead of writing them as real zeros
- Total bytes, file count, current file, speed, files per second, elapsed time, and time remaining
- Pause, resume, cancel, failed-item list, retry, and export
- Overwrite, skip, or rename on conflicts; optional skip when size and modified time match
- When copying folders, optional gitignore-style skip using a named ignore file (default `.gitignore`)
- Hard links, symbolic links, and directory junctions: ignore (default), follow target, or preserve as links
- Recycle Bin by default, with optional permanent delete
- Optional file-size check after copy; last-write time, last-access time, and basic attributes are kept
- Files ≥ 64 MiB use a dedicated worker and unbuffered CopyFileEx
- Locked/in-use files are skipped and listed at the end for retry
- Explorer cascade for the current user (no admin): Quick Cut, Quick Copy, Quick Delete, copy as symbolic or hard link, Folder size, Copy paths, Batch rename, Settings (each with its own icon), Open link target, and View source path
- After a cut, copy, or link-copy, Quick Paste (or Paste as symbolic/hard link) and Cancel Cut/Copy appear on a folder background
- Selection is read from Explorer, so more than ~100 top-level items can be included
- App icon and context-menu icons (cut, copy, paste, delete, settings)
- Compact WinForms-like 9pt UI text and controls
- Window always opens centered on the current screen
- Toast notification when a task finishes (can be turned off in Settings; symbolic/hard-link paste has its own toggle). The toast is sent as FastCopy, not PowerShell, so a console does not flash. The progress window still closes on success

## Usage

1. Opening the app shows Settings immediately. Save settings and Close stay visible at the bottom.
2. In Explorer, use the FastCopy submenu on files or folders for Quick Cut, Quick Copy, or Quick Delete, Copy as symbolic link / Copy as hard link, Folder size, Copy paths, Batch rename, Open link target, View source path, or Settings. After a cut, copy, or link-copy, use Quick Paste (or Paste as symbolic/hard link) on a folder background, or Cancel Cut/Copy to clear the pending list.
3. After paste or delete starts, the app shows only the progress window (speed, time remaining, pause/cancel).
4. The window closes when a task finishes successfully; a toast is shown if that option is enabled in Settings. It stays open if there were errors so you can retry or export the list.

Permanent delete does not use the Recycle Bin; the app asks for confirmation first (CLI: skip with `--yes`). Cancelling a copy removes unfinished new destination files. Overwrites write a temporary file first, then replace, so the original stays until replacement finishes.

Settings are stored in `%LOCALAPPDATA%\FastCopy\settings.json`.

## Explorer menu

In Settings, click **Register** to add the menu for the current Windows user (no administrator prompt). An older all-users install can still be removed with **Unregister all-users menu (admin)**. The FastCopy submenu appears for selected files and/or folders (cut, copy, delete, copy as symbolic or hard link, Folder size, Copy paths, Batch rename, Open link target, View source path, and Settings, including mixed selections). Open link target and View source path appear only when a single shortcut, symbolic link, or junction is selected: the former opens Explorer at the real target; the latter shows the path in a window (Copy path / Open path). Folder size scans the selection and shows file count, folder count, and bytes. Copy paths writes one absolute path per line to the Windows text clipboard (hold Shift for paths relative to the common parent). Batch rename opens one window for the whole selection: editable old/new name lists (delete or reorder old-name lines to drop/reorder files and recalculate), white `%1`/`#` pattern boxes (auto-filled from the common prefix/suffix like Everything F2; `%1` is the first capture; `###. %1` for `001.` prefixes), OK/Cancel. Quick Paste is not shown when files or folders are selected; it appears on a folder background only after a cut, copy, or link-copy, together with Cancel Cut/Copy. When the clipboard holds a link-copy, the paste label is “Paste as symbolic link” / “Paste as hard link”, or “Paste (N files) as …” when more than one item was copied. Windows 11 may put classic menus under **Show more options**. An already-registered menu is repaired on the next launch so file items also get Cut/Copy/Delete.

Quick Copy, Quick Cut, and the link-copy commands use FastCopy’s own clipboard and do not replace the Windows text clipboard. Quick Paste clears that clipboard, so you must cut or copy again to paste. Hold Shift while clicking Quick Paste to keep the list and paste again (a cut still clears it, because the source is gone). When Explorer starts a menu command, the app reads the current Explorer selection so more than 100 top-level items can be included. Quick Delete uses the default delete mode saved in Settings.

Paste as symbolic link creates one link per top-level item pointing at the original path (directories use a directory symbolic link; creating symbolic links may need Developer Mode). Paste as hard link creates hard links for files; for a folder it recreates the tree and hard-links each file (same volume only). If the destination name already exists, the new link is named with a trailing ` 2`, ` 3`, ` 4`, … and the existing file is left unchanged. Link paste runs in the background and does not open the progress window.

Unregister from the Settings page. Register and unregister refresh the menu status automatically. After moving the EXE, unregister from the old location and register again from the new one. Re-register after changing icons or upgrading.

Paste/Cancel on a folder background follow the UI language immediately. The FastCopy submenu labels update when the language is changed.

## Command line

Headless copy, move, or delete:

```text
fastcopy.exe --copy SOURCE [SOURCE…] DEST_FOLDER [--workers N] [--ignore]
fastcopy.exe --move SOURCE [SOURCE…] DEST_FOLDER [--workers N] [--ignore]
fastcopy.exe --delete PATH [PATH…] [--permanent|--recycle-bin] [--yes] [--ignore]
```

| Flag | Meaning |
| --- | --- |
| `--workers N` | Worker thread count (1–64) |
| `--ignore` | Honor the ignore file name from Settings (currently only when copying folders) |
| `--permanent` / `--recycle-bin` | Delete mode (delete only) |
| `--yes` | Skip the permanent-delete confirmation |

Exit codes: `0` success, `1` skipped (conflict skip or unchanged), `2` failures, `3` cancelled, `64` usage error.

Other commands:

```text
fastcopy.exe --settings
fastcopy.exe --register-shell
fastcopy.exe --unregister-shell
```

`--settings` opens the Settings window. `--register-shell` / `--unregister-shell` add or remove the Explorer menu for the current user.

## Build

Requires Rust stable, Visual Studio C++ Build Tools / Windows SDK, and 7-Zip for packaging:

```powershell
cargo test
cargo build --release
node pack.js
```

The executable is `target/release/fastcopy.exe`. `node pack.js` builds Release and writes `release/fastcopy_1.0.2.7z` (the `x.x.x` comes from `Cargo.toml`). Release builds use OpenGL (glow), LTO, and symbol stripping to keep size down.

## Performance notes

Measured against Explorer copy/paste and permanent delete: [benchmark.md](benchmark.md).

On a same-volume HDD `node_modules` tree (4019 files, 61 MiB, average ~15.5 KiB), FastCopy 1.0.2 with 16 workers copied about **12×** faster than Explorer paste (median 0.93 s vs 11.47 s) and permanently deleted about **2×** faster (1.02 s vs 2.02 s). Explorer paste was timed with the same `IFileOperation` engine Explorer uses (no UI). A single large file is still usually limited by the disk and cache; FastCopy is not guaranteed to win that case.

- Many small files are limited by random disk I/O, antivirus scans, and filesystem metadata. Raising concurrency often helps.
- Copy uses concurrent `ReadFile`/`WriteFile` for small files, overlapped with the tree scan. Files ≥ 64 MiB run on one worker with unbuffered `CopyFileEx` (falls back if that flag is rejected).
- Avoid very high concurrency on HDDs; try 2–4. On SSD/NVMe, start from the default.
- Recycle Bin work is done by Windows Shell; progress updates after each top-level item.

## Current limits

- First release is Windows 10/11 x64 only.
- Ignore files apply only when copying folders, not when copying individual files or when moving/deleting.
- Hard links / symlinks / junctions default to skip; follow copies target content; preserve recreates links (directory junctions are recreated as junctions; creating symbolic links may need Developer Mode).
- Verification checks size, not a content hash.
- NTFS ACLs and alternate data streams are not copied. If the destination volume does not support sparse files, the copy is written as a normal file.
- In-use or sharing-locked files are skipped, listed at the end, and can be retried. Other permission or security failures stay in the same list.
