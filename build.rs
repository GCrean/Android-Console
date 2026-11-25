fn main() {
    // Embed the application icon (resource id 1, which gpui uses for the
    // window/taskbar icon) and version info into the Windows executable.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icons/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icons/icon.ico");
        res.set("ProductName", "Android Console");
        res.set("FileDescription", "Android Console - ADB shell terminal");
        if let Err(e) = res.compile() {
            // Not fatal: the app still works without an embedded icon.
            println!("cargo:warning=could not embed Windows icon: {e}");
        }
    }
}
