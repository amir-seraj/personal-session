use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};
const IDS: [&str; 4] = ["vscode", "chrome", "obsidian", "telegram"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Storage {
    container: String,
    mount: String,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Preferences {
    apps: Vec<String>,
    vault: String,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            apps: IDS.iter().map(|s| s.to_string()).collect(),
            vault: "Vault".into(),
        }
    }
}
pub struct Config {
    pub home: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub container: String,
    pub mount: String,
    pub vault: String,
    apps: Vec<String>,
}
impl Config {
    pub fn enabled(&self, id: &str) -> bool {
        self.apps.iter().any(|a| a == id)
    }
}
static CONFIG: OnceLock<Config> = OnceLock::new();
pub fn get() -> &'static Config {
    CONFIG
        .get()
        .expect("configuration initialized at entry point")
}
fn optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).with_context(|| format!("Invalid {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn load(privileged: bool) -> Result<()> {
    let uid = if privileged {
        ensure!(
            unsafe { libc::geteuid() } == 0,
            "Volume helper requires sudo"
        );
        std::env::var("SUDO_UID")
            .context("Invoke the helper through sudo as the volume owner")?
            .parse()?
    } else {
        unsafe { libc::getuid() }
    };
    ensure!(
        uid != 0 && (privileged || unsafe { libc::geteuid() } == uid),
        "Run Personal Session as a desktop user"
    );
    let passwd = crate::volume::output("/usr/bin/getent", &["passwd", &uid.to_string()])?;
    let fields: Vec<_> = passwd.split(':').collect();
    ensure!(
        fields.len() == 7 && fields[2].parse::<u32>()? == uid,
        "Unknown user"
    );
    let home = PathBuf::from(fields[5]);
    let gid = fields[3].parse()?;
    let base = format!("/var/lib/personal-session/{uid}");
    let settings = PathBuf::from(format!("/etc/personal-session/{uid}.json"));
    if settings.try_exists()? {
        trusted_path(&settings)?;
    }
    let storage = optional(&settings)?.unwrap_or(Storage {
        container: format!("{base}/volume.hc"),
        mount: format!("{base}/open"),
    });
    for s in [&storage.mount, &storage.container] {
        let p = Path::new(s);
        ensure!(
            p.is_absolute()
                && p.parent().is_some()
                && !s.chars().any(char::is_whitespace)
                && p.components()
                    .all(|c| matches!(c, Component::Normal(_) | Component::RootDir)),
            "Storage paths must be absolute, without whitespace or traversal"
        );
    }
    ensure!(
        storage.mount != storage.container,
        "Container and mount must differ"
    );
    let prefs = if privileged {
        Preferences::default()
    } else {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        optional(&dir.join("personal-session/config.json"))?.unwrap_or_default()
    };
    ensure!(
        !prefs.apps.is_empty() && prefs.apps.iter().all(|a| IDS.contains(&a.as_str())),
        "Select one or more supported apps"
    );
    ensure!(
        !prefs.vault.is_empty()
            && Path::new(&prefs.vault)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "vault must be relative to the mounted volume"
    );
    CONFIG
        .set(Config {
            home,
            uid,
            gid,
            container: storage.container,
            mount: storage.mount,
            vault: prefs.vault,
            apps: prefs.apps,
        })
        .map_err(|_| anyhow::anyhow!("Configuration already set"))
}
// The root helper accepts only storage locations controlled by the administrator.
pub fn trusted_path(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        let m =
            fs::symlink_metadata(part).with_context(|| format!("Inspecting {}", part.display()))?;
        ensure!(
            m.uid() == 0 && m.mode() & 0o022 == 0 && !m.file_type().is_symlink(),
            "Untrusted storage or config path: {}",
            part.display()
        );
    }
    Ok(())
}
#[cfg(test)]
pub fn test_config() {
    CONFIG.get_or_init(|| Config {
        home: "/home/example".into(),
        uid: 1000,
        gid: 1000,
        container: "/fixture/test.hc".into(),
        mount: "/fixture/open".into(),
        vault: "Vault".into(),
        apps: IDS.iter().map(|s| s.to_string()).collect(),
    });
}
