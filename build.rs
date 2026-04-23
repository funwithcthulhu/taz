fn main() {
    slint_build::compile("ui/app-window.slint").expect("failed to compile Slint UI");

    // Embed the app icon into the Windows executable
    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/taz.ico");
        res.compile().expect("failed to compile Windows resources");
    }
}
