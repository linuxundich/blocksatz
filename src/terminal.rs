//! The fold-out terminal at the bottom of the editor area (header-bar
//! toggle, F12). A VTE terminal running the user's own shell, started in
//! the open article's folder the first time the panel opens and kept
//! running while it is hidden again - closing the panel is not closing the
//! shell. `exit` in the shell does close the panel; the next opening
//! starts a fresh one.
//!
//! In the Flatpak the shell has to run on the host, not in the sandbox
//! (which has no user tools at all), so it goes through `flatpak-spawn
//! --host` - which is what the `org.freedesktop.Flatpak` talk permission
//! in the manifest is for.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk4::{gdk, gio, glib};
use vte4::prelude::*;

use crate::i18n::tr;

/// An action name and the accelerators it had before the terminal took
/// the focus.
type SavedAccels = (String, Vec<glib::GString>);

/// Stay active inside the terminal.
const KEPT_SHORTCUTS: &[&str] = &["win.toggle-terminal", "win.toggle-preview"];

pub struct TerminalPanel {
    pub widget: gtk4::Box,
    terminal: vte4::Terminal,
    running: Cell<bool>,
    on_exit: RefCell<Option<Box<dyn Fn()>>>,
}

impl TerminalPanel {
    pub fn new() -> Rc<Self> {
        let terminal = vte4::Terminal::builder().vexpand(true).hexpand(true).scrollback_lines(10_000).build();
        terminal.set_mouse_autohide(true);
        terminal.set_bold_is_bright(true);

        // Overlay scrollbars, so the terminal's background reaches the edge.
        let scroller = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).child(&terminal).build();

        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget.add_css_class("blocksatz-terminal");
        widget.append(&scroller);
        widget.set_visible(false);

        let panel = Rc::new(Self { widget, terminal, running: Cell::new(false), on_exit: RefCell::new(None) });
        panel.install_clipboard();
        panel.release_app_shortcuts_while_focused();
        panel.follow_color_scheme();

        let weak = Rc::downgrade(&panel);
        panel.terminal.connect_child_exited(move |_, _| {
            let Some(panel) = weak.upgrade() else { return };
            panel.running.set(false);
            panel.terminal.reset(true, true);
            let on_exit = panel.on_exit.borrow();
            if let Some(on_exit) = on_exit.as_ref() {
                on_exit();
            }
        });
        panel
    }

    /// Called when the shell ends by itself (`exit`, Ctrl+D) - the window
    /// hides the panel and resets its toggle.
    pub fn connect_exit(&self, f: impl Fn() + 'static) {
        *self.on_exit.borrow_mut() = Some(Box::new(f));
    }

    /// Starts the shell in `dir` unless one is already running (that one
    /// keeps its own working directory), and moves the keyboard focus in.
    pub fn open(self: &Rc<Self>, dir: &Path) {
        if !self.running.get() {
            self.spawn(dir);
        }
        // The editor's font when one is set (Einstellungen), else the
        // system's monospace font.
        let font = crate::appearance::load_editor_font_override().map(|desc| gtk4::pango::FontDescription::from_string(&desc));
        self.terminal.set_font_desc(font.as_ref());
        // The panel was only just made visible and isn't mapped yet, which
        // a focus grab right now would silently miss.
        let terminal = self.terminal.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(terminal) = terminal.upgrade() {
                terminal.grab_focus();
            }
        });
    }

    fn spawn(self: &Rc<Self>, dir: &Path) {
        self.running.set(true);
        let dir = dir.to_string_lossy().to_string();
        let marker = format!("blocksatz-terminal-{}-{}", std::process::id(), glib::monotonic_time());
        let argv = shell_argv(&dir, &marker);
        if in_flatpak() {
            self.forward_resizes(marker);
        }
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let terminal = self.terminal.clone();
        self.terminal.spawn_async(
            vte4::PtyFlags::DEFAULT,
            Some(&dir),
            &argv,
            &["COLORTERM=truecolor"],
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            None::<&gio::Cancellable>,
            move |result| {
                if let Err(err) = result {
                    terminal.feed(format!("{}\r\n", tr("Shell konnte nicht gestartet werden: {err}").replace("{err}", err.message())).as_bytes());
                }
            },
        );
    }

    /// `flatpak-spawn` passes no SIGWINCH on to the host, so the host's
    /// `script` (see `shell_argv`) would never learn about a new terminal
    /// size and full-screen programs would draw for the old one. VTE has
    /// no signal for "rows/columns changed" either, so the size is checked
    /// a few times a second while the shell runs, and a change is sent to
    /// `script` - found by the marker in its command line - as SIGWINCH;
    /// it then reads the new size from the terminal and passes it on.
    fn forward_resizes(self: &Rc<Self>, marker: String) {
        let weak = Rc::downgrade(self);
        let last = Cell::new((self.terminal.row_count(), self.terminal.column_count()));
        glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            let Some(panel) = weak.upgrade() else { return glib::ControlFlow::Break };
            if !panel.running.get() {
                return glib::ControlFlow::Break;
            }
            let size = (panel.terminal.row_count(), panel.terminal.column_count());
            if size != last.replace(size) {
                if let Ok(mut child) = std::process::Command::new("flatpak-spawn").args(["--host", "pkill", "-WINCH", "-f", &marker]).spawn() {
                    std::thread::spawn(move || child.wait());
                }
            }
            glib::ControlFlow::Continue
        });
    }

    /// Ctrl+Shift+C/V like every other terminal, plus a context menu. The
    /// controller runs in the capture phase so these keys reach the
    /// terminal before the window's own Ctrl+Shift+C/V shortcuts.
    fn install_clipboard(&self) {
        let actions = gio::SimpleActionGroup::new();
        let copy = gio::SimpleAction::new("copy", None);
        {
            let terminal = self.terminal.clone();
            copy.connect_activate(move |_, _| terminal.copy_clipboard_format(vte4::Format::Text));
        }
        let paste = gio::SimpleAction::new("paste", None);
        {
            let terminal = self.terminal.clone();
            paste.connect_activate(move |_, _| terminal.paste_clipboard());
        }
        let select_all = gio::SimpleAction::new("select-all", None);
        {
            let terminal = self.terminal.clone();
            select_all.connect_activate(move |_, _| terminal.select_all());
        }
        {
            let copy = copy.clone();
            self.terminal.connect_selection_changed(move |terminal| copy.set_enabled(terminal.has_selection()));
        }
        copy.set_enabled(false);
        actions.add_action(&copy);
        actions.add_action(&paste);
        actions.add_action(&select_all);
        self.terminal.insert_action_group("term", Some(&actions));

        let menu = gio::Menu::new();
        let clipboard_section = gio::Menu::new();
        clipboard_section.append(Some(&tr("Kopieren")), Some("term.copy"));
        clipboard_section.append(Some(&tr("Aus Zwischenablage einfügen")), Some("term.paste"));
        menu.append_section(None, &clipboard_section);
        let select_section = gio::Menu::new();
        select_section.append(Some(&tr("Alles auswählen")), Some("term.select-all"));
        menu.append_section(None, &select_section);
        self.terminal.set_context_menu_model(Some(&menu));

        let shortcuts = gtk4::ShortcutController::new();
        shortcuts.set_propagation_phase(gtk4::PropagationPhase::Capture);
        for (trigger, action) in [("<Ctrl><Shift>c", "term.copy"), ("<Ctrl><Shift>v", "term.paste")] {
            shortcuts.add_shortcut(gtk4::Shortcut::new(gtk4::ShortcutTrigger::parse_string(trigger), Some(gtk4::NamedAction::new(action))));
        }
        self.terminal.add_controller(shortcuts);
    }

    /// The application's accelerators are handled before any widget sees
    /// the key, so Ctrl+N would open "Neuer Artikel" instead of reaching
    /// the shell. While the terminal has the focus they are switched off,
    /// except the ones that toggle the panels.
    fn release_app_shortcuts_while_focused(&self) {
        let saved: Rc<RefCell<Vec<SavedAccels>>> = Rc::new(RefCell::new(Vec::new()));
        let focus = gtk4::EventControllerFocus::new();
        {
            let saved = saved.clone();
            focus.connect_enter(move |_| {
                let Some(app) = gio::Application::default().and_downcast::<gtk4::Application>() else { return };
                let mut saved = saved.borrow_mut();
                if !saved.is_empty() {
                    return;
                }
                for action in app.list_action_descriptions() {
                    if KEPT_SHORTCUTS.contains(&action.as_str()) {
                        continue;
                    }
                    let accels = app.accels_for_action(&action);
                    if !accels.is_empty() {
                        app.set_accels_for_action(&action, &[]);
                        saved.push((action.to_string(), accels.to_vec()));
                    }
                }
            });
        }
        focus.connect_leave(move |_| {
            let Some(app) = gio::Application::default().and_downcast::<gtk4::Application>() else { return };
            for (action, accels) in saved.borrow_mut().drain(..) {
                let accels: Vec<&str> = accels.iter().map(|a| a.as_str()).collect();
                app.set_accels_for_action(&action, &accels);
            }
        });
        self.terminal.add_controller(focus);
    }

    fn follow_color_scheme(&self) {
        let terminal = self.terminal.downgrade();
        crate::appearance::connect_scheme_changed(move |scheme| {
            let Some(terminal) = terminal.upgrade() else { return false };
            apply_scheme(&terminal, scheme);
            true
        });
    }
}

/// The 16 ANSI colors from the GNOME palette, one set per background:
/// on light schemes yellow, green, cyan and "white" are darkened so they
/// stay readable, on dark ones blue and black are lightened.
const PALETTE_LIGHT: [&str; 16] = [
    "#241f31", "#c01c28", "#26a269", "#c88800", "#1c71d8", "#9141ac", "#1a8fa6", "#77767b", "#5e5c64", "#e01b24", "#2ec27e", "#e5a50a", "#3584e4", "#c061cb", "#0ab9dc", "#9a9996",
];
const PALETTE_DARK: [&str; 16] = [
    "#5e5c64", "#ed333b", "#57e389", "#f8e45c", "#62a0ea", "#c061cb", "#4fd2fd", "#deddda", "#9a9996", "#f66151", "#8ff0a4", "#f9f06b", "#99c1f1", "#dc8add", "#93ddf3", "#ffffff",
];

/// Background, text, cursor and selection from the editor's color scheme
/// (`appearance::current_scheme`), so the terminal sits in the same
/// colors as the editor above it; the 16 ANSI colors are Console's.
fn apply_scheme(terminal: &vte4::Terminal, scheme: &sourceview5::StyleScheme) {
    let rgba = |color: Option<String>| color.and_then(|c| gdk::RGBA::parse(c.as_str()).ok());
    let (background, foreground) = crate::appearance::scheme_style_colors(scheme, "text");
    let dark = background.as_deref().and_then(|c| gdk::RGBA::parse(c).ok()).is_some_and(|bg| 0.299 * bg.red() + 0.587 * bg.green() + 0.114 * bg.blue() < 0.5);
    let palette: Vec<gdk::RGBA> = (if dark { PALETTE_DARK } else { PALETTE_LIGHT }).iter().filter_map(|c| gdk::RGBA::parse(*c).ok()).collect();
    let palette_refs: Vec<&gdk::RGBA> = palette.iter().collect();
    terminal.set_colors(rgba(foreground).as_ref(), rgba(background).as_ref(), &palette_refs);
    let (cursor_background, cursor_foreground) = crate::appearance::scheme_style_colors(scheme, "cursor");
    terminal.set_color_cursor(rgba(cursor_foreground.or(cursor_background)).as_ref());
    let (selection_background, selection_foreground) = crate::appearance::scheme_style_colors(scheme, "selection");
    terminal.set_color_highlight(rgba(selection_background).as_ref());
    terminal.set_color_highlight_foreground(rgba(selection_foreground).as_ref());
}

fn in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// The user's login shell as an argv - on the host via `flatpak-spawn`
/// when running as a Flatpak. There the shell would get no controlling
/// terminal (no job control, `sudo` and Ctrl+Z misbehave), so util-linux'
/// `script` runs in between and gives it a real pty on the host; `marker`
/// is a shell comment in its command line for `forward_resizes`. Without
/// `script` on the host the shell runs bare.
fn shell_argv(dir: &str, marker: &str) -> Vec<String> {
    if in_flatpak() {
        let shell = host_shell().unwrap_or_else(|| "/bin/bash".to_string());
        let wrapper = format!("if command -v script >/dev/null 2>&1; then exec script -qfe -c 'exec {shell} #{marker}' /dev/null; fi; exec {shell}");
        return vec![
            "flatpak-spawn".into(),
            "--host".into(),
            "--watch-bus".into(),
            format!("--directory={dir}"),
            "--env=TERM=xterm-256color".into(),
            "--env=COLORTERM=truecolor".into(),
            format!("--env=SHELL={shell}"),
            "/bin/sh".into(),
            "-c".into(),
            wrapper,
        ];
    }
    vec![std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/bash".to_string())]
}

/// The sandbox's own passwd has no idea which shell the user picked, so
/// ask the host's.
fn host_shell() -> Option<String> {
    let user = glib::user_name().to_string_lossy().to_string();
    let output = std::process::Command::new("flatpak-spawn").args(["--host", "getent", "passwd", &user]).output().ok()?;
    let line = String::from_utf8(output.stdout).ok()?;
    line.trim().rsplit(':').next().filter(|shell| !shell.is_empty()).map(str::to_string)
}

/// Where a new shell starts: the open article's folder, else the library.
pub fn start_dir(article: Option<&Path>) -> PathBuf {
    article.and_then(Path::parent).filter(|dir| dir.is_dir()).map(Path::to_path_buf).unwrap_or_else(crate::library::root)
}
