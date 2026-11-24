use crate::{
    apps::{self, APPS, N},
    session::{self, Counts},
    volume,
};
use anyhow::{Context, Result, ensure};
use personal_session::config::get;
use rune::{
    App, AppSpec, Color, Cx, El, Image, ImageCache, Key, TextStyle, Tx, Waker, WindowOptions, col,
    el, row, text,
};
use std::path::Path;
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
    Picked(Result<Option<String>>),
    Saved(Result<()>),
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Check,
    Start,
    Stop,
}
static CHECK: rune::Icon = rune::Icon {
    name: "selected-app",
    view: 16.,
    body: r#"<path d="m3 8 3 3 7-7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

struct Editor {
    original: Vec<String>,
    selected: Vec<String>,
    custom: Vec<String>,
    notice: Option<String>,
}

impl Editor {
    fn open() -> Self {
        let selected = get().selection();
        let custom = selected
            .iter()
            .filter(|key| Path::new(key).is_absolute())
            .cloned()
            .collect();
        Self {
            original: selected.clone(),
            selected,
            custom,
            notice: None,
        }
    }

    fn toggle(&mut self, key: &str) {
        self.notice = None;
        if self.selected.iter().any(|k| k == key) {
            self.selected.retain(|k| k != key);
        } else {
            self.selected.push(key.into());
        }
    }

    fn changed(&self) -> bool {
        self.original.len() != self.selected.len()
            || self.selected.iter().any(|k| !self.original.contains(k))
    }
}

fn pick_executable() -> Result<Option<String>> {
    let (program, args): (&str, &[&str]) = if Path::new("/usr/bin/zenity").is_file() {
        (
            "/usr/bin/zenity",
            &[
                "--file-selection",
                "--title=Choose an app executable",
                "--filename=/usr/bin/",
            ],
        )
    } else {
        (
            "/usr/bin/kdialog",
            &[
                "--title",
                "Choose an app executable",
                "--getopenfilename",
                "/usr/bin/",
            ],
        )
    };
    let output = Command::new(program)
        .args(args)
        .output()
        .context("Could not open the file picker. Install Zenity or KDialog and try again.")?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(None);
    }
    ensure!(
        output.status.success(),
        "File picker failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let output =
        String::from_utf8(output.stdout).context("Choose an executable with a UTF-8 path.")?;
    let path = output.strip_suffix('\n').unwrap_or(&output);
    crate::custom::validate(path)?;
    Ok(Some(path.to_owned()))
}
struct Panel {
    active: Option<bool>,
    editor: Option<Editor>,
    open_editor_on_check: bool,
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
            editor: None,
            open_editor_on_check: false,
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
    fn edit(&mut self, tx: &mut Tx) {
        if !self.busy && self.active == Some(false) && self.error.is_none() {
            self.editor = Some(Editor::open());
            tx.animated();
            tx.focus_when_shown("app-vscode", true);
        }
    }

    fn leave_editor(&mut self, tx: &mut Tx) {
        self.editor = None;
        tx.animated();
        tx.focus_when_shown("manage", true);
    }

    fn editor_job(&mut self, picking: bool) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if self.busy || self.active != Some(false) {
            return;
        }
        self.busy = true;
        editor.notice = Some(
            if picking {
                "Choose an executable in the file picker…"
            } else {
                "Saving app selection…"
            }
            .into(),
        );
        let (original, selected) = (editor.original.clone(), editor.selected.clone());
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let event = if picking {
                Message::Picked(pick_executable())
            } else {
                Message::Saved(session::save_selection(&original, &selected))
            };
            let _ = tx.send(event);
            wake();
        });
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
                        // The CLI verifies the mount again under the operation lock.
                        if !volume::mounted()? {
                            ensure!(
                                Command::new("/usr/bin/kitty")
                                    .args(["--title", "Unlock Personal Session", "--"])
                                    .arg(std::env::current_exe()?)
                                    .args(["start", "--if-sealed"])
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
            Message::Picked(result) => {
                s.busy = false;
                if s.quit_when_done {
                    tx.quit();
                }
                if let Some(editor) = &mut s.editor {
                    editor.notice = None;
                    match result {
                        Ok(Some(path)) => {
                            if !editor.custom.contains(&path) {
                                editor.custom.push(path.clone());
                            }
                            if !editor.selected.contains(&path) {
                                editor.selected.push(path.clone());
                            } else {
                                editor.notice = Some("This app is already selected.".into());
                            }
                            tx.focus_when_shown(format!("app-{path}"), true);
                        }
                        Ok(None) => tx.focus_when_shown("add", true),
                        Err(e) => editor.notice = Some(format!("{e:#}")),
                    }
                }
            }
            Message::Saved(result) => {
                s.busy = false;
                if s.quit_when_done {
                    tx.quit();
                }
                match result {
                    Ok(()) => {
                        s.leave_editor(tx);
                        s.detail = "App selection saved for the next session.".into();
                    }
                    Err(e) => {
                        if let Some(editor) = &mut s.editor {
                            editor.notice = Some(format!("{e:#}"));
                        }
                    }
                }
            }
            Message::Done(result) => {
                s.busy = false;
                if s.quit_when_done {
                    tx.quit();
                }
                match result {
                    Ok(state) => {
                        s.active = Some(state.is_some());

                        s.detail = if state.is_some() {
                            "Super+S opens the scratchpad. Stop and seal to change apps."
                        } else {
                            "Start your session to unlock the personal volume."
                        }
                        .into();
                        s.counts = state.unwrap_or_default();
                        if s.open_editor_on_check {
                            s.open_editor_on_check = false;
                            s.edit(tx);
                        }
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
        .focus_ring(2., 1., rgb("#efbb7c"))
        .focusable(enabled)
        .disabled(!enabled)
        .center()
        .h(29.)
        .px(11.)
        .radius(7.)
        .fill(rgb(fill))
        .border(1., rgb(if primary { "#eabe89" } else { "#50443a" }))
        .child(label(name, 13., if primary { 600. } else { 400. }, fg));
    if enabled {
        b.hover(rgb(if primary { "#f6c890" } else { "#453b33" }))
            .on_press_tx(on)
    } else {
        b
    }
}
fn view(s: &Panel, cx: &mut Cx<Panel>) -> El {
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
        .border(1., rgb("#594738"));
    if let Some(editor) = &s.editor {
        content = edit_view(content, s, editor);
    } else {
        content = session_view(content, s);
    }
    content = content.key(if s.editor.is_some() {
        "editor-screen"
    } else {
        "session-screen"
    });
    if !cx.system_reduced_motion() {
        content = content.transition(rune::Transition::opacity());
    }
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

fn edit_view(mut content: El, s: &Panel, editor: &Editor) -> El {
    let editable = !s.busy && s.active == Some(false) && s.error.is_none();
    content = content
        .child(label("Manage apps", 23., 600., "#f7f3ee").window_drag())
        .child(
            label(
                "Select startup apps. Removing one keeps its files.",
                13.,
                400.,
                "#d2c7bd",
            )
            .wrap(),
        );
    let mut list = col()
        .key("choices")
        .grow(1.)
        .min_h(0.)
        .gap(2.)
        .padding(2.)
        .scroll(Default::default());
    for (key, name) in APPS.iter().map(|a| (a.id, a.name)).chain(
        editor
            .custom
            .iter()
            .map(|path| (path.as_str(), path.as_str())),
    ) {
        let selected = editor.selected.iter().any(|k| k == key);
        let mut choice = row()
            .key(format!("app-{key}"))
            .role("checkbox")
            .label(name)
            .checked(selected)
            .focusable(editable)
            .disabled(!editable)
            .focus_ring(1., -2., rgb("#efbb7c"))
            .w_full()
            .min_h(22.)
            .center_y()
            .gap(8.)
            .px(4.)
            .radius(5.);
        let mut check = el()
            .size(15., 15.)
            .radius(4.)
            .border(1., rgb(if selected { "#eabe89" } else { "#897a6e" }))
            .center();
        if selected {
            check = check
                .fill(rgb("#efbb7c"))
                .child(rune::icon(&CHECK, 13.).color(rgb("#442e1d")));
        }
        let description = if let Some(parent) = Path::new(key).parent().filter(|p| p.is_absolute())
        {
            let basename = Path::new(key)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(name);
            col()
                .grow(1.)
                .min_w(0.)
                .child(label(basename, 13., 400., "#f7f3ee").truncate())
                .child(label(parent.display().to_string(), 11., 400., "#bcb4ae").truncate_middle())
        } else {
            label(name, 13., 400., "#f7f3ee")
                .grow(1.)
                .min_w(0.)
                .truncate()
        };
        choice = choice.child(check).child(description);
        if editable {
            let key = key.to_owned();
            choice = choice
                .hover(rgb("#39302a"))
                .on_press_tx(move |s: &mut Panel, _| {
                    if let Some(e) = &mut s.editor {
                        e.toggle(&key);
                    }
                });
        }
        list = list.child(choice);
    }
    list = list.child(
        label(
            "Custom apps must honor HOME and XDG paths.",
            12.,
            400.,
            "#bcb4ae",
        )
        .wrap(),
    );
    let mut footer = row()
        .w_full()
        .gap(7.)
        .center_y()
        .child(button(
            "Add executable…",
            "add",
            editable,
            false,
            |s, _| s.editor_job(true),
        ))
        .child(el().grow(1.))
        .child(button("Cancel", "cancel", !s.busy, false, |s, tx| {
            s.leave_editor(tx)
        }));
    let save = editable && editor.changed();
    footer = footer.child(button("Save", "save", save, save, |s, _| {
        s.editor_job(false)
    }));
    if let Some(note) =
        s.error
            .as_deref()
            .or(editor.notice.as_deref())
            .or(if s.active == Some(true) {
                Some("Stop and seal the session before saving changes.")
            } else {
                None
            })
    {
        list = col()
            .grow(1.)
            .min_h(0.)
            .gap(4.)
            .child(
                label(note, 12., 400., "#f4ddc6")
                    .wrap()
                    .max_h(45.)
                    .scroll(Default::default()),
            )
            .child(list);
    }
    content
        .child(list)
        .child(el().w_full().h(1.).fill(rgb("#41362d")))
        .child(footer)
}

fn session_view(mut content: El, s: &Panel) -> El {
    let editable = !s.busy && s.error.is_none() && s.active == Some(false);
    content = content
        .child(
            row()
                .w_full()
                .center_y()
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
                .child(el().grow(1.).window_drag())
                .child(button("Manage apps", "manage", editable, false, |s, tx| {
                    s.edit(tx)
                })),
        )
        .child(label(&s.detail, 13., 400., "#d2c7bd").wrap());
    let entries = apps::entries();
    let mut list = col().grow(1.).min_h(0.).gap(3.).scroll(Default::default());
    if let Some(e) = &s.error {
        list = list.child(label(e, 12., 400., "#f4ddc6").wrap());
    } else if entries.is_empty() {
        list = list
            .child(label("No session apps", 15., 600., "#f7f3ee"))
            .child(
                label(
                    "Choose Manage apps to add your first app.",
                    13.,
                    400.,
                    "#d2c7bd",
                )
                .wrap(),
            );
    } else {
        for (i, entry) in entries.iter().enumerate() {
            let status = match (s.active, s.counts.get(i).copied()) {
                (None, _) | (Some(true), None) => "…".into(),
                (Some(false), _) | (_, Some((0, 0))) => "Closed".into(),
                (_, Some((0, _))) => "Background".into(),
                (_, Some((1, _))) => "1 window".into(),
                (_, Some((w, _))) => format!("{w} windows"),
            };
            let mut icon = el().size(18., 18.);
            if let Some(img) = APPS
                .iter()
                .position(|a| a.id == entry.key)
                .and_then(|i| s.icons[i].as_ref())
            {
                icon = icon.pixels(img.pixels());
            } else {
                icon = icon.center().radius(4.).fill(rgb("#51463c")).child(label(
                    entry.name.chars().next().unwrap_or('?').to_string(),
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
                    .child(
                        label(&entry.name, 13., 400., "#f7f3ee")
                            .grow(1.)
                            .min_w(0.)
                            .truncate(),
                    )
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
            s.allowed(Action::Start) && !entries.is_empty(),
            s.allowed(Action::Start) && !entries.is_empty(),
            |s, _| s.run(Action::Start),
        ));
    content
        .child(list)
        .child(el().w_full().h(1.).fill(rgb("#41362d")))
        .child(footer)
}
pub fn run(mut args: Vec<String>) -> Result<()> {
    let manage = args.iter().any(|a| a == "--manage-apps");
    args.retain(|a| a != "--manage-apps");
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
    if manage {
        spec.init = || Panel {
            open_editor_on_check: true,
            ..Panel::default()
        };
    }
    spec.min_size = Some((428., 290.));
    spec.setup = Some(setup);
    spec.poll = Some(poll);
    spec.on_key = Some(|s, e, tx| {
        if !s.busy && matches!(e.key, Key::Escape | Key::Command('w')) {
            if s.editor.is_some() && matches!(e.key, Key::Escape) {
                s.leave_editor(tx);
            } else {
                tx.quit();
            }
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
