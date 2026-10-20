// Build scripts run on the host, so check the target OS rather than cfg!(windows).
fn main() -> std::io::Result<()> {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/vmanager.ico")
            .compile()?;
    }
    Ok(())
}
