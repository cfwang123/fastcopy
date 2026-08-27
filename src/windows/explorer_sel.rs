use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::core::{HSTRING, Interface, VARIANT};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, IServiceProvider, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::{
    IContextMenu, IFolderView, IShellBrowser, IShellFolder, IShellItem, IShellItemArray, IShellLinkW,
    IShellWindows, IWebBrowserApp, ShellLink, ShellWindows, BHID_SFObject, CMF_EXPLORE, CMF_NORMAL,
    SHCreateItemFromParsingName, SIGDN_FILESYSPATH, SVGIO_SELECTION, SID_STopLevelBrowser,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetForegroundWindow, GetMenuItemCount, GetMenuStringW, IsChild,
    MF_BYPOSITION,
};

pub fn selected_paths(clicked: &Path) -> Vec<PathBuf> {
    match selected_paths_from_explorer() {
        Ok(paths) if !paths.is_empty() => {
            if paths.iter().any(|path| path == clicked) {
                paths
            } else {
                let mut paths = paths;
                paths.push(clicked.to_path_buf());
                paths
            }
        }
        _ => vec![clicked.to_path_buf()],
    }
}

fn selected_paths_from_explorer() -> windows::core::Result<Vec<PathBuf>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let foreground = GetForegroundWindow();
        let shell_windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let count = shell_windows.Count()?;
        let mut fallback = Vec::new();
        for index in 0..count {
            let Ok(dispatch) = shell_windows.Item(&VARIANT::from(index)) else {
                continue;
            };
            let Ok(browser) = dispatch.cast::<IWebBrowserApp>() else {
                continue;
            };
            let Ok(explorer_hwnd) = browser.HWND() else {
                continue;
            };
            let explorer = HWND(explorer_hwnd.0);
            let matched = explorer == foreground
                || IsChild(explorer, foreground).as_bool()
                || IsChild(foreground, explorer).as_bool();
            let Ok(paths) = selection_from_browser(&browser) else {
                continue;
            };
            if paths.is_empty() {
                continue;
            }
            if matched {
                return Ok(paths);
            }
            if fallback.is_empty() {
                fallback = paths;
            }
        }
        Ok(fallback)
    }
}

fn selection_from_browser(browser: &IWebBrowserApp) -> windows::core::Result<Vec<PathBuf>> {
    unsafe {
        let provider: IServiceProvider = browser.cast()?;
        let shell_browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser)?;
        let view = shell_browser.QueryActiveShellView()?;
        let folder_view: IFolderView = view.cast()?;
        let items: IShellItemArray = folder_view.Items(SVGIO_SELECTION)?;
        let count = items.GetCount()?;
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let item: IShellItem = items.GetItemAt(index)?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            if name.is_null() {
                continue;
            }
            if let Ok(text) = name.to_string() {
                if !text.is_empty() {
                    paths.push(PathBuf::from(text));
                }
            }
            CoTaskMemFree(Some(name.0.cast()));
        }
        Ok(paths)
    }
}

pub fn background_menu_labels(folder: &Path) -> windows::core::Result<Vec<String>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let item: IShellItem =
            SHCreateItemFromParsingName(&HSTRING::from(folder.to_string_lossy().as_ref()), None)?;
        let shell_folder: IShellFolder = item.BindToHandler(None, &BHID_SFObject)?;
        let menu: IContextMenu = shell_folder.CreateViewObject(HWND(0))?;
        let hmenu = CreatePopupMenu()?;
        let result = menu.QueryContextMenu(hmenu, 0, 1, 0x7FFF, CMF_NORMAL | CMF_EXPLORE);
        let mut labels = Vec::new();
        if result.is_ok() {
            let count = GetMenuItemCount(hmenu);
            for index in 0..count {
                let mut buffer = [0u16; 512];
                let copied = GetMenuStringW(hmenu, index as u32, Some(&mut buffer), MF_BYPOSITION);
                if copied <= 0 {
                    continue;
                }
                let text = String::from_utf16_lossy(&buffer[..copied as usize]);
                let text = text.replace('&', "").replace('\u{8}', "");
                let text = text.split('\t').next().unwrap_or(&text).trim();
                if !text.is_empty() {
                    labels.push(text.to_owned());
                }
            }
        }
        let _ = DestroyMenu(hmenu);
        result?;
        Ok(labels)
    }
}

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub fn resolve_link_target(path: &Path) -> Option<PathBuf> {
    if is_shortcut(path) {
        return resolve_shortcut(path);
    }
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let is_reparse = metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    if !metadata.file_type().is_symlink() && !is_reparse {
        return None;
    }
    let raw = std::fs::read_link(path).ok()?;
    Some(absolute_from_link(path, raw))
}

pub fn display_path(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    strip_extended_prefix(&absolute)
}

pub fn source_path_for(path: &Path) -> PathBuf {
    resolve_link_target(path).unwrap_or_else(|| match std::fs::canonicalize(path) {
        Ok(canonical) => strip_extended_prefix(&canonical),
        Err(_) => display_path(path),
    })
}

pub fn reveal_path(path: &Path) -> std::io::Result<()> {
    let path = strip_extended_prefix(path);
    if path.is_dir() {
        Command::new("explorer.exe").arg(&path).spawn()?;
    } else {
        Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .spawn()?;
    }
    Ok(())
}

fn is_shortcut(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
}

fn absolute_from_link(link: &Path, target: PathBuf) -> PathBuf {
    let absolute = if target.is_absolute() {
        target
    } else if let Some(parent) = link.parent() {
        parent.join(target)
    } else {
        target
    };
    match std::fs::canonicalize(&absolute) {
        Ok(canonical) => strip_extended_prefix(&canonical),
        Err(_) => absolute,
    }
}

fn strip_extended_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
        path.to_path_buf()
    }
}

fn resolve_shortcut(path: &Path) -> Option<PathBuf> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_ALL).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist
            .Load(
                &HSTRING::from(path.to_string_lossy().as_ref()),
                STGM_READ,
            )
            .ok()?;
        let mut buffer = [0u16; 2048];
        link.GetPath(&mut buffer, std::ptr::null_mut(), 0).ok()?;
        let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
        if len == 0 {
            return None;
        }
        let target = PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
        Some(absolute_from_link(path, target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolve_symlink_file_when_supported() {
        let root = tempdir().unwrap();
        let source = root.path().join("源.txt");
        let link = root.path().join("链接.txt");
        std::fs::write(&source, b"data").unwrap();
        if std::os::windows::fs::symlink_file(&source, &link).is_err() {
            return;
        }
        let target = resolve_link_target(&link).expect("symlink target");
        assert_eq!(
            std::fs::canonicalize(&target).unwrap(),
            std::fs::canonicalize(&source).unwrap()
        );
    }

    #[test]
    fn resolve_regular_file_is_none() {
        let root = tempdir().unwrap();
        let file = root.path().join("普通.txt");
        std::fs::write(&file, b"data").unwrap();
        assert!(resolve_link_target(&file).is_none());
    }

    #[test]
    fn resolve_relative_symlink_when_supported() {
        let root = tempdir().unwrap();
        let source = root.path().join("源.txt");
        let link = root.path().join("链接.txt");
        std::fs::write(&source, b"data").unwrap();
        if std::os::windows::fs::symlink_file("源.txt", &link).is_err() {
            return;
        }
        let target = resolve_link_target(&link).expect("relative symlink");
        assert!(target.ends_with("源.txt"));
        assert_eq!(std::fs::read(&target).unwrap(), b"data");
    }

    #[test]
    fn source_path_for_regular_file_is_itself() {
        let root = tempdir().unwrap();
        let file = root.path().join("普通.txt");
        std::fs::write(&file, b"data").unwrap();
        let source = source_path_for(&file);
        assert_eq!(
            std::fs::canonicalize(&source).unwrap(),
            std::fs::canonicalize(&file).unwrap()
        );
        assert_eq!(source_path_for(&file), display_path(&file));
    }

    #[test]
    fn source_path_for_symlink_is_target_when_supported() {
        let root = tempdir().unwrap();
        let source = root.path().join("源.txt");
        let link = root.path().join("链接.txt");
        std::fs::write(&source, b"data").unwrap();
        if std::os::windows::fs::symlink_file(&source, &link).is_err() {
            return;
        }
        let shown = source_path_for(&link);
        assert_eq!(
            std::fs::canonicalize(&shown).unwrap(),
            std::fs::canonicalize(&source).unwrap()
        );
        assert_ne!(
            display_path(&link).to_string_lossy().to_ascii_lowercase(),
            shown.to_string_lossy().to_ascii_lowercase()
        );
    }
}
