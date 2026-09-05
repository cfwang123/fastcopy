fn spawn_copy_release_exe() {
    println!("cargo:rerun-if-changed=copy_release_exe.py");
    #[cfg(windows)]
    spawn_copy_release_exe_windows();
}

#[cfg(windows)]
fn spawn_copy_release_exe_windows() {
    if std::env::var("PROFILE").unwrap_or_default() != "release" {
        return;
    }
    let pkg = match std::env::var("CARGO_PKG_NAME") {
        Ok(n) if !n.is_empty() => n,
        _ => return,
    };
    let manifest = match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(p) => std::path::PathBuf::from(p),
        None => return,
    };
    let out_dir = match std::env::var_os("OUT_DIR") {
        Some(p) => std::path::PathBuf::from(p),
        None => return,
    };
    let script = manifest.join("copy_release_exe.py");
    if !script.is_file() {
        println!("cargo:warning=missing copy_release_exe.py; skip copying release exe/dll");
        return;
    }
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let exe_name = format!("{pkg}.exe");
    let src = lexical_normalize(&profile_dir.join(&exe_name));
    let dst = lexical_normalize(&manifest.join("release").join(&exe_name));
    let src_s = win_path(&src);
    let dst_s = win_path(&dst);
    let Some(script_s) = script.to_str() else {
        return;
    };
    let initial_mtime_ms = file_mtime_ms(&src).unwrap_or(0);
    let cmd = format!(
        "Start-Process -FilePath python -WindowStyle Hidden -ArgumentList {x},{utf},{script},{src},{dst},{mtime}",
        x = ps_quote("-X"),
        utf = ps_quote("utf8"),
        script = ps_quote(script_s),
        src = ps_quote(&src_s),
        dst = ps_quote(&dst_s),
        mtime = ps_quote(&initial_mtime_ms.to_string()),
    );
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &cmd])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(windows)]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(windows)]
fn win_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.replace('/', "\\")
}

#[cfg(windows)]
fn lexical_normalize(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                let _ = out.pop();
            }
            std::path::Component::CurDir => {}
            rest => out.push(rest),
        }
    }
    out
}

#[cfg(windows)]
fn file_mtime_ms(path: &std::path::Path) -> Option<u128> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis())
}
