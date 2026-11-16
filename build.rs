// Embeds the app icon and version info into aurolo.exe (shown in Windows search,
// the Start menu, Task Manager and Explorer).
fn main() {
    println!("cargo:rerun-if-changed=assets/aurolo.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/aurolo.ico");
        res.set("ProductName", "Aurolo");
        res.set("FileDescription", "Aurolo");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
