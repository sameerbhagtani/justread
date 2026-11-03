fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // Checked at runtime, not with #[cfg]: build scripts are compiled for the host, not the target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            // Windows shows FileDescription as the app's name, e.g. in Task Manager.
            .set("ProductName", "JustRead")
            .set("FileDescription", "JustRead")
            .set("CompanyName", "Sameer Bhagtani")
            .compile()
            .expect("embedding the Windows icon failed");
    }
}
