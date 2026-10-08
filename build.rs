fn main() {
    println!("cargo:rerun-if-changed=assets/icons/app.ico");
    compile_shell_dll();
    spawn_copy_release_exe();
    let version = env!("CARGO_PKG_VERSION");
    let file_version = if version.split('.').count() == 3 {
        format!("{version}.0")
    } else {
        version.to_owned()
    };
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut resource = winres::WindowsResource::new();
    resource.set_icon(manifest.join("assets/icons/app.ico").to_str().unwrap());
    resource.set("ProductName", "FastCopy");
    resource.set("FileDescription", "FastCopy");
    resource.set("FileVersion", &file_version);
    resource.set("ProductVersion", version);
    resource
        .compile()
        .expect("failed to compile Windows resources");
}

fn compile_shell_dll() {
    println!("cargo:rerun-if-changed=shell_ext/fastcopy_shell.c");
    println!("cargo:rerun-if-changed=shell_ext/fastcopy_shell.def");
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("shell_ext/fastcopy_shell.c");
    let def = manifest.join("shell_ext/fastcopy_shell.def");
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile_dir = out_dir.ancestors().nth(3).expect("profile dir");
    let dll = profile_dir.join("fastcopy_shell.dll");
    let compiler = cc::Build::new().get_compiler();
    let mut cmd = std::process::Command::new(compiler.path());
    for (key, value) in compiler.env() {
        cmd.env(key, value);
    }
    if compiler.is_like_msvc() {
        cmd.args(["/nologo", "/LD", "/MT", "/O2", "/W3", "/DUNICODE", "/D_UNICODE"]);
        for arg in compiler.args() {
            let text = arg.to_string_lossy();
            if text.starts_with("/I") || text.starts_with("-I") {
                cmd.arg(arg);
            }
        }
        cmd.arg(&src);
        cmd.arg(format!("/Fe:{}", dll.display()));
        cmd.arg("/link");
        cmd.arg(format!("/DEF:{}", def.display()));
        cmd.args(["ole32.lib", "shell32.lib", "user32.lib", "gdi32.lib", "advapi32.lib", "uuid.lib"]);
    } else {
        cmd.args(["-shared", "-municode", "-O2", "-o"]);
        cmd.arg(&dll);
        cmd.arg(&src);
        cmd.args(["-lole32", "-lshell32", "-luser32", "-lgdi32", "-ladvapi32", "-luuid"]);
    }
    cmd.current_dir(&out_dir);
    let status = cmd.status().expect("failed to start compiler for fastcopy_shell.dll");
    assert!(status.success(), "failed to compile fastcopy_shell.dll: {status}");
    assert!(dll.is_file(), "compiler did not write {}", dll.display());
}

include!("copy_release_exe.rs");
