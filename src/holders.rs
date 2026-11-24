use crate::{apps, volume};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    thread,
    time::{Duration, Instant},
};

fn pids(mount: &str) -> Result<BTreeSet<i32>> {
    let out = volume::bounded("/usr/bin/fuser", &["-mM", mount], 4)?;
    if out.status.code() == Some(1) && out.stderr.is_empty() {
        return Ok(BTreeSet::new());
    }
    ensure!(
        out.status.success(),
        "Cannot inspect volume holders: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    std::str::from_utf8(&out.stdout)?
        .split_whitespace()
        .map(|s| {
            let pid: i32 = s.parse()?;
            ensure!(pid > 1, "Unexpected volume holder {pid}");
            Ok(pid)
        })
        .collect()
}

// Only the first unit below the current user's manager (and its slices)
// is managed by systemctl --user. Nested container units belong elsewhere.
fn service(cgroup: &str, uid: u32) -> Option<&str> {
    let prefix = format!("0::/user.slice/user-{uid}.slice/user@{uid}.service/");
    let path = cgroup.lines().find_map(|line| line.strip_prefix(&prefix))?;
    let unit = path.split('/').find(|s| !s.ends_with(".slice"))?;
    (unit.ends_with(".service") && !unit.contains(['*', '?', '[', ']'])).then_some(unit)
}

pub fn release(mount: &str, progress: &dyn Fn(&str)) -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let own_group = fs::read_to_string("/proc/self/cgroup")?;
    let mut units = BTreeSet::new();
    let mut processes = Vec::new();
    let mut windows = Vec::new();
    let holders = pids(mount)?;
    if holders.is_empty() {
        return Ok(());
    }
    let clients = apps::clients()?;
    for pid in holders {
        if pid == std::process::id() as i32 {
            continue;
        }
        let Some(fd) = apps::pin(pid)? else { continue };
        let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
        if !fs::metadata(&proc).is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        let Ok(group) = fs::read_to_string(proc.join("cgroup")) else {
            continue;
        };
        if group.lines().any(|line| line.ends_with("/init.scope")) {
            continue;
        }
        for w in clients.iter().filter(|w| w.mapped && w.pid == pid) {
            windows.push(w.address.clone());
        }
        if let Some(unit) = service(&group, uid) {
            if Some(unit) != service(&own_group, uid) {
                units.insert((pid, unit.to_owned()));
            }
        } else {
            processes.push((pid, fd));
        }
    }

    if !windows.is_empty() {
        progress("Closing windows using the personal volume…");
        for address in &windows {
            apps::close_window(address)?;
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while apps::clients()?
            .iter()
            .any(|w| w.mapped && windows.contains(&w.address))
        {
            ensure!(
                Instant::now() < deadline,
                "A window using the volume is waiting to close. Check for a save prompt, then retry."
            );
            thread::sleep(Duration::from_millis(250));
        }
    }
    let remaining = pids(mount)?;
    let units: BTreeSet<_> = units
        .into_iter()
        .filter(|(pid, unit)| {
            remaining.contains(pid)
                && fs::read_to_string(format!("/proc/{pid}/cgroup"))
                    .is_ok_and(|cg| service(&cg, uid) == Some(unit.as_str()))
        })
        .map(|(_, unit)| unit)
        .collect();
    if !units.is_empty() {
        let mut args = vec!["--user", "stop", "--"];
        args.extend(units.iter().map(String::as_str));
        progress(&format!("Stopping {}…", args[3..].join(", ")));
        let out = volume::bounded("/usr/bin/systemctl", &args, 100)?;
        ensure!(
            out.status.success(),
            "Could not stop {}: {} ({})",
            args[3..].join(", "),
            String::from_utf8_lossy(&out.stderr).trim(),
            out.status
        );
    }
    if !processes.is_empty() {
        let remaining = pids(mount)?;
        for (pid, fd) in processes.iter().filter(|(pid, _)| remaining.contains(pid)) {
            progress(&format!("Closing volume holder {pid}…"));
            apps::terminate_pinned(fd).with_context(|| format!("Closing volume holder {pid}"))?;
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline
            && pids(mount)?
                .iter()
                .any(|p| processes.iter().any(|(pid, _)| pid == p))
        {
            thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_only_services_owned_by_the_user_manager() {
        let base = "0::/user.slice/user-1001.slice/user@1001.service";
        for unit in ["sync-agent.service", "indexer@profile.service"] {
            assert_eq!(
                service(&format!("{base}/app.slice/{unit}"), 1001),
                Some(unit)
            );
        }
        assert_eq!(
            service(
                &format!("{base}/app.slice/gui.scope/container.service"),
                1001
            ),
            None
        );
        assert_eq!(
            service(&format!("{base}/app.slice/sync-agent.service"), 1002),
            None
        );
        assert_eq!(service("0::/system.slice/host.service", 1001), None);
    }
}
