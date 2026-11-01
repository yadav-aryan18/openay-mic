//! Windows-only build script: embed the logo icon (assets/icons/
//! openay-mic.ico) into the executable so Explorer and the taskbar show it
//! before/without a window. Requires a resource compiler (rc.exe on the
//! MSVC toolchain); on hosts without one (cross builds), the embed is
//! skipped with a warning — the window/taskbar icon still shows the logo
//! at runtime via the in-app window icon.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icons/openay-mic.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icons/openay-mic.ico");
    if let Err(e) = res.compile() {
        println!(
            "cargo:warning=openay-gui: exe icon not embedded (no resource              compiler on this host): {e}"
        );
    }
}
