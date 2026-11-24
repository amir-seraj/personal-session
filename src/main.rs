mod apps;
mod custom;
mod session;
mod ui;
use anyhow::{Result, bail, ensure};
use personal_session::volume;
use std::{
    env,
    io::{self, IsTerminal},
    path::Path,
};

fn run() -> Result<i32> {
    // Shells may be inside the personal mount when invoking the session manager.
    let mut args = env::args().collect::<Vec<_>>();
    let invoked = args.remove(0);
    let name = Path::new(&invoked)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let cmd = match name {
        "personal-start" => "start",
        "personal-stop" => "stop",
        "code-personal" => "vscode",
        "chrome-personal" => "chrome",
        "telegram-personal" => "telegram",
        "obsidian-personal" => "obsidian",
        _ if args
            .first()
            .is_some_and(|s| matches!(s.as_str(), "start" | "stop" | "launch-app")) =>
        {
            let cmd = args.remove(0);
            return command(&cmd, &args);
        }
        _ => {
            personal_session::config::load(false)?;
            ui::run(args)?;
            return Ok(0);
        }
    };
    command(cmd, &args)
}
fn command(cmd: &str, args: &[String]) -> Result<i32> {
    if cmd == "stop" {
        personal_session::config::load_for_stop()?;
    } else {
        personal_session::config::load(false)?;
    }
    match cmd {
        "start" => match args {
            [] => session::start(false),
            [v] if v == "--if-sealed" => session::start(true),
            [v] if v == "--status" => Ok(if session::status()? { 0 } else { 1 }),
            [v] if v == "--gui" => {
                ui::run(vec![])?;
                Ok(0)
            }
            [v] if v == "--help" => {
                println!("personal-start [--status | --if-sealed | --gui]");
                Ok(0)
            }
            _ => bail!("Unknown personal-start option"),
        },
        "stop" => {
            env::set_current_dir("/")?;
            let interactive = args.is_empty()
                && io::stdin().is_terminal()
                && io::stdout().is_terminal()
                && (env::var_os("WAYLAND_DISPLAY").is_some() || env::var_os("DISPLAY").is_some());
            if interactive || args == ["--gui"] {
                ui::run(vec![])?;
                return Ok(0);
            }
            let args = args.strip_prefix(&["--cli".to_string()]).unwrap_or(args);
            match args {
                [] => session::stop(&|m| println!("{m}")),
                [v] if v == "--check" => {
                    if let Some(counts) = session::check()? {
                        for (a, (w, p)) in apps::entries().iter().zip(counts) {
                            println!("{}: {w} window(s), {p} main process(es)", a.name);
                        }
                    } else {
                        println!("Personal filesystem is unmounted.");
                    }
                    Ok(0)
                }
                [v] if v == "--help" => {
                    println!("personal-stop [--gui | --cli | --check]");
                    Ok(0)
                }
                _ => bail!("Unknown personal-stop option"),
            }
        }
        "launch-app" => {
            ensure!(args.len() == 1, "Expected one profile");
            apps::profile(&args[0], &[], true)?;
            Ok(0)
        }
        id => {
            apps::profile(id, args, false)?;
            Ok(0)
        }
    }
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("Personal Session: {e:#}");
            std::process::exit(1)
        }
    }
}
