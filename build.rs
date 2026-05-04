fn main() {
    slint_build::compile("ui/app-window.slint").expect("failed to compile Slint UI");

    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/taz.ico");
        res.set("ProductName", "Taz Reader");
        res.set("FileDescription", "Taz Reader");
        res.set("InternalName", "Taz Reader");
        res.set("OriginalFilename", "taz-reader.exe");
        res.set("CompanyName", "Tom Boeding");
        res.set("LegalCopyright", "Copyright (C) 2026 Tom Boeding");
        res.compile().expect("failed to compile Windows resources");
    }
}
