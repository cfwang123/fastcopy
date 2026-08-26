# FastCopy

[中文说明](README.zh.md)

A Windows 10/11 Rust file tool for copying, moving, and deleting large numbers of files. It uses a file-level concurrent queue, which works well for directories with many small files. Moves on the same volume prefer a filesystem rename.

## Features

- Copy, move, and delete files and folders
- Opening the app shows Settings; Explorer menu tasks show only the progress window
- Chinese/English UI language switch in Settings (labels in the language dropdown stay `中文` / `English`)
- Scan progress before copy/move/delete starts
- Choose files and destinations from the Explorer context menu
- Total bytes, file count, current file, speed, files per second, elapsed time, and time remaining
- Pause, resume, cancel, failed-item list, retry, and export
- Overwrite, skip, or rename on conflicts; optional skip when size and modified time match
- When copying folders, optional gitignore-style skip using a named ignore file (default `.gitignore`)
- Hard links, symbolic links, and directory junctions: ignore (default), follow target, or preserve as links
- Recycle Bin by default, with optional permanent delete
- Optional file-size check after copy; CopyFileEx keeps last-write time, last-access time, and basic attributes
- Files ≥ 64 MiB use a dedicated worker and unbuffered CopyFileEx
- Locked/in-use files are skipped and listed at the end for retry
- Explorer cascade for the current user (no admin): one FastCopy item with Quick Cut, Quick Copy, and Quick Delete in the submenu. Quick Paste and Cancel Cut/Copy appear on a folder background only after a cut or copy. Selection is read from Explorer, so more than ~100 top-level items can be included
- App icon and context-menu icons (cut, copy, paste, delete)
- Compact WinForms-like 9pt UI text and controls
- Window always opens centered on the current screen
- Toast notification when a task finishes; the progress window still closes on success

## Usage

1. Opening the app shows Settings immediately. Save settings and Close stay visible at the bottom.
2. In Explorer, use the FastCopy submenu (Quick Cut, Quick Copy, Quick Delete) on files or folders. After a cut or copy, use Quick Paste on a folder background, or Cancel Cut/Copy to clear the pending list.
3. After paste or delete starts, the app shows only the progress window (speed, time remaining, pause/cancel).
4. The window closes when a task finishes successfully and a toast is shown; it stays open if there were errors so you can retry or export the list.
5. Headless copy, move, or delete: `fastcopy.exe --copy SOURCE DEST_FOLDER [--workers N]`, `fastcopy.exe --move SOURCE DEST_FOLDER [--workers N]`, `fastcopy.exe --delete PATH [--permanent|--recycle-bin] [--yes]`.

Permanent delete does not use the Recycle Bin; the app asks for confirmation first (CLI: skip with `--yes`). Cancelling a copy removes unfinished new destination files. Overwrites write a temporary file first, then replace, so the original stays until replacement finishes.

## Explorer menu

In Settings, click **Register** to add the menu for the current Windows user (no administrator prompt). An older all-users install can still be removed with **Unregister all-users menu (admin)**. Quick Paste is not shown when files or folders are selected; it appears on a folder background only after a cut or copy, together with Cancel Cut/Copy. Windows 11 may put classic menus under **Show more options**.

Quick Copy and Quick Cut use FastCopy’s own clipboard and do not replace the Windows text clipboard. Quick Paste clears that clipboard, so you must cut or copy again to paste. When Explorer starts a menu command, the app reads the current Explorer selection so more than 100 top-level items can be included. Quick Delete uses the default delete mode saved in Settings.

Unregister from the Settings page. Register and unregister refresh the menu status automatically. After moving the EXE, unregister from the old location and register again from the new one. Re-register after changing icons or upgrading.

Paste/Cancel on a folder background follow the UI language immediately. The FastCopy submenu labels update when the language is changed.

## Build

Requires Rust stable and Visual Studio C++ Build Tools / Windows SDK:

```powershell
cargo test
cargo build --release
node pack.js
```

The executable is `target/release/fastcopy.exe`. `node pack.js` builds Release and writes `release/fastcopy_x.x.x.7z` (7-Zip required). Release builds use OpenGL (glow), LTO, and symbol stripping to keep size down.

## Performance notes

- Many small files are limited by random disk I/O, antivirus scans, and filesystem metadata. Raising concurrency often helps.
- Copy uses concurrent Windows `CopyFileEx` for small files. Files ≥ 64 MiB run on one worker with `COPY_FILE_NO_BUFFERING` (falls back if that flag is rejected).
- Avoid very high concurrency on HDDs; try 2–4. On SSD/NVMe, start from the default.
- A single large file usually runs near cache and device limits; it is not guaranteed to beat Explorer.
- Recycle Bin work is done by Windows Shell; progress updates after each top-level item.

## Current limits

- First release is Windows 10/11 x64 only.
- Ignore files apply only when copying folders, not when copying individual files or when moving/deleting.
- Hard links / symlinks / junctions default to skip; follow copies target content; preserve recreates links (directory junctions are recreated as junctions; creating symbolic links may need Developer Mode).
- Verification checks size, not a content hash.
- NTFS ACLs and sparse-file layout are not copied.
- In-use or sharing-locked files are skipped, listed at the end, and can be retried. Other permission or security failures stay in the same list.
