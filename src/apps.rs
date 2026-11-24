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
            "--ozone-platform=x11",
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
    pid: i32,
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
    pub fn enabled(&self) -> bool {
        get().enabled(self.id)
    }
    fn owns(&self, p: &Process) -> bool {
        self.enabled() && self.owns_profile(p, &get().mount)
    }
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
pub type Groups = [Vec<Window>; N];
pub fn windows() -> Result<Groups> {
    let mut out = Groups::default();
    let mut cache = HashMap::new();
    for w in clients()?.into_iter().filter(|w| w.mapped) {
        let Some(i) = APPS
            .iter()
            .position(|a| a.matches(&w.class) || a.matches(&w.initial_class))
        else {
            continue;
        };
        let mut pid = w.pid;
        let mut seen = HashSet::new();
        while pid > 1 && seen.insert(pid) {
            let Some(p) = cache.entry(pid).or_insert_with(|| proc_info(pid)) else {
                break;
            };
            if APPS[i].owns(p) {
                out[i].push(w);
                break;
            }
            pid = p.parent;
        }
    }
    Ok(out)
}
pub fn roots() -> Result<Vec<(i32, usize)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if let Some(p) = proc_info(pid)
            && !p.args.iter().any(|a| a.starts_with("--type="))
            && let Some(i) = APPS.iter().position(|a| a.owns(&p))
        {
            out.push((pid, i));
        }
    }
    Ok(out)
}
pub fn terminate(pid: i32, i: usize) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(e.into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    if let Some(p) = proc_info(pid)
        && APPS[i].owns(&p)
    {
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
    }
    Ok(())
}
pub fn dispatch(s: &str) -> Result<()> {
    let reply = volume::output("/usr/bin/hyprctl", &["dispatch", s])?;
    ensure!(reply == "ok", "Hyprland: {reply}");
    Ok(())
}
pub fn launch(i: usize) -> Result<()> {
    let command = format!("/usr/local/bin/personal-session launch-app {}", APPS[i].id);
    dispatch(&format!(
        "hl.dsp.exec_cmd({}, {{workspace=\"{WORKSPACE} silent\"}})",
        json!(command)
    ))
}
pub fn profile(id: &str, extra: &[String], defaults: bool) -> Result<()> {
    let app = APPS.iter().find(|a| a.id == id).context("Unknown app")?;
    volume::verify()?;
    let mount = &get().mount;
    ensure!(app.enabled(), "Profile disabled");
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
