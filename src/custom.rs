use anyhow::{Context, Result, ensure};
use std::{
    ffi::CString,
    fs::{self, DirBuilder},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
};

const MARKER: &str = "PERSONAL_SESSION_CUSTOM_APP";

pub fn id(path: &str) -> String {
    format!("custom-{}", blake3::hash(path.as_bytes()).to_hex())
}

pub fn validate(path: &str) -> Result<()> {
    ensure!(
        personal_session::config::is_executable_path(path),
        "Choose an absolute executable path"
    );
    ensure!(
        fs::metadata(path)
            .with_context(|| format!("Inspecting {path}"))?
            .is_file(),
        "Choose an executable file"
    );
    let executable = CString::new(path)?;
    ensure!(
        unsafe { libc::access(executable.as_ptr(), libc::X_OK) } == 0,
        "Cannot execute {path}"
    );
    Ok(())
}

fn profile(mount: &str, id: &str) -> PathBuf {
    Path::new(mount).join("custom-apps").join(id)
}

fn private_dir(path: &Path) -> Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e).with_context(|| format!("Creating {}", path.display())),
    }
    let info = fs::symlink_metadata(path)?;
    ensure!(
        info.is_dir() && info.uid() == unsafe { libc::getuid() } && info.mode() & 0o077 == 0,
        "Expected a private directory at {}",
        path.display()
    );
    Ok(())
}

pub fn launch(path: &str, mount: &str) -> Result<()> {
    ensure!(
        personal_session::config::get().mount == mount,
        "Unexpected custom app mount"
    );
    personal_session::volume::verify()?;
    Err(command(path, mount)?.exec()).with_context(|| format!("Launching {path}"))
}

fn command(path: &str, mount: &str) -> Result<Command> {
    validate(path)?;
    let id = id(path);
    let root = profile(mount, &id);
    private_dir(root.parent().context("Missing custom apps directory")?)?;
    private_dir(&root)?;
    let mut command = Command::new(path);
    for (var, name) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("TMPDIR", "tmp"),
    ] {
        let directory = root.join(name);
        private_dir(&directory)?;
        command.env(var, directory);
    }
    command.current_dir(root.join("home")).env(MARKER, id);
    Ok(command)
}

pub fn owner_key(pid: i32, mount: &str) -> Option<String> {
    if pid <= 1 {
        return None;
    }
    let process = Path::new("/proc").join(pid.to_string());
    if !fs::metadata(&process).is_ok_and(|info| info.uid() == unsafe { libc::getuid() }) {
        return None;
    }
    let environment = fs::read(process.join("environ")).ok()?;
    environment_owner(&environment, mount).map(str::to_owned)
}

fn environment_owner<'a>(environment: &'a [u8], mount: &str) -> Option<&'a str> {
    let marker_prefix = concat!("PERSONAL_SESSION_CUSTOM_APP", "=").as_bytes();
    let mut marker = None;
    let mut home = None;
    for variable in environment.split(|byte| *byte == 0) {
        if let Some(value) = variable.strip_prefix(marker_prefix) {
            marker = Some(value);
        } else if let Some(value) = variable.strip_prefix(b"HOME=") {
            home = Some(value);
        }
        if marker.is_some() && home.is_some() {
            break;
        }
    }
    let id = std::str::from_utf8(marker?).ok()?;
    let digest = id.strip_prefix("custom-")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let expected = profile(mount, id).join("home");
    (home? == expected.as_os_str().as_encoded_bytes()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn executable_path_is_absolute_and_executable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("app with spaces");
        fs::write(&path, b"")?;
        let path_string = path.to_str().context("Temporary path is not UTF-8")?;
        assert!(validate(path_string).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        assert!(validate(path_string).is_ok());
        assert!(validate("relative/app").is_err());
        assert!(validate("/bin/../bin/sh").is_err());
        assert!(validate(temp.path().to_str().unwrap()).is_err());
        Ok(())
    }

    #[test]
    fn profile_ids_distinguish_paths_with_the_same_basename() {
        let key = id("/usr/bin/editor");
        assert_eq!(key, id("/usr/bin/editor"));
        assert_ne!(key, id("/opt/editor"));
        assert_eq!(key.len(), "custom-".len() + 64);
    }

    #[test]
    fn ownership_requires_exact_marker_and_home() {
        let id = id("/usr/bin/editor");
        let home = profile("/fixture/open", &id).join("home");
        let exact = format!("{MARKER}={id}\0HOME={}\0PATH=/bin\0", home.display());
        assert_eq!(
            environment_owner(exact.as_bytes(), "/fixture/open"),
            Some(id.as_str())
        );
        assert_eq!(environment_owner(exact.as_bytes(), "/other/open"), None);
        let forge = format!(
            "{MARKER}={}\0HOME={}\0",
            super::id("/opt/editor"),
            home.display()
        );
        assert_eq!(environment_owner(forge.as_bytes(), "/fixture/open"), None);
        let suffix = format!("{MARKER}={id}suffix\0HOME={}\0", home.display());
        assert_eq!(environment_owner(suffix.as_bytes(), "/fixture/open"), None);
    }

    #[test]
    fn recognizes_the_isolated_launch_of_an_executable() -> Result<()> {
        use std::{io::Read, process::Stdio};

        fn launched_owner(mut command: Command, mount: &str) -> Result<Option<String>> {
            let mut child = command
                .args(["-c", "printf x; read unused"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let ready = child.stdout.take().unwrap().read_exact(&mut [0]);
            let owner = owner_key(child.id().try_into()?, mount);
            drop(child.stdin.take());
            child.wait()?;
            ready?;
            Ok(owner)
        }

        let temp = tempfile::tempdir()?;
        let mount = temp
            .path()
            .to_str()
            .context("Temporary path is not UTF-8")?;
        let path = "/usr/bin/sh";
        let isolated_owner = launched_owner(command(path, mount)?, mount)?;
        let mut ordinary = Command::new(path);
        ordinary.env_remove(MARKER);
        let ordinary_owner = launched_owner(ordinary, mount)?;
        assert_eq!(isolated_owner, Some(id(path)));
        assert_eq!(ordinary_owner, None);
        Ok(())
    }

    #[test]
    fn refuses_redirected_profile_directory() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let private = temp.path().join("profile");
        let redirected = temp.path().join("redirected");
        private_dir(&private)?;
        private_dir(&private)?;
        symlink(private, &redirected)?;
        assert!(private_dir(&redirected).is_err());
        Ok(())
    }
}
