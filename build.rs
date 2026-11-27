fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/tide.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/tide.ico")
            .set("FileDescription", "Tide")
            .set("ProductName", "Tide")
            .compile()?;
    }

    Ok(())
}
