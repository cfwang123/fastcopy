fn main() {
    println!("cargo:rerun-if-changed=assets/icons/app.ico");
    let version = env!("CARGO_PKG_VERSION");
    let file_version = if version.split('.').count() == 3 {
        format!("{version}.0")
    } else {
        version.to_owned()
    };
    let mut resource = winres::WindowsResource::new();
    resource.set_icon("assets/icons/app.ico");
    resource.set("ProductName", "FastCopy");
    resource.set("FileDescription", "FastCopy");
    resource.set("FileVersion", &file_version);
    resource.set("ProductVersion", version);
    resource
        .compile()
        .expect("failed to compile Windows resources");
}
