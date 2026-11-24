use crate::apps::{self, APPS, Groups, N, WORKSPACE};
use crate::volume;
use anyhow::{Result, ensure};
use serde_json::json;
use std::{
    collections::HashMap,
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::Command,
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
pub type Counts = [(usize, usize); N];
pub fn check() -> Result<Option<Counts>> {
    if !volume::mounted()? {
        return Ok(None);
    }
    volume::verify()?;
    let groups = apps::windows()?;
    let mut counts = std::array::from_fn(|i| (groups[i].len(), 0));
    for (_, i) in apps::roots()? {
        counts[i].1 += 1
    }
    Ok(Some(counts))
}
fn ready(wins: &[apps::Window]) -> bool {
    !wins.is_empty() && wins.iter().all(|w| w.workspace.name == WORKSPACE)
}
fn report(g: &Groups) -> Result<bool> {
    let roots = apps::roots()?;
    let mut count = 0;
    for (i, wins) in g.iter().enumerate().filter(|(i, _)| APPS[*i].enabled()) {
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
        println!("{}: {state} {detail}", APPS[i].name);
    }
    let total = APPS.iter().filter(|a| a.enabled()).count();
    println!("{count}/{total} ready · Super+S toggles the scratchpad");
    Ok(count == total)
}
pub fn status() -> Result<bool> {
    volume::verify()?;
    println!("Verified personal volume.");
    report(&apps::windows()?)
}
pub fn start(only_sealed: bool) -> Result<i32> {
    let Some(_guard) = lock("start.lock")? else {
        eprintln!("Another personal session command is in progress.");
        return Ok(2);
    };
    if only_sealed && volume::mounted()? {
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
    let result = start_locked(&mut log);
    if let Err(e) = &result {
        writeln!(log, "{e:#}")?;
    }
    result
}
fn start_locked(log: &mut File) -> Result<i32> {
    apps::clients()?;
    if !volume::mounted()? {
        ensure!(
            Command::new("/usr/bin/sudo")
                .arg("/usr/local/bin/personal-mount")
                .status()?
                .success(),
            "Unlock failed."
        )
    }
    volume::verify()?;
    let g = apps::windows()?;
    let bg = apps::roots()?;
    let mut failed = [false; N];
    thread::scope(|scope| -> Result<()> {
        let mut jobs = Vec::new();
        for (i, _) in g
            .iter()
            .enumerate()
            .filter(|(i, w)| APPS[*i].enabled() && w.is_empty())
        {
            let verb = if bg.iter().any(|p| p.1 == i) {
                "Reopening"
            } else {
                "Starting"
            };
            println!("{verb} {}", APPS[i].name);
            writeln!(log, "{verb} {}", APPS[i].name)?;
            jobs.push((i, scope.spawn(move || apps::launch(i))));
        }
        for (i, job) in jobs {
            if let Err(e) = job.join().expect("Window launcher thread failed") {
                failed[i] = true;
                println!("{}: {e}", APPS[i].name);
                writeln!(log, "{}: {e}", APPS[i].name)?;
            }
        }
        Ok(())
    })?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut quiet = None;
    let mut moved = HashMap::new();
    loop {
        let groups = apps::windows()?;
        let now = Instant::now();
        let mut settled = true;
        for (i, wins) in groups.iter().enumerate() {
            if APPS[i].enabled() && wins.is_empty() && !failed[i] {
                settled = false
            }
            for w in wins.iter().filter(|w| w.workspace.name != WORKSPACE) {
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
    let g = apps::windows()?;
    for (i, w) in g.iter().enumerate() {
        writeln!(
            log,
            "{}: {} windows, ready={}",
            APPS[i].name,
            w.len(),
            ready(w)
        )?
    }
    Ok(if report(&g)? { 0 } else { 1 })
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
    let groups = apps::windows()?;
    for (i, wins) in groups.iter().enumerate() {
        if !wins.is_empty() {
            progress(&format!("Closing {} ({})", APPS[i].name, wins.len()));
        }
        for w in wins {
            let selector = json!(format!("address:{}", w.address));
            if let Err(e) = apps::dispatch(&format!("hl.dsp.window.close({{window={selector}}})"))
                && apps::clients()?.iter().any(|c| c.address == w.address)
            {
                return Err(e);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let groups = apps::windows()?;
        let open: Vec<_> = groups
            .iter()
            .enumerate()
            .filter(|(_, w)| !w.is_empty())
            .map(|(i, _)| APPS[i].name)
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
    let roots = apps::roots()?;
    if !roots.is_empty() {
        progress("Quitting personal background apps…");
    }
    for (pid, i) in roots {
        apps::terminate(pid, i)?
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !apps::roots()?.is_empty() {
        ensure!(
            Instant::now() < deadline,
            "Personal apps are still exiting. Retry after they finish."
        );
        thread::sleep(Duration::from_millis(250));
    }
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
        let make = |name: &str| {
            serde_json::from_value(json!({"address":"0x1","mapped":true,"pid":1,"workspace":{"name":name},"class":"chrome"})).unwrap()
        };
        assert!(!ready(&[]));
        assert!(ready(&[make(WORKSPACE)]));
        assert!(!ready(&[make(WORKSPACE), make("9")]));
    }
}
