use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{OnceLock, RwLock},
};
const IDS: [&str; 4] = ["vscode", "chrome", "obsidian", "telegram"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Storage {
    container: String,
    mount: String,
}
#[derive(Deserialize, Serialize)]
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
    apps: RwLock<Vec<String>>,
    preferences_path: Option<PathBuf>,
}
impl Config {
    pub fn enabled(&self, id: &str) -> bool {
        self.apps
            .read()
            .expect("app selection lock")
            .iter()
            .any(|app| app == id)
    }

    pub fn selection(&self) -> Vec<String> {
        self.apps.read().expect("app selection lock").clone()
    }

    pub fn refresh_selection(&self) -> Result<()> {
        let path = self
            .preferences_path
            .as_deref()
            .context("User preferences are unavailable")?;
        let mut apps = self.apps.write().expect("app selection lock");
        *apps = read_preferences(path)?.apps;
        Ok(())
    }

    pub fn save_selection(&self, expected: &[String], next: &[String]) -> Result<()> {
        let path = self
            .preferences_path
            .as_deref()
            .context("User preferences are unavailable")?;
        save_selection(path, expected, next, &self.apps)
    }
}

fn save_selection(
    path: &Path,
    expected: &[String],
    next: &[String],
    current: &RwLock<Vec<String>>,
) -> Result<()> {
    validate_apps(next)?;
    let mut seen = HashSet::new();
    ensure!(
        next.iter().all(|app| seen.insert(app)),
        "Select each app only once."
    );
    let mut current = current.write().expect("app selection lock");
    let mut prefs = read_preferences(path)?;
    ensure!(
        prefs.apps == expected,
        "App selection changed on disk. Reopen Personal Session and try again."
    );
    prefs.apps = next.to_vec();
    let parent = path.parent().context("Missing preferences directory")?;
    fs::create_dir_all(parent).with_context(|| format!("Creating {}", parent.display()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, &prefs)?;
    writeln!(file)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .with_context(|| format!("Saving {}", path.display()))?;
    *current = prefs.apps;
    Ok(())
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

pub fn is_executable_path(path: &str) -> bool {
    path.strip_prefix('/')
        .is_some_and(|tail| tail.split('/').all(|s| !matches!(s, "" | "." | "..")))
        && !path.chars().any(char::is_control)
}
fn validate_apps(apps: &[String]) -> Result<()> {
    for app in apps {
        ensure!(
            IDS.contains(&app.as_str()) || is_executable_path(app),
            "Invalid app selection {app:?}. Use a supported app ID or an absolute executable path."
        );
    }
    Ok(())
}

fn read_preferences(path: &Path) -> Result<Preferences> {
    let mut prefs: Preferences = optional(path)?.unwrap_or_default();
    validate_apps(&prefs.apps)?;
    let mut seen = HashSet::new();
    prefs.apps.retain(|app| seen.insert(app.clone()));
    ensure!(
        !prefs.vault.is_empty()
            && Path::new(&prefs.vault)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "vault must be relative to the mounted volume"
    );
    Ok(prefs)
}

pub fn load(privileged: bool) -> Result<()> {
    load_config(privileged, !privileged)
}
pub fn load_for_stop() -> Result<()> {
    load_config(false, false)
}
fn load_config(privileged: bool, read_apps: bool) -> Result<()> {
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
    let preferences_path = (!privileged).then(|| {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        dir.join("personal-session/config.json")
    });
    let prefs = preferences_path
        .as_deref()
        .filter(|_| read_apps)
        .map(read_preferences)
        .transpose()?
        .unwrap_or_default();
    CONFIG
        .set(Config {
            home,
            uid,
            gid,
            container: storage.container,
            mount: storage.mount,
            vault: prefs.vault,
            apps: RwLock::new(prefs.apps),
            preferences_path,
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
        apps: RwLock::new(Preferences::default().apps),
        preferences_path: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_accepts_legacy_ids_empty_lists_and_missing_executables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(read_preferences(&path).unwrap().apps, IDS);

        let current = RwLock::new(Preferences::default().apps);
        let expected = current.read().unwrap().clone();
        let next = ["chrome".into(), "/missing/Personal Tool".into()];
        save_selection(&path, &expected, &next, &current).unwrap();
        assert_eq!(read_preferences(&path).unwrap().apps, next);
        assert_eq!(*current.read().unwrap(), next);

        save_selection(&path, &next, &[], &current).unwrap();
        assert!(read_preferences(&path).unwrap().apps.is_empty());
        assert!(current.read().unwrap().is_empty());
    }

    #[test]
    fn failed_saves_keep_current_selection_and_preserve_disk_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = r#"{"apps":["chrome","chrome"],"vault":"Notes/Private"}"#;
        fs::write(&path, original).unwrap();
        let current = RwLock::new(vec!["vscode".into()]);
        let expected = current.read().unwrap().clone();

        assert!(save_selection(&path, &expected, &[], &current).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(*current.read().unwrap(), expected);

        let expected = ["chrome".into()];
        for invalid in ["terminal", "bin/tool", "/", "/usr/../tool", "/bin/\0tool"] {
            assert!(save_selection(&path, &expected, &[invalid.into()], &current).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
        let duplicate = ["chrome".into(), "chrome".into()];
        assert!(save_selection(&path, &expected, &duplicate, &current).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        save_selection(&path, &expected, &[], &current).unwrap();
        assert_eq!(read_preferences(&path).unwrap().vault, "Notes/Private");
    }
}
