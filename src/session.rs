use crate::apps::{self, Entry, Groups, WORKSPACE};
use crate::volume;
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    collections::HashMap,
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

fn state_dir() -> Result<PathBuf> {
    let p = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            personal_session::config::get()
                .home
                .clone()
                .join(".local/state")
        })
        .join("personal-start");
    fs::create_dir_all(&p)?;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700))?;
    Ok(p)
}
pub fn lock(name: &str) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(state_dir()?.join(name))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn save_selection(expected: &[String], next: &[String]) -> Result<()> {
    let _lock = lock("start.lock")?.ok_or_else(|| {
        anyhow::anyhow!("A session operation is in progress. Try again when it finishes.")
    })?;
    ensure!(
        !volume::mounted()?,
        "Stop and seal the session before changing its apps."
    );
    personal_session::config::get().save_selection(expected, next)
}
#[derive(Clone, Copy)]
pub struct AppState {
    pub windows: usize,
    pub processes: usize,
    pub ready: bool,
}
pub type Counts = Vec<AppState>;
pub fn check() -> Result<Option<Counts>> {
    personal_session::config::get().refresh_selection()?;
    if !volume::mounted()? {
        return Ok(None);
    }
    volume::verify()?;
    let entries = apps::entries();
    let groups = apps::windows(&entries)?;
    let mut counts: Counts = groups
        .iter()
        .map(|w| AppState {
            windows: w.len(),
            processes: 0,
            ready: ready(w),
        })
        .collect();
    for (_, i) in apps::roots(&entries)? {
        counts[i].processes += 1
    }
    Ok(Some(counts))
}
fn ready(wins: &[apps::Window]) -> bool {
    !wins.is_empty() && wins.iter().all(|w| w.workspace.is_scratchpad())
}
fn report(entries: &[Entry], g: &Groups) -> Result<bool> {
    let roots = apps::roots(entries)?;
    let mut count = 0;
    for (i, wins) in g.iter().enumerate() {
        let state = if ready(wins) {
            count += 1;
            "ready"
        } else if !wins.is_empty() {
            "misplaced"
        } else if roots.iter().any(|p| p.1 == i) {
            "background only"
        } else {
            "missing"
        };
        let detail = wins
            .iter()
            .map(|w| format!("{} on {}", w.address, w.workspace.name))
            .collect::<Vec<_>>()
            .join(", ");
        println!("{}: {state} {detail}", entries[i].name);
    }
    let total = entries.len();
    println!("{count}/{total} ready · Super+S toggles the scratchpad");
    Ok(count == total)
}
pub fn status() -> Result<bool> {
    personal_session::config::get().refresh_selection()?;
    volume::verify()?;
    println!("Verified personal volume.");
    {
        let entries = apps::entries();
        report(&entries, &apps::windows(&entries)?)
    }
}
#[derive(Clone, Copy, PartialEq)]
pub enum StartMode {
    Terminal,
    IfSealed,
    Panel,
}

fn unlock_with_dialog() -> Result<bool> {
    let (program, args): (&str, &[&str]) = if std::path::Path::new("/usr/bin/zenity").is_file() {
        (
            "/usr/bin/zenity",
            &["--password", "--title=Unlock personal volume"],
        )
    } else {
        (
            "/usr/bin/kdialog",
            &[
                "--title",
                "Personal Session",
                "--password",
                "VeraCrypt volume password",
            ],
        )
    };
    let dialog = Command::new(program)
        .args(args)
        .output()
        .context("Could not open the volume password dialog. Install Zenity or KDialog.")?;
    let password = zeroize::Zeroizing::new(dialog.stdout);
    if dialog.status.code() == Some(1) && password.is_empty() && dialog.stderr.is_empty() {
        return Ok(false);
    }
    ensure!(
        dialog.status.success(),
        "Could not open the volume password dialog: {}",
        String::from_utf8_lossy(&dialog.stderr).trim()
    );
    ensure!(
        password
            .strip_suffix(b"\n")
            .is_some_and(|line| !line.is_empty() && !line.contains(&b'\n')),
        "Enter the volume password."
    );
    let mut helper = Command::new("/usr/bin/sudo")
        .args(["-n", "/usr/local/bin/personal-mount"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("Starting the volume helper")?;
    let written = helper
        .stdin
        .take()
        .context("Missing password pipe")?
        .write_all(&password);
    drop(password);
    let result = helper.wait_with_output()?;
    ensure!(
        result.status.success(),
        "Could not unlock the volume. {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    written.context("Sending the volume password")?;
    Ok(true)
}

pub fn start(mode: StartMode) -> Result<i32> {
    let Some(_guard) = lock("start.lock")? else {
        eprintln!("Another personal session command is in progress.");
        return Ok(2);
    };
    personal_session::config::get().refresh_selection()?;
    if mode == StartMode::IfSealed && volume::mounted()? {
        volume::verify()?;
        println!("Personal session is already active.");
        return Ok(0);
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(state_dir()?.join("start.log"))?;
    writeln!(log, "{:?} start", SystemTime::now())?;
    let result = start_locked(&mut log, mode);
    if let Err(e) = &result {
        writeln!(log, "{e:#}")?;
    }
    result
}
fn start_locked(log: &mut File, mode: StartMode) -> Result<i32> {
    let entries = apps::entries();
    ensure!(
        !entries.is_empty(),
        "Choose an app in Manage apps before starting."
    );
    apps::clients()?;
    if !volume::mounted()? {
        if mode == StartMode::Panel {
            if !unlock_with_dialog()? {
                return Ok(0);
            }
        } else {
            ensure!(
                Command::new("/usr/bin/sudo")
                    .arg("/usr/local/bin/personal-mount")
                    .status()?
                    .success(),
                "Unlock failed."
            );
        }
    }
    volume::verify()?;
    let g = apps::windows(&entries)?;
    let bg = apps::roots(&entries)?;
    let mut failed = vec![false; entries.len()];
    thread::scope(|scope| -> Result<()> {
        let mut jobs = Vec::new();
        for (i, _) in g.iter().enumerate().filter(|(_, w)| w.is_empty()) {
            let verb = if bg.iter().any(|p| p.1 == i) {
                "Reopening"
            } else {
                "Starting"
            };
            println!("{verb} {}", entries[i].name);
            writeln!(log, "{verb} {}", entries[i].name)?;
            let entry = &entries[i];
            jobs.push((i, scope.spawn(move || apps::launch(entry))));
        }
        for (i, job) in jobs {
            if let Err(e) = job.join().expect("Window launcher thread failed") {
                failed[i] = true;
                println!("{}: {e}", entries[i].name);
                writeln!(log, "{}: {e}", entries[i].name)?;
            }
        }
        Ok(())
    })?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut quiet = None;
    let mut moved = HashMap::new();
    loop {
        let groups = apps::windows(&entries)?;
        let now = Instant::now();
        let mut settled = true;
        for (i, wins) in groups.iter().enumerate() {
            if wins.is_empty() && !failed[i] {
                settled = false
            }
            for w in wins.iter().filter(|w| !w.workspace.is_scratchpad()) {
                settled = false;
                if moved
                    .get(&w.address)
                    .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(1))
                {
                    continue;
                }
                moved.insert(w.address.clone(), now);
                let selector = json!(format!("address:{}", w.address));
                if let Err(e) = apps::dispatch(&format!(
                    "hl.dsp.window.move({{workspace=\"{WORKSPACE}\",window={selector},follow=false}})"
                )) {
                    writeln!(log, "Move failed: {e}")?
                }
            }
        }
        if settled {
            if quiet.get_or_insert(now).elapsed() >= Duration::from_secs(1) {
                break;
            }
        } else {
            quiet = None
        }
        if now >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    volume::verify()?;
    let g = apps::windows(&entries)?;
    for (i, w) in g.iter().enumerate() {
        writeln!(
            log,
            "{}: {} windows, ready={}",
            entries[i].name,
            w.len(),
            ready(w)
        )?
    }
    if !report(&entries, &g)? {
        let pending = entries
            .iter()
            .zip(&g)
            .filter(|(_, wins)| !ready(wins))
            .map(|(entry, wins)| {
                if wins.is_empty() {
                    format!("{} did not open a window", entry.name)
                } else {
                    format!("{} is outside the session workspace", entry.name)
                }
            })
            .collect::<Vec<_>>()
            .join(". ");
        anyhow::bail!("Volume unlocked. {pending}.");
    }
    Ok(0)
}
pub fn stop(progress: &dyn Fn(&str)) -> Result<i32> {
    let Some(_guard) = lock("start.lock")? else {
        eprintln!("Another personal session command is in progress.");
        return Ok(2);
    };
    if !volume::mounted()? {
        progress("Personal filesystem is already unmounted.");
        return Ok(0);
    }
    volume::verify()?;
    let entries = apps::cleanup_entries()?;
    let groups = apps::windows(&entries)?;
    for (i, wins) in groups.iter().enumerate() {
        if !wins.is_empty() {
            progress(&format!("Closing {} ({})", entries[i].name, wins.len()));
        }
        for w in wins {
            apps::close_window(&w.address)?;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let groups = apps::windows(&entries)?;
        let open: Vec<_> = groups
            .iter()
            .enumerate()
            .filter(|(_, w)| !w.is_empty())
            .map(|(i, _)| entries[i].name.as_str())
            .collect();
        if open.is_empty() {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "Still open: {}. Check the scratchpad for a save or quit prompt.",
            open.join(", ")
        );
        thread::sleep(Duration::from_millis(250));
    }
    let roots = apps::roots(&entries)?;
    if !roots.is_empty() {
        progress("Quitting personal background apps…");
    }
    for (pid, i) in roots {
        apps::terminate(pid, &entries[i])?
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !apps::roots(&entries)?.is_empty() {
        ensure!(
            Instant::now() < deadline,
            "Personal apps are still exiting. Retry after they finish."
        );
        thread::sleep(Duration::from_millis(250));
    }
    crate::holders::release(&personal_session::config::get().mount, progress)?;
    progress("Checking holders and sealing the volume…");
    let out = Command::new("/usr/bin/sudo")
        .args(["-n", "/usr/local/bin/personal-umount"])
        .output()?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    progress(String::from_utf8_lossy(&out.stdout).trim());
    Ok(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_windows_must_be_on_target() {
        let make = |id, name: &str| {
            serde_json::from_value(json!({"address":"0x1","mapped":true,"pid":1,"workspace":{"id":id,"name":name},"class":"chrome"})).unwrap()
        };
        assert!(!ready(&[]));
        assert!(ready(&[make(-99, "special:special")]));
        assert!(!ready(&[
            make(-99, "special:special"),
            make(-98, "special:special")
        ]));
        assert!(!ready(&[make(9, "9")]));
    }
}
