use std::path::{Path, PathBuf};
use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IServiceProvider, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    IFolderView, IShellBrowser, IShellItem, IShellItemArray, IShellWindows, IWebBrowserApp,
    ShellWindows, SIGDN_FILESYSPATH, SVGIO_SELECTION, SID_STopLevelBrowser,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsChild};

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
