// Embeds the app icon and version info into aurolo.exe (shown in Windows search,
// the Start menu, Task Manager and Explorer).
//
// A personal icon can be dropped in as assets/aurolo-custom.ico: it is used instead of
// the default one and is git-ignored, so it never ends up in the repo or a release.
fn main() {
    println!("cargo:rerun-if-changed=assets/aurolo.ico");
    println!("cargo:rerun-if-changed=assets/aurolo-custom.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = if std::path::Path::new("assets/aurolo-custom.ico").exists() {
            "assets/aurolo-custom.ico"
        } else {
            "assets/aurolo.ico"
        };
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon);
        res.set("ProductName", "Aurolo");
        res.set("FileDescription", "Aurolo");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
