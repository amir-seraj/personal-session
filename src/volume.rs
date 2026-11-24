use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use crate::config::{get, trusted_path};
pub fn bounded(program: &str, args: &[&str], seconds: u32) -> Result<Output> {
    Command::new("/usr/bin/timeout")
        .args(["-k", "2"])
        .arg(seconds.to_string())
        .arg(program)
        .args(args)
        .output()
        .with_context(|| format!("Running {program}"))
}
pub fn output(program: &str, args: &[&str]) -> Result<String> {
    let o = bounded(program, args, 8)?;
    ensure!(
        o.status.success(),
        "{program}: {} ({})",
        String::from_utf8_lossy(&o.stderr).trim(),
        o.status
    );
    Ok(String::from_utf8(o.stdout)?.trim().to_owned())
}
pub fn mounted() -> Result<bool> {
    Ok(fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .any(|l| l.split_whitespace().nth(4) == Some(get().mount.as_str())))
}
pub fn verify() -> Result<PathBuf> {
    ensure!(mounted()?, "Personal filesystem is unmounted.");
    let table = output(
        "/usr/bin/findmnt",
        &["-n", "-o", "SOURCE,FSROOT", "--mountpoint", &get().mount],
    )?;
    let volumes = output(
        "/usr/bin/veracrypt",
        &["--text", "--non-interactive", "--list"],
    )?;
    let dev = verified_device(&table, &volumes)
        .context("Cannot verify the expected personal VeraCrypt volume")?;
    let source = fs::metadata(table.split_whitespace().next().unwrap())?;
    let meta = fs::metadata(&dev)?;
    ensure!(
        meta.file_type().is_block_device()
            && source.file_type().is_block_device()
            && source.rdev() == meta.rdev(),
        "Unexpected backing device."
    );
    let path = std::ffi::CString::new(get().mount.as_str())?;
    let writable = unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0;
    ensure!(writable, "Personal filesystem is not writable.");
    Ok(dev)
}
fn verified_device(table: &str, volumes: &str) -> Option<PathBuf> {
    let fields: Vec<_> = table.split_whitespace().collect();
    if fields.len() != 2 || fields[1] != "/" {
        return None;
    }
    volumes.lines().find_map(|line| {
        let f: Vec<_> = line.split_whitespace().collect();
        (f.len() == 4 && f[1] == get().container && f[3] == get().mount)
            .then(|| PathBuf::from(f[2]))
    })
}
pub fn helper(mount: bool) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "Run the installed volume helper through sudo."
    );
    std::env::set_current_dir("/")?;
    let cfg = get();
    trusted_path(
        Path::new(&cfg.mount)
            .parent()
            .context("Missing storage parent")?,
    )?;
    if mount {
        if mounted()? {
            verify()?;
            println!("Already mounted.");
            return Ok(());
        }
        trusted_path(Path::new(&cfg.container))?;
        trusted_path(Path::new(&cfg.mount))?;
        ensure!(
            fs::metadata(&cfg.container)?.is_file() && fs::metadata(&cfg.mount)?.is_dir(),
            "Invalid container or mount directory"
        );
        ensure!(
            Command::new("/usr/bin/veracrypt")
                .args([
                    "--text",
                    "--mount",
                    &cfg.container,
                    &cfg.mount,
                    "--pim=0",
                    "--keyfiles=",
                    "--protect-hidden=no"
                ])
                .status()?
                .success(),
            "Unlock failed."
        );
        verify()?;
        ensure!(
            Command::new("/usr/bin/chown")
                .args([&format!("{}:{}", cfg.uid, cfg.gid), &get().mount])
                .status()?
                .success(),
            "Cannot set volume owner."
        );
        let dirs = [
            "vscode",
            "vscode-ext",
            "vscode-shared",
            "chrome",
            "obsidian",
            "telegram",
            "claude",
            "codex",
        ];
        ensure!(
            Command::new("/usr/bin/sudo")
                .args(["-u", &format!("#{}", cfg.uid), "/usr/bin/mkdir", "-p"])
                .args(dirs.map(|d| Path::new(&cfg.mount).join(d)))
                .status()?
                .success(),
            "Cannot prepare app directories."
        );
    } else {
        if !mounted()? {
            println!("Personal filesystem is unmounted.");
            return Ok(());
        }
        let dev = verify()?;
        let holders = bounded("/usr/bin/fuser", &["-mM", &get().mount], 4)?;
        match holders.status.code() {
            Some(0) => {
                let pids = String::from_utf8_lossy(&holders.stdout);
                let names = pids
                    .split_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok())
                    .map(|p| {
                        let comm =
                            fs::read_to_string(format!("/proc/{p}/comm")).unwrap_or_default();
                        format!("{} ({p})", comm.trim())
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("Volume busy: {names}. Close the holders or cd out of the volume.")
            }
            Some(1) => (),
            _ => bail!("Holder check failed: {}", holders.status),
        }
        let o = bounded(
            "/usr/bin/veracrypt",
            &["--text", "--non-interactive", "--unmount", &get().mount],
            25,
        )?;
        ensure!(
            o.status.success(),
            "VeraCrypt did not confirm a clean dismount: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        ensure!(
            !mounted()? && !dev.exists(),
            "VeraCrypt mapping is still present."
        );
        println!("Sealed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_other_container_and_submount() {
        crate::config::test_config();
        let good = format!(
            "1: {} /dev/mapper/veracrypt1 {}",
            get().container,
            get().mount
        );
        assert!(verified_device("/dev/dm-1 /", &good).is_some());
        assert!(verified_device("/dev/dm-1 /subdir", &good).is_none());
        assert!(
            verified_device("/dev/dm-1 /", &good.replace(&get().container, "/other.hc")).is_none()
        );
    }
}
