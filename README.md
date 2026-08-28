# FastCopy

Version **1.0.2**. [中文说明](README.zh.md) · [Changelog](CHANGELOG.md) (English, then Chinese, per version) · [Benchmark](benchmark.md)

A Windows 10/11 file tool for copying, moving, and deleting large numbers of files. It uses a file-level concurrent queue, which works well for directories with many small files. Moves on the same volume prefer a filesystem rename.

Same-volume HDD `node_modules` (4019 files, 61 MiB, average ~15.5 KiB), FastCopy 1.0.2 with 16 workers vs Explorer paste / permanent delete ([benchmark.md](benchmark.md)):

| | Explorer | FastCopy |
| --- | ---: | ---: |
| Copy (paste) | 11.47 s | **0.93 s (12×)** |
| Permanent delete | 2.02 s | **1.02 s (2×)** |

Explorer paste was timed with the same `IFileOperation` engine Explorer uses (no UI). A single large file is still usually limited by the disk and cache; FastCopy is not guaranteed to win that case.

## Install

1. Put `fastcopy.exe` in a folder that will stay put (not Downloads or a temporary extract you will delete). The Explorer menu records the full path of this EXE.
2. Start `fastcopy.exe`. Settings opens.
3. Click **Register** (current user, no administrator).

After that, Explorer shows a **FastCopy** submenu on selected files and folders, and **Quick Paste** / **Cancel Cut/Copy** on a folder background. Windows 11 may put classic menus under **Show more options**. If you move the EXE later, unregister from the old location, then start from the new path and register again.

## Selected files and folders

Right-click one or more files, folders, or a mix. Commands are under the **FastCopy** submenu. The app reads the current Explorer selection, so more than about 100 top-level items can be included.

- **Quick Cut** — Put the selection on FastCopy’s own clipboard (not the Windows text clipboard). Paste later on a folder background to move.
- **Quick Copy** — Same clipboard, paste later to copy.
- **Quick Delete** — Delete using the mode saved in Settings (Recycle Bin by default, or permanent delete with a confirmation).
- **Copy as symbolic link** — Remember the selection as a symbolic-link paste. Then use the folder-background paste command.
- **Copy as hard link** — Remember the selection as a hard-link paste. Then use the folder-background paste command.
- **Open link target** — Only when a single shortcut (`.lnk`), symbolic link, or directory junction is selected. Opens Explorer at the real target.
- **View source path** — Same single-link condition. Shows the path (the real target for those links) with Copy path and Open path.
- **Folder size** — Scan the selection and show file count, folder count, and bytes.
- **Copy paths** — Write one absolute path per line to the Windows text clipboard. Hold Shift for paths relative to the common parent.
- **Batch rename** — One window for the whole selection. Old and new name lists are editable (delete or reorder old-name lines to drop/reorder files). Opening fills Everything-style patterns: common prefix/suffix stay literal, the varying middle is `%1` (`*` in the old pattern is also a capture). In the new pattern, `%1` is that capture and `#` / `###` are numbers. Toggling Ignore extension rebuilds the pattern from the stems.
- **Settings** — Open FastCopy Settings (separator above this item).

## Folder background

Right-click empty space inside a folder. These items appear only after a Quick Cut, Quick Copy, or link-copy, and are hidden again after a normal paste or cancel.

- **Quick Paste** — Paste into this folder from FastCopy’s clipboard. A copy or cut opens the progress window. After a link-copy the label is **Paste as symbolic link** / **Paste as hard link**, or **Paste (N files) as …** when more than one item was copied; that paste runs in the background with no progress window. Hold Shift while clicking to keep the clipboard and paste again. A cut still clears it, because the source is gone.
- **Cancel Cut/Copy** — Clear the pending list without pasting.

Paste as symbolic link creates one link per top-level item pointing at the original path (directories use a directory symbolic link; creating symbolic links may need Developer Mode). Paste as hard link creates hard links for files; for a folder it recreates the tree and hard-links each file (same volume only). If the destination name already exists, the new link is named with a trailing ` 2`, ` 3`, ` 4`, … and the existing file is left unchanged.

## After a command runs

Opening the app with no menu task shows Settings. Save settings and Close stay visible at the bottom. Copy, move, and delete show only the progress window (scan, speed, time remaining, pause/cancel). The window closes on success; a toast is shown if that option is enabled in Settings (symbolic/hard-link paste has its own toggle). The toast is sent as FastCopy, not PowerShell. The window stays open if there were errors so you can retry or export the list.

Permanent delete does not use the Recycle Bin; the app asks for confirmation first (CLI: skip with `--yes`). Cancelling a copy removes unfinished new destination files. Overwrites write a temporary file first, then replace, so the original stays until replacement finishes.

Settings are stored in `%LOCALAPPDATA%\FastCopy\settings.json`.

## Register and language

An older all-users install can still be removed with **Unregister all-users menu (admin)**. Unregister from the Settings page. Register and unregister refresh the menu status automatically. New menu items and icons from an upgrade are repaired on the next launch.

Paste/Cancel on a folder background follow the UI language immediately. The FastCopy submenu labels update when the language is changed.

## Engine and settings

- Chinese/English UI language switch in Settings (dropdown labels stay `中文` / `English`). Window title bar shows the package version (e.g. `FastCopy 1.0.2`).
- Overwrite, skip, or rename on conflicts; optional skip when size and modified time match.
- When copying folders, optional gitignore-style skip using a named ignore file (default `.gitignore`).
- Hard links, symbolic links, and directory junctions: ignore (default), follow target, or preserve as links.
- Preserve NTFS sparse-file holes instead of writing them as real zeros.
- Optional file-size check after copy; last-write time, last-access time, and basic attributes are kept.
- Many small files copy with concurrent `ReadFile`/`WriteFile`, overlapped with the tree scan; files ≥ 64 MiB use a dedicated worker and unbuffered CopyFileEx.
- Locked/in-use files are skipped and listed at the end for retry.
- Compact WinForms-like 9pt UI; the window always opens centered on the current screen.

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
cargo test --release
cargo build --release
node pack.js
```

The executable is `target/release/fastcopy.exe`. `node pack.js` builds Release and writes `release/fastcopy_1.0.2.7z` (the `x.x.x` comes from `Cargo.toml`). Release builds use OpenGL (glow), LTO, and symbol stripping to keep size down.

## Performance notes

Headline numbers are at the top of this page. Method, environment, and per-round times: [benchmark.md](benchmark.md).

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
