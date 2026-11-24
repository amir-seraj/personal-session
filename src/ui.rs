use crate::{
    apps::{self, APPS, N},
    session::{self, Counts},
    volume,
};
use anyhow::{Result, ensure};
use rune::{
    App, AppSpec, Color, Cx, El, Image, ImageCache, Key, TextStyle, Tx, Waker, WindowOptions, col,
    el, row, text,
};
use std::{
    fs,
    process::Command,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

enum Message {
    Progress(String),
    Done(Result<Option<Counts>>),
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Check,
    Start,
    Stop,
}
struct Panel {
    active: Option<bool>,
    counts: Counts,
    busy: bool,
    error: Option<String>,
    detail: String,
    icons: [Option<Image>; N],
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    wake: Waker,
    last_check: f64,
    quit_when_done: bool,
}
impl Default for Panel {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            active: None,
            counts: Default::default(),
            busy: false,
            error: None,
            detail: "Checking personal apps…".into(),
            icons: Default::default(),
            tx,
            rx,
            wake: Arc::new(|| {}),
            last_check: 0.,
            quit_when_done: false,
        }
    }
}
impl Panel {
    fn allowed(&self, a: Action) -> bool {
        !self.busy
            && (a == Action::Check
                || self.error.is_none()
                    && matches!(
                        (a, self.active),
                        (Action::Stop, Some(true)) | (Action::Start, Some(false))
                    ))
    }
    fn run(&mut self, a: Action) {
        if !self.allowed(a) {
            return;
        }
        self.busy = true;
        self.error = None;
        self.detail = match a {
            Action::Check => "Checking personal apps…",
            Action::Start => "Unlock your personal volume in the terminal prompt.",
            Action::Stop => "Closing personal apps…",
        }
        .into();
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let work = || -> Result<Option<Counts>> {
                match a {
                    Action::Check => (),
                    Action::Start => {
                        // The fixed launcher verifies the mount again under the operation lock.
                        if !volume::mounted()? {
                            ensure!(
                                Command::new("/usr/bin/kitty")
                                    .args([
                                        "--title",
                                        "Unlock Personal Session",
                                        "--",
                                        "/usr/local/bin/personal-start",
                                        "--if-sealed"
                                    ])
                                    .status()?
                                    .success(),
                                "Unlock or startup was cancelled."
                            );
                            ensure!(
                                session::status()?,
                                "Personal session did not finish starting."
                            );
                        }
                    }
                    Action::Stop => {
                        ensure!(
                            session::stop(&|msg| {
                                let _ = tx.send(Message::Progress(msg.into()));
                                wake();
                            })? == 0,
                            "Could not stop personal session."
                        );
                    }
                }
                session::check()
            };
            let _ = tx.send(Message::Done(work()));
            wake();
        });
    }
}
fn setup(s: &mut Panel, w: Waker) {
    s.wake = w.clone();
    let cache = ImageCache::new(w);
    s.icons = std::array::from_fn(|i| {
        fs::read(format!(
            "{}/.local/share/icons/WhiteSur/apps/scalable/{}.svg",
            personal_session::config::get().home.display(),
            APPS[i].icon
        ))
        .ok()
        .map(|b| cache.svg(b, 18., 2.))
    });
    s.run(Action::Check)
}
fn poll(s: &mut Panel, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    while let Ok(event) = s.rx.try_recv() {
        changed = true;
        match event {
            Message::Progress(msg) => s.detail = msg,
            Message::Done(result) => {
                s.busy = false;
                if s.quit_when_done {
                    tx.quit();
                }
                match result {
                    Ok(state) => {
                        s.active = Some(state.is_some());
                        s.counts = state.unwrap_or_default();
                        s.detail = if state.is_some() {
                            "Volume unlocked · Super+S opens the scratchpad."
                        } else {
                            "Start your session to unlock the personal volume."
                        }
                        .into();
                    }
                    Err(e) => {
                        s.error = Some(format!("{e:#}"));
                        s.detail = "Review the details, then refresh the session.".into();
                    }
                }
            }
        }
    }
    if clock - s.last_check >= 2.0 {
        s.last_check = clock;
        if !s.busy && s.error.is_none() && volume::mounted().ok() != s.active {
            s.run(Action::Check);
            changed = true;
        }
    }
    tx.wake_after(Duration::from_secs(2));
    changed
}
fn rgb(s: &str) -> Color {
    rune::color::hex(s).expect("valid color")
}
fn label(s: impl Into<String>, size: f32, weight: f32, color: &str) -> El {
    text(s.into())
        .font(TextStyle::new(size, weight))
        .color(rgb(color))
}
fn button(
    name: &str,
    key: &str,
    enabled: bool,
    primary: bool,
    on: impl Fn(&mut Panel, &mut Tx) + 'static,
) -> El {
    let fill = if primary { "#efbb7c" } else { "#342d28" };
    let fg = if !enabled {
        "#9a918b"
    } else if primary {
        "#25160a"
    } else {
        "#f5efea"
    };
    let b = row()
        .key(key)
        .role("button")
        .label(name)
        .focusable(enabled)
        .disabled(!enabled)
        .center()
        .h(29.)
        .px(11.)
        .radius(7.)
        .fill(rgb(fill))
        .border(1., rgb(if primary { "#eabe89" } else { "#50443a" }))
        .child(label(name, 13., if primary { 600. } else { 400. }, fg));
    if enabled { b.on_press_tx(on) } else { b }
}
fn view(s: &Panel, _: &mut Cx<Panel>) -> El {
    let mut content = col()
        .grow(1.)
        .gap(8.)
        .padding(17.)
        .radius(15.)
        .fill(rgb("#1a1717"))
        .radial_gradient(
            [0.33, 1.05],
            170.,
            [(0., rgb("#56331c")), (1., rgb("#1a1717"))],
        )
        .border(1., rgb("#594738"))
        .child(
            label(
                if s.error.is_some() {
                    "Session needs attention"
                } else {
                    "Personal Session"
                },
                23.,
                600.,
                "#f7f3ee",
            )
            .window_drag(),
        )
        .child(label(&s.detail, 13., 400., "#d2c7bd").wrap());
    let mut list = col().grow(1.).min_h(0.).gap(3.);
    if let Some(e) = &s.error {
        list = list
            .scroll(Default::default())
            .child(label(e, 12., 400., "#f4ddc6").wrap())
    } else {
        for (i, app) in APPS.iter().enumerate().filter(|(_, app)| app.enabled()) {
            let (w, p) = s.counts[i];
            let status = if s.active.is_none() {
                "…".into()
            } else {
                match (w, p) {
                    (0, 0) => "Closed".into(),
                    (0, _) => "Background".into(),
                    (1, _) => "1 window".into(),
                    _ => format!("{w} windows"),
                }
            };
            let mut icon = el().size(18., 18.);
            if let Some(img) = &s.icons[i] {
                icon = icon.pixels(img.pixels());
            } else {
                icon = icon.center().radius(4.).fill(rgb("#51463c")).child(label(
                    &app.name[..1],
                    12.,
                    600.,
                    "#f7f3ee",
                ));
            }
            list = list.child(
                row()
                    .w_full()
                    .center_y()
                    .gap(8.)
                    .child(icon)
                    .child(label(app.name, 13., 400., "#f7f3ee"))
                    .child(el().grow(1.))
                    .child(label(status, 12., 400., "#bcb4ae")),
            );
        }
    }
    let mut footer = row().w_full().gap(7.).justify_end();
    if s.error.is_some() {
        footer = footer.child(button("Refresh", "refresh", !s.busy, false, |s, _| {
            s.run(Action::Check)
        }))
    }
    footer = footer
        .child(button("Close", "close", !s.busy, false, |_, tx| tx.quit()))
        .child(button(
            "Stop & seal",
            "stop",
            s.allowed(Action::Stop),
            s.allowed(Action::Stop),
            |s, _| s.run(Action::Stop),
        ))
        .child(button(
            if s.active == Some(true) {
                "Session active"
            } else {
                "Start session"
            },
            "start",
            s.allowed(Action::Start),
            s.allowed(Action::Start),
            |s, _| s.run(Action::Start),
        ));
    content = content
        .child(list)
        .child(el().w_full().h(1.).fill(rgb("#41362d")))
        .child(footer);
    col()
        .w_full()
        .h_full()
        .padding(16.)
        .radial_gradient(
            [0.34, 0.98],
            160.,
            [(0., rgb("#8a451a")), (1., rgb("#0e0d0d"))],
        )
        .child(content)
}
pub fn run(args: Vec<String>) -> Result<()> {
    let tooling = args.iter().any(|s| s == "--snap" || s == "--tree");
    let _guard = if tooling {
        None
    } else {
        match session::lock("panel.lock")? {
            Some(f) => Some(f),
            None => {
                for _ in 0..30 {
                    if let Some(w) = apps::clients()?
                        .into_iter()
                        .find(|w| w.class == "personal-session" && w.mapped)
                    {
                        apps::dispatch(&format!(
                            "hl.dsp.focus({{window={}}})",
                            serde_json::json!(format!("address:{}", w.address))
                        ))?;
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                anyhow::bail!("Personal Session is already opening.")
            }
        }
    };
    rune::set_app_id("personal-session");
    let mut spec = AppSpec::new("Personal Session", (428., 290.), Panel::default, view);
    spec.min_size = Some((428., 290.));
    spec.setup = Some(setup);
    spec.poll = Some(poll);
    spec.on_key = Some(|s, e, tx| {
        if !s.busy && matches!(e.key, Key::Escape | Key::Command('w')) {
            tx.quit();
            true
        } else {
            false
        }
    });
    App::new(spec)
        .args(args)
        .keep_running()
        .on_close(|s, tx| {
            if s.busy {
                s.quit_when_done = true
            } else {
                tx.quit()
            }
        })
        .window_options(WindowOptions {
            borderless: true,
            resizable: false,
            ..Default::default()
        })
        .run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn start_is_only_available_when_sealed() {
        let mut s = Panel::default();
        assert!(!s.allowed(Action::Start));
        s.active = Some(false);
        assert!(s.allowed(Action::Start));
        assert!(!s.allowed(Action::Stop));
        s.busy = true;
        assert!(!s.allowed(Action::Start));
        s.busy = false;
        s.active = Some(true);
        assert!(!s.allowed(Action::Start));
        assert!(s.allowed(Action::Stop));
        s.error = Some("Volume error".into());
        assert!(!s.allowed(Action::Stop));
        assert!(s.allowed(Action::Check));
    }
}
