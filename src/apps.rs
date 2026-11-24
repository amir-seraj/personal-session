use crate::volume;
use anyhow::{Context, Result, bail, ensure};
use personal_session::config::get;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::Path,
    process::Command,
};

pub const WORKSPACE: &str = "special:special";
pub const N: usize = 4;
pub struct App {
    pub name: &'static str,
    pub id: &'static str,
    pub icon: &'static str,
    classes: &'static [&'static str],
    exes: &'static [&'static str],
    args: &'static [&'static str],
}
pub const APPS: [App; N] = [
    App {
        name: "VS Code",
        id: "vscode",
        icon: "visual-studio-code",
        classes: &["com.microsoft.vscode", "code"],
        exes: &["code"],
        args: &["--new-window", "--ozone-platform=wayland"],
    },
    App {
        name: "Chrome",
        id: "chrome",
        icon: "google-chrome",
        classes: &["google-chrome", "chrome", "com.google.chrome"],
        exes: &["chrome", "google-chrome-stable"],
        args: &[
            "--ozone-platform=wayland",
            "--no-first-run",
            "--new-window",
            "about:blank",
        ],
    },
    App {
        name: "Obsidian",
        id: "obsidian",
        icon: "obsidian",
        classes: &["md.obsidian.obsidian", "obsidian"],
        exes: &["electron", "obsidian"],
        args: &[],
    },
    App {
        name: "Telegram",
        id: "telegram",
        icon: "telegram",
        classes: &["org.telegram.desktop", "telegram", "telegramdesktop"],
        exes: &["telegram", "telegram-desktop"],
        args: &[],
    },
];
#[derive(Deserialize, Clone)]
pub struct Workspace {
    pub name: String,
}
#[derive(Deserialize, Clone)]
pub struct Window {
    pub address: String,
    pub workspace: Workspace,
    pub class: String,
    #[serde(rename = "initialClass", default)]
    initial_class: String,
    pub mapped: bool,
    pub pid: i32,
}
pub fn clients() -> Result<Vec<Window>> {
    Ok(serde_json::from_str(&volume::output(
        "/usr/bin/hyprctl",
        &["-j", "clients"],
    )?)?)
}
struct Process {
    exe: String,
    args: Vec<String>,
    parent: i32,
}
fn proc_info(pid: i32) -> Option<Process> {
    let p = Path::new("/proc").join(pid.to_string());
    if fs::metadata(&p).ok()?.uid() != unsafe { libc::getuid() } {
        return None;
    }
    let exe = fs::read_link(p.join("exe"))
        .ok()?
        .file_name()?
        .to_string_lossy()
        .to_ascii_lowercase();
    let raw = fs::read(p.join("cmdline")).ok()?;
    let mut args: Vec<_> = raw
        .split(|b| *b == 0)
        .filter(|v| !v.is_empty())
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .collect();
    if args.len() == 1 {
        args = args[0].split_whitespace().map(str::to_owned).collect()
    }
    let stat = fs::read_to_string(p.join("stat")).ok()?;
    let parent = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(Process { exe, args, parent })
}
impl App {
    fn owns_profile(&self, p: &Process, mount: &str) -> bool {
        if !self.exes.contains(&p.exe.as_str()) {
            return false;
        }
        let flag = if self.id == "telegram" {
            "-workdir"
        } else {
            "--user-data-dir"
        };
        let profile = format!("{mount}/{}", self.id);
        let joined = format!("{flag}={profile}");
        p.args.iter().any(|a| a == &joined)
            || p.args.windows(2).any(|a| a[0] == flag && a[1] == profile)
    }
    fn matches(&self, class: &str) -> bool {
        let c = class.to_ascii_lowercase();
        self.classes.contains(&c.as_str())
            || (self.id == "telegram" && c.starts_with("org.telegram.desktop."))
    }
}
#[derive(Clone)]
pub struct Entry {
    pub key: String,
    pub name: String,
    token: String,
}
impl Entry {
    fn new(key: String) -> Self {
        let name = APPS
            .iter()
            .find(|app| app.id == key)
            .map(|a| a.name)
            .or_else(|| Path::new(&key).file_name()?.to_str())
            .unwrap_or(&key)
            .to_owned();
        let token = if key.starts_with('/') {
            crate::custom::id(&key)
        } else {
            key.clone()
        };
        Self { key, name, token }
    }
    fn builtin(&self) -> Option<&'static App> {
        APPS.iter().find(|app| app.id == self.key)
    }
    fn owns(&self, process: &Process, custom_owner: Option<&str>) -> bool {
        match self.builtin() {
            Some(app) => app.owns_profile(process, &get().mount),
            None => custom_owner == Some(self.token.as_str()),
        }
    }
}
pub fn entries() -> Vec<Entry> {
    get().selection().into_iter().map(Entry::new).collect()
}
// Cleanup follows profile identity even if the preference file was edited during a session.
pub fn cleanup_entries() -> Result<Vec<Entry>> {
    let mut result = entries();
    for app in &APPS {
        if !result.iter().any(|entry| entry.key == app.id) {
            result.push(Entry::new(app.id.to_owned()));
        }
    }
    let directory = Path::new(&get().mount).join("custom-apps");
    match fs::read_dir(directory) {
        Ok(children) => {
            for child in children {
                let token = child?.file_name().to_string_lossy().into_owned();
                if token
                    .strip_prefix("custom-")
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    && !result.iter().any(|entry| entry.token == token)
                {
                    result.push(Entry {
                        key: token.clone(),
                        name: "Custom app".into(),
                        token,
                    });
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(result)
}
pub type Groups = Vec<Vec<Window>>;
pub fn windows(entries: &[Entry]) -> Result<Groups> {
    let mut out = vec![Vec::new(); entries.len()];
    let mut cache = HashMap::new();
    let custom = entries.iter().any(|a| a.builtin().is_none());
    for w in clients()?.into_iter().filter(|w| w.mapped) {
        let mut pid = w.pid;
        let mut seen = HashSet::new();
        while pid > 1 && seen.insert(pid) {
            let Some((p, owner)) = cache.entry(pid).or_insert_with(|| {
                Some((
                    proc_info(pid)?,
                    if custom {
                        crate::custom::owner_key(pid, &get().mount)
                    } else {
                        None
                    },
                ))
            }) else {
                break;
            };
            let index = entries.iter().position(|entry| {
                entry
                    .builtin()
                    .is_none_or(|a| a.matches(&w.class) || a.matches(&w.initial_class))
                    && entry.owns(p, owner.as_deref())
            });
            if let Some(i) = index {
                out[i].push(w);
                break;
            }
            pid = p.parent;
        }
    }
    Ok(out)
}
pub fn roots(entries: &[Entry]) -> Result<Vec<(i32, usize)>> {
    let mut out = Vec::new();
    let custom = entries.iter().any(|a| a.builtin().is_none());
    for entry in fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if let Some(p) = proc_info(pid) {
            let owner = if custom {
                crate::custom::owner_key(pid, &get().mount)
            } else {
                None
            };
            if let Some(i) = entries.iter().position(|a| {
                (a.builtin().is_none() || !p.args.iter().any(|arg| arg.starts_with("--type=")))
                    && a.owns(&p, owner.as_deref())
            }) {
                out.push((pid, i));
            }
        }
    }
    Ok(out)
}
pub fn pin(pid: i32) -> Result<Option<std::os::fd::OwnedFd>> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(e.into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    Ok(Some(fd))
}
pub fn terminate_pinned(fd: &std::os::fd::OwnedFd) -> Result<()> {
    use std::os::fd::AsRawFd;
    let r = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            libc::SIGTERM,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e.into());
        }
    }
    Ok(())
}
pub fn terminate(pid: i32, entry: &Entry) -> Result<()> {
    let Some(fd) = pin(pid)? else { return Ok(()) };
    if let Some(p) = proc_info(pid)
        && entry.owns(&p, crate::custom::owner_key(pid, &get().mount).as_deref())
    {
        terminate_pinned(&fd)?;
    }
    Ok(())
}
pub fn close_window(address: &str) -> Result<()> {
    let selector = serde_json::json!(format!("address:{address}"));
    if let Err(e) = dispatch(&format!("hl.dsp.window.close({{window={selector}}})"))
        && clients()?.iter().any(|w| w.address == address)
    {
        return Err(e);
    }
    Ok(())
}
pub fn dispatch(s: &str) -> Result<()> {
    let reply = volume::output("/usr/bin/hyprctl", &["dispatch", s])?;
    ensure!(reply == "ok", "Hyprland: {reply}");
    Ok(())
}
pub fn launch(entry: &Entry) -> Result<()> {
    if entry.builtin().is_none() {
        crate::custom::validate(&entry.key)?;
    }
    let exe = std::env::current_exe()?;
    let quoted = exe
        .to_str()
        .context("Invalid launcher path")?
        .replace('\'', "'\\''");
    let command = format!("'{quoted}' launch-app {}", entry.token);
    dispatch(&format!(
        "hl.dsp.exec_cmd({}, {{workspace=\"{WORKSPACE} silent\"}})",
        json!(command)
    ))
}
pub fn profile(id: &str, extra: &[String], defaults: bool) -> Result<()> {
    let entry = entries()
        .into_iter()
        .find(|a| a.token == id)
        .context("App is not in this session")?;
    let mount = &get().mount;
    let Some(app) = entry.builtin() else {
        return crate::custom::launch(&entry.key, mount);
    };
    volume::verify()?;
    let root = Path::new(mount);
    let mut cmd = match id {
        "vscode" => {
            let mut c = Command::new("/usr/bin/code");
            for (dir, var) in [
                ("claude", "CLAUDE_CONFIG_DIR"),
                ("codex", "CODEX_HOME"),
                ("codex", "CODEX_SQLITE_HOME"),
            ] {
                let path = root.join(dir);
                fs::create_dir_all(&path)?;
                c.env(var, path);
            }
            c.args([
                format!("--extensions-dir={mount}/vscode-ext"),
                format!("--shared-data-dir={mount}/vscode-shared"),
            ]);
            c
        }
        "chrome" => Command::new("/usr/bin/google-chrome-stable"),
        "obsidian" => {
            let vault = root.join(&get().vault);
            ensure!(
                vault.join(".obsidian").is_dir(),
                "Expected personal vault is missing."
            );
            let dir = root.join("obsidian");
            fs::create_dir_all(&dir)?;
            let target = dir.join("obsidian.json");
            let mut data: Value = if target.exists() {
                serde_json::from_slice(&fs::read(&target)?)?
            } else {
                json!({})
            };
            let vaults = data
                .as_object_mut()
                .context("Invalid Obsidian settings")?
                .entry("vaults")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .context("Invalid vault list")?;
            for v in vaults.values_mut().filter_map(Value::as_object_mut) {
                v.insert("open".into(), false.into());
            }
            vaults.insert(
                "personal-session".into(),
                json!({"path":vault,"ts":0,"open":true}),
            );
            let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
            serde_json::to_writer_pretty(&mut tmp, &data)?;
            writeln!(tmp)?;
            tmp.persist(target)?;
            let mut c = Command::new("/usr/bin/obsidian");
            c.args([
                "--ozone-platform=wayland",
                &format!("obsidian://open?path={}", vault.display()),
            ]);
            c
        }
        "telegram" => {
            let user_telegram = get().home.join(".local/bin/Telegram");
            let exe = [
                "/usr/bin/telegram-desktop",
                user_telegram.to_str().context("Invalid Telegram path")?,
                "/usr/bin/Telegram",
            ]
            .into_iter()
            .find(|x| Path::new(x).is_file())
            .context("Telegram executable missing")?;
            let mut c = Command::new(exe);
            c.args(["-workdir", &format!("{mount}/telegram")]);
            c
        }
        _ => bail!("Unknown profile"),
    };
    if id != "telegram" {
        cmd.arg(format!("--user-data-dir={mount}/{id}"));
    }
    if defaults {
        cmd.args(app.args);
    }
    Err(cmd
        .args(extra)
        .env("ELECTRON_OZONE_PLATFORM_HINT", "wayland")
        .env("XDG_SESSION_TYPE", "wayland")
        .exec()
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_matching_is_exact() {
        let mount = "/fixture/open";
        let mut p = Process {
            exe: "chrome".into(),
            args: vec![format!("--user-data-dir={mount}/chrome-other")],
            parent: 1,
        };
        assert!(!APPS[1].owns_profile(&p, mount));
        p.args = vec!["--user-data-dir".into(), format!("{mount}/chrome")];
        assert!(APPS[1].owns_profile(&p, mount));
        p.exe = "other".into();
        assert!(!APPS[1].owns_profile(&p, mount));
    }
}
