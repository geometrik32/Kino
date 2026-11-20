// Windows: puts the icon and the product name into the .exe of the desktop app.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../desktop/assets/icon.ico");
        res.set("ProductName", "Орловский кинопоказ");
        res.set("FileDescription", "Орловский кинопоказ");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}
