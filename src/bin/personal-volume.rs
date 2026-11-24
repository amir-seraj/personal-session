use anyhow::{Result, bail, ensure};
fn run() -> Result<()> {
    let exe = std::env::args_os().next().unwrap_or_default();
    let name = std::path::Path::new(&exe)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    ensure!(
        std::env::args_os().len() == 1,
        "This helper accepts no arguments."
    );
    let mount = match name {
        "personal-mount" => true,
        "personal-umount" => false,
        _ => bail!("Invoke as personal-mount or personal-umount."),
    };
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::process::CommandExt;
        return Err(std::process::Command::new("/usr/bin/sudo")
            .arg(format!("/usr/local/bin/{name}"))
            .exec()
            .into());
    }
    personal_session::config::load(true)?;
    personal_session::volume::helper(mount)
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e:#}");
        std::process::exit(1)
    }
}
