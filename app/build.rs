// Windows: embeds the application icon and metadata into the desktop executable.
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() && std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../desktop/assets/icon.ico");
        res.set("ProductName", "Орловский кинопоказ");
        res.set("FileDescription", "Орловский кинопоказ");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}
