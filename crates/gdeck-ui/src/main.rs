//! G Deck interface.
//!
//! This crate draws. It owns no security decisions: reading the keyboard,
//! applying descriptors, parsing the config, sealing it and launching anything
//! all live in `gdeck`, which has no dependencies at all.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod autostart;
mod filedialog;
mod iconart;
mod icons;
mod single_instance;
mod theme;
mod tray;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke, Vec2};

use gdeck::actions::{self, Performed};
use gdeck::audit::Outcome;
use gdeck::config::{classify_target, Action};
use gdeck::keys::{GKey, G_KEY_COUNT};
use gdeck::{Config, Event, Policy, Reader, Sid, Slot};

/// How long a key stays lit after it was physically pressed.
const FLASH: Duration = Duration::from_millis(450);

// ---------------------------------------------------------------------------
// Live state, written by the reader thread
// ---------------------------------------------------------------------------

struct Live {
    connected: bool,
    held: u32,
    last_press: [Option<Instant>; G_KEY_COUNT],
    /// What the most recent press did, and whether it went well.
    last_result: Option<(String, bool)>,
    /// Exit code and full output of the most recent shell run.
    last_shell: Option<(Option<u32>, String)>,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            connected: false,
            held: 0,
            last_press: [None; G_KEY_COUNT],
            last_result: None,
            last_shell: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Editor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    None,
    Launch,
    Open,
    Note,
    Shell,
}

impl Kind {
    const ALL: [Kind; 5] = [Kind::None, Kind::Launch, Kind::Open, Kind::Note, Kind::Shell];

    fn title(self) -> &'static str {
        match self {
            Kind::None => "Unassigned",
            Kind::Launch => "Launch application",
            Kind::Open => "Open path or URL",
            Kind::Note => "Quick note",
            Kind::Shell => "Shell command",
        }
    }

    /// What the target field is called for this kind.
    fn target_label(self) -> &'static str {
        match self {
            Kind::Launch => "EXECUTABLE",
            Kind::Open => "TARGET",
            Kind::Note => "NOTE NAME",
            Kind::Shell => "COMMAND",
            Kind::None => "TARGET",
        }
    }
}

struct Editor {
    key: GKey,
    kind: Kind,
    label: String,
    target: String,
    args: String,
    workdir: String,
    focus_if_running: bool,
    hidden: bool,
}

impl Editor {
    fn load(key: GKey, config: &Config) -> Editor {
        let mut editor = Editor {
            key,
            kind: Kind::None,
            label: String::new(),
            target: String::new(),
            args: String::new(),
            workdir: String::new(),
            focus_if_running: false,
            hidden: true,
        };

        if let Some(slot) = config.slot(key) {
            editor.label = slot.label.clone();
            match &slot.action {
                Action::None => {}
                Action::Launch {
                    target,
                    args,
                    workdir,
                    focus_if_running,
                } => {
                    editor.kind = Kind::Launch;
                    editor.target = target.clone();
                    editor.args = args.clone();
                    editor.workdir = workdir.clone();
                    editor.focus_if_running = *focus_if_running;
                }
                Action::Open { target } => {
                    editor.kind = Kind::Open;
                    editor.target = target.clone();
                }
                Action::Note { name } => {
                    editor.kind = Kind::Note;
                    editor.target = name.clone();
                }
                Action::Shell { command, hidden } => {
                    editor.kind = Kind::Shell;
                    editor.target = command.clone();
                    editor.hidden = *hidden;
                }
            }
        }
        editor
    }

    /// Build a slot, or explain why it cannot be built.
    ///
    /// The same validation the config parser applies, so the interface cannot
    /// write a file that the parser would then refuse.
    fn to_slot(&self, shell_enabled: bool) -> Result<Option<Slot>, String> {
        if self.kind == Kind::None {
            return Ok(None);
        }

        let label = self.label.trim();
        if label.is_empty() {
            return Err("Give the key a label.".to_string());
        }
        let target = self.target.trim();
        if target.is_empty() {
            return Err(format!("{} is empty.", self.kind.target_label()));
        }

        let action = match self.kind {
            Kind::None => unreachable!("handled above"),
            Kind::Launch | Kind::Open => {
                classify_target(target)?;
                if self.kind == Kind::Launch {
                    Action::Launch {
                        target: target.to_string(),
                        args: self.args.trim().to_string(),
                        workdir: self.workdir.trim().to_string(),
                        focus_if_running: self.focus_if_running,
                    }
                } else {
                    Action::Open {
                        target: target.to_string(),
                    }
                }
            }
            Kind::Note => {
                if target.contains(['\\', '/', ':']) {
                    return Err("A note name is a plain name, not a path.".to_string());
                }
                Action::Note {
                    name: target.to_string(),
                }
            }
            Kind::Shell => {
                if !shell_enabled {
                    return Err(
                        "Shell actions are disabled. Turn on shell_enabled in the config first."
                            .to_string(),
                    );
                }
                Action::Shell {
                    command: target.to_string(),
                    hidden: self.hidden,
                }
            }
        };

        Ok(Some(Slot {
            label: label.to_string(),
            action,
        }))
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// What the window is being used for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Editing bindings: the grid plus the editor panel.
    Edit,
    /// A deck: nothing but keys, and a tap fires one.
    Deck,
    /// The security descriptors on the config store.
    Security,
}

/// One object in the store, with what is on it and what should be.
struct SecuredObject {
    label: &'static str,
    path: std::path::PathBuf,
    name: String,
    actual: String,
    expected: String,
    matches: bool,
    exposed: Vec<&'static str>,
}

#[derive(Default)]
struct SecurityView {
    loaded: bool,
    objects: Vec<SecuredObject>,
    /// Paste your own descriptor here and apply it verbatim.
    custom: String,
    custom_target: usize,
    message: Option<(String, bool)>,
}

/// Where the tiles are this frame.
///
/// A drop has to be resolved to a key from a pointer position, and the tile
/// that was dropped on may not have been drawn yet when the dragged one
/// reports the release. Keeping the geometry means the answer does not depend
/// on the order the tiles happened to be drawn in.
#[derive(Clone, Copy)]
struct Grid {
    origin: egui::Pos2,
    tile: Vec2,
    gap: f32,
}

impl Grid {
    /// The key under a point. `None` for the gaps between tiles and for
    /// anywhere outside the grid, so a drop that lands between two tiles does
    /// nothing rather than picking whichever one the arithmetic rounds to.
    fn key_at(&self, pos: egui::Pos2) -> Option<GKey> {
        let dx = pos.x - self.origin.x;
        let dy = pos.y - self.origin.y;
        if dx < 0.0 || dy < 0.0 {
            return None;
        }
        let pitch_x = self.tile.x + self.gap;
        let pitch_y = self.tile.y + self.gap;
        let column = (dx / pitch_x).floor();
        let row = (dy / pitch_y).floor();
        if !(0.0..3.0).contains(&column) || !(0.0..6.0).contains(&row) {
            return None;
        }
        if dx - column * pitch_x > self.tile.x || dy - row * pitch_y > self.tile.y {
            return None;
        }
        GKey::from_index(row as usize * 3 + column as usize)
    }
}

struct App {
    config: Config,
    /// The copy the reader thread fires from.
    shared: Arc<Mutex<Config>>,
    /// Shared with the reader thread so that changing the policy takes effect
    /// everywhere at once, rather than leaving the thread on a stale copy.
    policy: Arc<Mutex<Option<Policy>>>,
    include_admins: bool,
    security: SecurityView,
    config_problem: Option<String>,
    integrity: String,
    integrity_ok: bool,
    /// What the Run key says right now, read at startup and after a change.
    autostart: autostart::State,
    /// The tile geometry for this frame, used to resolve where a drag ended.
    grid: Option<Grid>,
    /// The key currently being dragged onto another one.
    drag_from: Option<GKey>,
    /// False when the config changed outside the app: presses do nothing.
    armed: Arc<Mutex<bool>>,
    editor: Editor,
    mode: Mode,
    fullscreen: bool,
    status: Option<(String, bool)>,
    live: Arc<Mutex<Live>>,
    /// Real icons, keyed by target. `None` means the shell had none for it,
    /// cached so a miss is not retried on every frame.
    icons: HashMap<String, Option<egui::TextureHandle>>,
    _reader: Option<Reader>,
    /// Dropped on the way out, which takes the notification icon with it.
    /// `None` when the icon could not be created, and then closing the
    /// window really does close the app, because nothing would bring it back.
    _tray: Option<tray::Tray>,
    /// Set when Quit is chosen, so the next close request is honoured
    /// instead of being turned into a hide.
    quitting: Arc<AtomicBool>,
}

fn describe_integrity() -> (String, bool, bool) {
    use gdeck::integrity::{check, Integrity};

    let path = match gdeck::store::config_path() {
        Ok(path) => path,
        Err(err) => return (format!("{}", err), false, false),
    };
    if !path.exists() {
        return ("no config yet".to_string(), true, true);
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => return (format!("unreadable: {}", err), false, false),
    };
    match check(&text) {
        Ok(Integrity::Intact) => ("config sealed and intact".to_string(), true, true),
        Ok(Integrity::NotSealed) => ("config not sealed".to_string(), false, true),
        Ok(Integrity::Modified { .. }) => ("CHANGED OUTSIDE THE APP".to_string(), false, false),
        Ok(Integrity::SealUnreadable { code }) => {
            (format!("seal unreadable ({})", code), false, false)
        }
        Err(err) => (format!("{}", err), false, false),
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);

        let (config, config_problem) = match gdeck::config::load() {
            Ok(config) => (config, None),
            Err(err) => (Config::default(), Some(format!("{}", err))),
        };
        let (integrity, integrity_ok, armed_now) = describe_integrity();
        let policy = Arc::new(Mutex::new(Sid::current_user().ok().map(Policy::lockdown)));

        let live = Arc::new(Mutex::new(Live::default()));
        let shared = Arc::new(Mutex::new(config.clone()));
        let armed = Arc::new(Mutex::new(armed_now && config_problem.is_none()));

        let reader = match Reader::start() {
            Ok((reader, events)) => {
                let live_for_thread = Arc::clone(&live);
                let shared_for_thread = Arc::clone(&shared);
                let armed_for_thread = Arc::clone(&armed);
                let policy_for_thread = Arc::clone(&policy);
                let ctx = cc.egui_ctx.clone();

                thread::spawn(move || {
                    while let Ok(event) = events.recv() {
                        let mut pressed = None;
                        {
                            let mut state = live_for_thread.lock().expect("live state");
                            match event {
                                Event::Connected => state.connected = true,
                                Event::Disconnected => {
                                    state.connected = false;
                                    state.held = 0;
                                }
                                Event::Pressed(key) => {
                                    state.held |= 1 << key.index();
                                    state.last_press[key.index()] = Some(Instant::now());
                                    pressed = Some(key);
                                }
                                Event::Released(key) => state.held &= !(1 << key.index()),
                            }
                        }
                        ctx.request_repaint();

                        if let Some(key) = pressed {
                            // Carried out off the reader thread. A shell
                            // command can take seconds to finish, and the event
                            // pump has to keep draining while it does.
                            let shared = Arc::clone(&shared_for_thread);
                            let armed = Arc::clone(&armed_for_thread);
                            let live = Arc::clone(&live_for_thread);
                            let policy = Arc::clone(&policy_for_thread);
                            let ctx = ctx.clone();
                            thread::spawn(move || {
                                fire(key, &shared, &armed, &policy, &live, &ctx);
                            });
                        }
                    }
                });
                Some(reader)
            }
            Err(_) => None,
        };

        let quitting = Arc::new(AtomicBool::new(false));

        // The icon runs its own message only window, because winit owns the
        // procedure belonging to the app window. Its clicks arrive on a
        // channel, and a forwarder thread turns them into viewport commands:
        // a hidden window draws no frames, so nothing would poll that channel.
        let tray = match tray::start("G Deck") {
            Some((handle, events)) => {
                let ctx = cc.egui_ctx.clone();
                let quitting_for_thread = Arc::clone(&quitting);
                thread::spawn(move || {
                    while let Ok(event) = events.recv() {
                        match event {
                            tray::TrayEvent::Open => {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                            }
                            tray::TrayEvent::Quit => {
                                quitting_for_thread.store(true, Ordering::SeqCst);
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                        ctx.request_repaint();
                    }
                });
                Some(handle)
            }
            None => None,
        };

        let editor = Editor::load(GKey::from_index(0).expect("G1"), &config);

        App {
            config,
            shared,
            policy,
            config_problem,
            integrity,
            integrity_ok,
            armed,
            editor,
            include_admins: false,
            security: SecurityView::default(),
            mode: Mode::Edit,
            fullscreen: false,
            status: None,
            live,
            icons: HashMap::new(),
            autostart: autostart::state(),
            grid: None,
            drag_from: None,
            _reader: reader,
            _tray: tray,
            quitting,
        }
    }

    /// The real icon for a target, loaded once and kept.
    fn icon_for(&mut self, ctx: &egui::Context, target: &str) -> Option<egui::TextureHandle> {
        if let Some(cached) = self.icons.get(target) {
            return cached.clone();
        }

        let loaded = appicon::load(target).map(|pixels| {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [pixels.width, pixels.height],
                &pixels.rgba,
            );
            ctx.load_texture(
                format!("icon:{}", target),
                image,
                egui::TextureOptions::LINEAR,
            )
        });

        self.icons.insert(target.to_string(), loaded.clone());
        loaded
    }

    /// Forget cached icons so a changed target picks up its new one.
    fn forget_icons(&mut self) {
        self.icons.clear();
    }

    /// Carry out a key's action from a tap, exactly as a physical press does.
    ///
    /// Runs on its own thread for the same reason the reader does: a shell
    /// command can take seconds and the interface has to stay responsive.
    fn fire_key(&self, key: GKey, ctx: &egui::Context) {
        {
            let mut state = self.live.lock().expect("live state");
            state.last_press[key.index()] = Some(Instant::now());
        }

        let shared = Arc::clone(&self.shared);
        let armed = Arc::clone(&self.armed);
        let live = Arc::clone(&self.live);
        let policy = Arc::clone(&self.policy);
        let ctx = ctx.clone();
        thread::spawn(move || {
            fire(key, &shared, &armed, &policy, &live, &ctx);
        });
    }

    /// The policy as it stands, shared with the reader thread.
    fn policy_now(&self) -> Option<Policy> {
        self.policy.lock().ok().and_then(|policy| policy.clone())
    }

    /// Read what is actually on every object in the store and compare it to
    /// what the policy asks for.
    fn refresh_security(&mut self) {
        use gdeck::security::{forbidden_present, normalize_sddl, read_sddl};

        self.security.loaded = true;
        self.security.objects.clear();

        let policy = match self.policy_now() {
            Some(policy) => policy,
            None => {
                self.security.message =
                    Some(("Could not resolve your account.".to_string(), false));
                return;
            }
        };
        let dir = match gdeck::store::store_dir() {
            Ok(dir) => dir,
            Err(err) => {
                self.security.message = Some((format!("{}", err), false));
                return;
            }
        };

        let wanted: [(&'static str, std::path::PathBuf, String); 4] = [
            ("store directory", dir.clone(), policy.directory_sddl()),
            (
                "config",
                dir.join(gdeck::store::CONFIG_FILE),
                policy.file_sddl(),
            ),
            (
                "seal",
                dir.join(gdeck::integrity::SEAL_FILE),
                policy.file_sddl(),
            ),
            (
                "audit log",
                dir.join(gdeck::store::AUDIT_FILE),
                policy.audit_sddl(),
            ),
        ];

        for (label, path, expected) in wanted {
            if !path.exists() {
                continue;
            }
            let actual = match read_sddl(&path) {
                Ok(sddl) => normalize_sddl(&sddl).unwrap_or(sddl),
                Err(err) => format!("could not read: {}", err),
            };
            let expected = normalize_sddl(&expected).unwrap_or(expected);
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string());

            self.security.objects.push(SecuredObject {
                label,
                matches: actual == expected,
                exposed: forbidden_present(&actual),
                name,
                path,
                actual,
                expected,
            });
        }

        if self.security.custom_target >= self.security.objects.len() {
            self.security.custom_target = 0;
        }
    }

    /// Put the policy back on every object in the store.
    fn reapply_policy(&mut self) {
        let policy = match self.policy_now() {
            Some(policy) => policy,
            None => {
                self.security.message =
                    Some(("Could not resolve your account.".to_string(), false));
                return;
            }
        };
        self.security.message = match gdeck::store::secure_all(&policy) {
            Ok(secured) => Some((
                format!("Applied the policy to {} object(s).", secured.len()),
                true,
            )),
            Err(err) => Some((format!("{}", err), false)),
        };
        self.refresh_security();
    }

    /// Add or remove the `BUILTIN\Administrators` entry, then re-apply.
    fn set_include_admins(&mut self, on: bool) {
        let owner = match self.policy_now() {
            Some(policy) => policy.owner,
            None => {
                self.security.message =
                    Some(("Could not resolve your account.".to_string(), false));
                return;
            }
        };

        let next = if on {
            Policy::with_administrators(owner)
        } else {
            Policy::lockdown(owner)
        };
        *self.policy.lock().expect("policy") = Some(next);
        self.include_admins = on;
        self.reapply_policy();
    }

    /// Apply a descriptor typed or pasted by hand, exactly as given.
    ///
    /// It goes through the same `security::apply` the built in policy uses, so
    /// a custom descriptor is not a second, looser path.
    fn apply_custom_sddl(&mut self) {
        let sddl = self.security.custom.trim().to_string();
        if sddl.is_empty() {
            self.security.message = Some(("Paste a descriptor first.".to_string(), false));
            return;
        }

        let path = match self.security.objects.get(self.security.custom_target) {
            Some(object) => object.path.clone(),
            None => {
                self.security.message = Some(("Nothing selected to apply it to.".to_string(), false));
                return;
            }
        };

        self.security.message = match gdeck::security::apply(&path, &sddl) {
            Ok(()) => Some((format!("Applied to {}.", path.display()), true)),
            Err(err) => Some((format!("{}", err), false)),
        };
        self.refresh_security();
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    fn leave_deck(&mut self, ctx: &egui::Context) {
        self.mode = Mode::Edit;
        if self.fullscreen {
            self.set_fullscreen(ctx, false);
        }
    }

    fn save(&mut self) {
        let policy = match self.policy_now() {
            Some(policy) => policy,
            None => {
                self.status = Some(("Could not resolve your account.".to_string(), false));
                return;
            }
        };

        let slot = match self.editor.to_slot(self.config.shell_enabled) {
            Ok(slot) => slot,
            Err(why) => {
                self.status = Some((why, false));
                return;
            }
        };

        let key = self.editor.key;
        let previous = self.config.slot(key).cloned();
        self.config.set_slot(key, slot);

        match gdeck::config::save(&self.config, &policy) {
            Ok(()) => {
                *self.shared.lock().expect("shared config") = self.config.clone();
                let (integrity, ok, armed) = describe_integrity();
                self.integrity = integrity;
                self.integrity_ok = ok;
                *self.armed.lock().expect("armed") = armed;
                // A changed target means a different icon.
                self.forget_icons();
                self.status = Some((format!("{} saved.", key), true));
            }
            Err(err) => {
                // Put it back; the file on disk is the truth.
                self.config.set_slot(key, previous);
                self.status = Some((format!("{}", err), false));
            }
        }
    }

    /// Move one key's assignment onto another, exchanging whatever was there.
    ///
    /// The 18 keys are physical, so there is no list to reorder. Reordering
    /// means changing which action sits on which key, which is a swap.
    fn swap_keys(&mut self, from: GKey, to: GKey) {
        if from == to {
            return;
        }
        let policy = match self.policy_now() {
            Some(policy) => policy,
            None => {
                self.status = Some(("Could not resolve your account.".to_string(), false));
                return;
            }
        };

        let moved = self.config.slot(from).cloned();
        let displaced = self.config.slot(to).cloned();
        if moved.is_none() && displaced.is_none() {
            // Two empty keys. Writing the file would change nothing, and a
            // write means re-sealing and re-applying descriptors.
            return;
        }
        let swapped = displaced.is_some();

        self.config.set_slot(to, moved.clone());
        self.config.set_slot(from, displaced.clone());

        match gdeck::config::save(&self.config, &policy) {
            Ok(()) => {
                *self.shared.lock().expect("shared config") = self.config.clone();
                let (integrity, ok, armed) = describe_integrity();
                self.integrity = integrity;
                self.integrity_ok = ok;
                *self.armed.lock().expect("armed") = armed;
                // The icon cache is keyed by target and a swap changes no
                // targets, so it stays valid and is deliberately not cleared.
                // Follow what was dragged, which now lives on the target.
                self.editor = Editor::load(to, &self.config);
                self.status = Some((
                    if swapped {
                        format!("{} and {} swapped.", from, to)
                    } else {
                        format!("Moved {} to {}.", from, to)
                    },
                    true,
                ));
            }
            Err(err) => {
                // The file on disk is the truth, so put both halves back.
                self.config.set_slot(from, moved);
                self.config.set_slot(to, displaced);
                self.status = Some((format!("{}", err), false));
            }
        }
    }

    fn clear(&mut self) {
        self.editor = Editor {
            key: self.editor.key,
            kind: Kind::None,
            label: String::new(),
            target: String::new(),
            args: String::new(),
            workdir: String::new(),
            focus_if_running: false,
            hidden: true,
        };
        self.save();
    }

    /// Turn shell actions on or off for the whole config, and save.
    /// Turn starting with Windows on or off, then re-read the key rather
    /// than assuming the write landed.
    fn set_autostart(&mut self, on: bool) {
        let outcome = if on {
            autostart::enable()
        } else {
            autostart::disable()
        };
        match outcome {
            Ok(()) => {
                self.autostart = autostart::state();
                self.security.message = Some((
                    if on {
                        "G Deck will start when you sign in.".to_string()
                    } else {
                        "G Deck will not start when you sign in.".to_string()
                    },
                    true,
                ));
            }
            Err(code) => {
                self.security.message =
                    Some((format!("Could not change it, code {}.", code), false));
            }
        }
    }

    fn set_shell_enabled(&mut self, on: bool) {
        // Turning it off while a key still runs a shell command would write a
        // file the parser then refuses to load, locking everything out. Say so
        // instead of producing one.
        if !on {
            let bound: Vec<String> = GKey::all()
                .filter(|key| {
                    matches!(
                        self.config.slot(*key).map(|s| &s.action),
                        Some(Action::Shell { .. })
                    )
                })
                .map(|key| key.to_string())
                .collect();
            if !bound.is_empty() {
                self.status = Some((
                    format!("Clear the shell action on {} first.", bound.join(", ")),
                    false,
                ));
                return;
            }
        }

        let policy = match self.policy_now() {
            Some(policy) => policy,
            None => {
                self.status = Some(("Could not resolve your account.".to_string(), false));
                return;
            }
        };

        let previous = self.config.shell_enabled;
        self.config.shell_enabled = on;
        match gdeck::config::save(&self.config, &policy) {
            Ok(()) => {
                *self.shared.lock().expect("shared config") = self.config.clone();
                let (integrity, ok, armed) = describe_integrity();
                self.integrity = integrity;
                self.integrity_ok = ok;
                *self.armed.lock().expect("armed") = armed;
                self.status = Some((
                    if on {
                        "Shell actions are now allowed.".to_string()
                    } else {
                        "Shell actions are now refused.".to_string()
                    },
                    true,
                ));
            }
            Err(err) => {
                self.config.shell_enabled = previous;
                self.status = Some((format!("{}", err), false));
            }
        }
    }

    fn select(&mut self, key: GKey) {
        if self.editor.key != key {
            self.editor = Editor::load(key, &self.config);
            self.status = None;
        }
    }
}

/// Carry out a key press, on the reader thread.
fn fire(
    key: GKey,
    shared: &Arc<Mutex<Config>>,
    armed: &Arc<Mutex<bool>>,
    policy: &Arc<Mutex<Option<Policy>>>,
    live: &Arc<Mutex<Live>>,
    ctx: &egui::Context,
) {
    let mut shell_output: Option<(Option<u32>, String)> = None;

    let message = {
        if !*armed.lock().expect("armed") {
            Some((
                "Not armed: the config changed outside the app.".to_string(),
                false,
            ))
        } else {
            let config = shared.lock().expect("shared config");
            let policy = policy.lock().expect("policy");
            match (config.slot(key), policy.as_ref()) {
                (None, _) => None,
                (Some(_), None) => Some(("No account policy available.".to_string(), false)),
                (Some(slot), Some(policy)) => {
                    let context = actions::Context {
                        shell_enabled: config.shell_enabled,
                        policy,
                    };
                    let (action, detail) = actions::describe(slot);
                    let result = actions::perform(slot, &context);
                    let (text, ok, outcome) = match &result {
                        Ok(Performed::Nothing) => (format!("{} is unassigned", key), true, None),
                        Ok(Performed::Launched { .. }) => {
                            (format!("{} launched {}", key, slot.label), true, Some(Outcome::Started))
                        }
                        Ok(Performed::Focused) => (
                            format!("{} focused {}", key, slot.label),
                            true,
                            Some(Outcome::Started),
                        ),
                        Ok(Performed::Opened) => (
                            format!("{} opened {}", key, slot.label),
                            true,
                            Some(Outcome::Started),
                        ),
                        Ok(Performed::ShowNote { .. }) => (
                            format!("{} note {}", key, slot.label),
                            true,
                            Some(Outcome::Started),
                        ),
                        Ok(Performed::RanShell(run)) => {
                            shell_output = Some((run.exit_code, run.output.clone()));
                            // The LAST non-empty line, not the first. Many
                            // commands open with a banner: ipconfig prints
                            // "Windows IP Configuration" before it prints what
                            // it actually did. The full text is shown in the
                            // panel, so this only has to be a one line gist.
                            let summary = run
                                .output
                                .lines()
                                .rev()
                                .find(|line| !line.trim().is_empty())
                                .unwrap_or("")
                                .trim()
                                .to_string();
                            match run.exit_code {
                                Some(0) => (
                                    if summary.is_empty() {
                                        format!("{} {} succeeded", key, slot.label)
                                    } else {
                                        format!("{} {}", key, summary)
                                    },
                                    true,
                                    Some(Outcome::Started),
                                ),
                                Some(code) => (
                                    if summary.is_empty() {
                                        format!("{} {} failed, exit {}", key, slot.label, code)
                                    } else {
                                        format!("{} failed ({}): {}", key, code, summary)
                                    },
                                    false,
                                    Some(Outcome::Failed),
                                ),
                                None => (
                                    format!("{} {} still running", key, slot.label),
                                    true,
                                    Some(Outcome::Started),
                                ),
                            }
                        }
                        Err(gdeck::Error::Refused { reason }) => {
                            (format!("{} refused: {}", key, reason), false, Some(Outcome::Refused))
                        }
                        Err(err) => {
                            (format!("{} failed: {}", key, err), false, Some(Outcome::Failed))
                        }
                    };
                    if let Some(outcome) = outcome {
                        let _ = gdeck::audit::record(Some(key), action, outcome, &detail);
                    }
                    Some((text, ok))
                }
            }
        }
    };

    if message.is_some() || shell_output.is_some() {
        {
            let mut state = live.lock().expect("live state");
            if let Some(message) = message {
                state.last_result = Some(message);
            }
            if shell_output.is_some() {
                state.last_shell = shell_output;
            }
        }
        ctx.request_repaint();
    }
}

fn flash_amount(last: Option<Instant>, held: bool) -> f32 {
    if held {
        return 1.0;
    }
    match last {
        Some(at) => {
            let elapsed = at.elapsed();
            if elapsed >= FLASH {
                0.0
            } else {
                1.0 - (elapsed.as_secs_f32() / FLASH.as_secs_f32())
            }
        }
        None => 0.0,
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(12.0);
    ui.label(
        RichText::new(text)
            .size(10.0)
            .color(theme::MUTED),
    );
    ui.add_space(3.0);
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (connected, held, last_press, last_result, last_shell) = {
            let state = self.live.lock().expect("live state");
            (
                state.connected,
                state.held,
                state.last_press,
                state.last_result.clone(),
                state.last_shell.clone(),
            )
        };

        if (0..G_KEY_COUNT).any(|i| flash_amount(last_press[i], false) > 0.0) {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }

        // Closing the window hides it rather than ending the process, so the
        // G keys keep working. Quit on the tray menu is the way out. With no
        // icon in the tray there would be no way back, so then a close is a
        // close.
        let closing = ui.input(|input| input.viewport().close_requested());
        if closing && self._tray.is_some() && !self.quitting.load(Ordering::SeqCst) {
            let ctx = ui.ctx().clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // F11 toggles fullscreen anywhere. Escape leaves deck mode, which
        // matters because fullscreen with no window chrome has no other exit.
        let (wants_fullscreen, wants_out) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::F11),
                input.key_pressed(egui::Key::Escape),
            )
        });
        if wants_fullscreen {
            let on = !self.fullscreen;
            let ctx = ui.ctx().clone();
            self.set_fullscreen(&ctx, on);
        }
        if wants_out && self.mode == Mode::Deck {
            self.leave_deck(&ui.ctx().clone());
        }

        match self.mode {
            Mode::Edit | Mode::Security => {
                egui::Panel::top("header")
                    .frame(
                        egui::Frame::default()
                            .fill(theme::SURFACE)
                            .inner_margin(egui::Margin::symmetric(18, 12)),
                    )
                    .show(ui, |ui| self.draw_header(ui, connected));

                // The editor belongs to a key, so it has no place in the
                // security view.
                if self.mode == Mode::Edit {
                    egui::Panel::right("editor")
                        .exact_size(300.0)
                        .resizable(false)
                        .frame(
                            egui::Frame::default()
                                .fill(theme::SURFACE)
                                .inner_margin(egui::Margin::same(18)),
                        )
                        .show(ui, |ui| self.draw_editor(ui, last_shell));
                }

                egui::Panel::bottom("footer")
                    .frame(
                        egui::Frame::default()
                            .fill(theme::BG)
                            // Same padding as the header, so the bands match.
                            .inner_margin(egui::Margin::symmetric(18, 12)),
                    )
                    .show(ui, |ui| self.draw_footer(ui, last_result));
            }
            Mode::Deck => {
                egui::Panel::top("deck.bar")
                    .frame(
                        egui::Frame::default()
                            .fill(theme::BG)
                            .inner_margin(egui::Margin::symmetric(14, 7)),
                    )
                    .show(ui, |ui| self.draw_deck_bar(ui, connected, last_result));
            }
        }

        let margin = if self.mode == Mode::Deck { 12 } else { 18 };
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    // One value for all four sides. The grid fills the
                    // remaining rectangle exactly, so these margins are the
                    // gaps you see around it, and anything other than `same`
                    // shows up immediately as a lopsided border.
                    .inner_margin(egui::Margin::same(margin)),
            )
            .show(ui, |ui| match self.mode {
                Mode::Security => self.draw_security(ui),
                _ => self.draw_grid(ui, held, &last_press),
            });
    }
}

impl App {
    fn draw_header(&mut self, ui: &mut egui::Ui, connected: bool) {
        ui.horizontal(|ui| {
            // No name or device here. The window's own title bar already says
            // G Deck, and the status on the right already names the keyboard,
            // so both would only be the window repeating itself.
            if ui
                .add(
                    egui::Button::new(RichText::new("Deck mode").size(11.0))
                        .fill(theme::SURFACE_2),
                )
                .on_hover_text(
                    "Hide the editor and tap keys to fire them, for a touch screen.\n\
                     Escape comes back, F11 toggles fullscreen.",
                )
                .clicked()
            {
                self.mode = Mode::Deck;
            }

            let in_security = self.mode == Mode::Security;
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(if in_security { "Back to keys" } else { "Security" })
                            .size(11.0)
                            .color(if in_security {
                                Color32::WHITE
                            } else {
                                theme::FG_2
                            }),
                    )
                    .fill(if in_security {
                        theme::ACCENT
                    } else {
                        theme::SURFACE_2
                    }),
                )
                .on_hover_text("Access control on the config store")
                .clicked()
            {
                self.mode = if in_security { Mode::Edit } else { Mode::Security };
                // Always read the descriptors fresh when the view opens.
                self.security.loaded = false;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (colour, text) = if connected {
                    (theme::GOOD, "K95 RGB connected")
                } else {
                    (theme::CRIT, "keyboard not found")
                };
                ui.label(RichText::new(text).size(12.0).color(colour));
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, colour);
                ui.add_space(14.0);
                ui.label(
                    RichText::new(&self.integrity).size(12.0).color(
                        if self.integrity_ok {
                            theme::MUTED
                        } else {
                            theme::WARN
                        },
                    ),
                );
            });
        });
    }

    /// The one thin strip deck mode keeps: enough to know it is alive and to
    /// get back out, and nothing else competing with the keys.
    fn draw_deck_bar(
        &mut self,
        ui: &mut egui::Ui,
        connected: bool,
        last_result: Option<(String, bool)>,
    ) {
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            let colour = if connected { theme::GOOD } else { theme::CRIT };
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.5, colour);

            if !connected {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("keyboard not found")
                        .size(11.0)
                        .color(theme::CRIT),
                );
            }

            if let Some((text, ok)) = &last_result {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(text)
                        .size(12.0)
                        .color(if *ok { theme::GOOD } else { theme::CRIT }),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("Edit").size(11.0))
                            .fill(theme::SURFACE_2),
                    )
                    .on_hover_text("Back to editing. Escape does the same.")
                    .clicked()
                {
                    self.leave_deck(&ctx);
                }
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(if self.fullscreen {
                                "Windowed"
                            } else {
                                "Fullscreen"
                            })
                            .size(11.0),
                        )
                        .fill(theme::SURFACE_2),
                    )
                    .on_hover_text("F11")
                    .clicked()
                {
                    let on = !self.fullscreen;
                    self.set_fullscreen(&ctx, on);
                }
            });
        });
    }

    /// What is actually on the store right now, next to what the policy wants.
    fn draw_security(&mut self, ui: &mut egui::Ui) {
        if !self.security.loaded {
            self.refresh_security();
        }

        // Buttons set flags rather than calling straight through, because the
        // list below is borrowed while it is being drawn.
        let mut refresh = false;
        let mut reapply = false;
        let mut admins_to: Option<bool> = None;
        let mut select_target: Option<usize> = None;
        let mut apply_custom = false;
        let mut autostart_to: Option<bool> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(RichText::new("CONFIG STORE").size(10.0).color(theme::MUTED));
            ui.label(
                RichText::new("Access control on the files that decide what runs")
                    .size(17.0)
                    .strong()
                    .color(theme::FG),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "This does not stop an administrator. Anyone with those rights can \
                     take ownership and rewrite any descriptor. What it does stop is other \
                     standard accounts on this machine, inherited permission accidents, \
                     and silent same privilege access, because anyone who is not the owner \
                     has to take ownership first, and that changes the owner shown below.",
                )
                .size(11.0)
                .color(theme::MUTED),
            );

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("Re-check").size(12.0))
                            .fill(theme::SURFACE_2),
                    )
                    .clicked()
                {
                    refresh = true;
                }
                if ui
                    .add(
                        egui::Button::new(RichText::new("Re-apply policy").size(12.0))
                            .fill(theme::SURFACE_2),
                    )
                    .on_hover_text("Puts the descriptors below back to what the policy asks for")
                    .clicked()
                {
                    reapply = true;
                }
                ui.add_space(12.0);

                let mut admins = self.include_admins;
                if ui
                    .checkbox(
                        &mut admins,
                        RichText::new("Grant BUILTIN\\Administrators").size(12.0),
                    )
                    .on_hover_text(
                        "Off by default. It does not lock administrators out either way, \
                         since they can take ownership, but leaving it out means they have \
                         to, and that is visible here.",
                    )
                    .changed()
                {
                    admins_to = Some(admins);
                }
            });

            if let Some((text, ok)) = &self.security.message {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(text)
                        .size(12.0)
                        .color(if *ok { theme::GOOD } else { theme::CRIT }),
                );
            }

            ui.add_space(14.0);

            if self.security.objects.is_empty() {
                ui.label(
                    RichText::new(
                        "The store does not exist yet. It is created the first time a key \
                         is saved.",
                    )
                    .size(12.0)
                    .color(theme::MUTED),
                );
            }

            for (index, object) in self.security.objects.iter().enumerate() {
                let healthy = object.matches && object.exposed.is_empty();
                egui::Frame::default()
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(
                        1.0,
                        if healthy { theme::LINE } else { theme::WARN },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(13, 11))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(object.label.to_uppercase())
                                    .size(10.0)
                                    .color(theme::MUTED),
                            );
                            ui.add_space(8.0);
                            ui.label(RichText::new(&object.name).size(13.0).color(theme::FG));

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (text, colour) = if !object.exposed.is_empty() {
                                        ("EXPOSED", theme::CRIT)
                                    } else if !object.matches {
                                        ("DRIFT", theme::WARN)
                                    } else {
                                        ("matches policy", theme::GOOD)
                                    };
                                    ui.label(RichText::new(text).size(11.0).color(colour));
                                },
                            );
                        });

                        ui.add_space(8.0);
                        for ace in gdeck::security::parse_aces(&object.actual) {
                            let who = ace
                                .trustee_name
                                .clone()
                                .unwrap_or_else(|| ace.trustee.clone());
                            ui.label(
                                RichText::new(format!("{}   {}", ace.kind, who))
                                    .size(12.0)
                                    .color(theme::FG_2),
                            );
                            ui.label(
                                RichText::new(format!("      {}   ·   {}", ace.rights, ace.flags))
                                    .size(11.0)
                                    .color(theme::MUTED),
                            );
                        }

                        for why in &object.exposed {
                            ui.add_space(5.0);
                            ui.label(
                                RichText::new(format!("EXPOSED: {}", why))
                                    .size(11.0)
                                    .color(theme::CRIT),
                            );
                        }

                        ui.add_space(8.0);
                        ui.label(RichText::new("on disk").size(10.0).color(theme::MUTED));
                        ui.label(
                            RichText::new(&object.actual)
                                .size(11.0)
                                .monospace()
                                .color(theme::FG_2),
                        );
                        if !object.matches {
                            ui.add_space(5.0);
                            ui.label(
                                RichText::new("the policy wants")
                                    .size(10.0)
                                    .color(theme::MUTED),
                            );
                            ui.label(
                                RichText::new(&object.expected)
                                    .size(11.0)
                                    .monospace()
                                    .color(theme::WARN),
                            );
                        }

                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Target for the descriptor below").size(11.0),
                                )
                                .fill(theme::SURFACE_2),
                            )
                            .clicked()
                        {
                            select_target = Some(index);
                        }
                    });
                ui.add_space(10.0);
            }

            ui.add_space(8.0);
            ui.label(
                RichText::new("YOUR OWN DESCRIPTOR")
                    .size(10.0)
                    .color(theme::MUTED),
            );
            let target = self
                .security
                .objects
                .get(self.security.custom_target)
                .map(|object| object.name.clone())
                .unwrap_or_else(|| "nothing selected".to_string());
            ui.label(
                RichText::new(format!(
                    "Applied verbatim to: {}. This goes through the same code the built \
                     in policy uses, so it is not a looser second path.",
                    target
                ))
                .size(11.0)
                .color(theme::FG_2),
            );
            ui.add_space(5.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.security.custom)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("O:<SID>D:PAI(A;;FA;;;<SID>)(A;;FA;;;SY)"),
            );
            ui.add_space(7.0);
            if ui
                .add(
                    egui::Button::new(RichText::new("Apply this descriptor").size(12.0))
                        .fill(theme::SURFACE_2),
                )
                .clicked()
            {
                apply_custom = true;
            }
            ui.add_space(22.0);
            ui.label(
                RichText::new("START WITH WINDOWS")
                    .size(10.0)
                    .color(theme::MUTED),
            );
            ui.label(
                RichText::new(format!(
                    "Off by default. Turning this on writes a single value under your own \
                     account, at {}, holding the quoted path to this executable. Quoted \
                     because an unquoted path containing a space lets Windows try shorter \
                     prefixes first. Never a service and never a scheduled task, since \
                     both can be made to run as SYSTEM. Turning it off deletes that value, \
                     and that is the whole uninstall.",
                    autostart::RUN_KEY_DISPLAY
                ))
                .size(11.0)
                .color(theme::MUTED),
            );
            ui.add_space(8.0);
            match &self.autostart {
                autostart::State::On => {
                    ui.label(
                        RichText::new("ON for this account")
                            .size(12.0)
                            .color(theme::GOOD),
                    );
                    ui.add_space(6.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Do not start with Windows").size(12.0),
                            )
                            .fill(theme::SURFACE_2),
                        )
                        .clicked()
                    {
                        autostart_to = Some(false);
                    }
                }
                autostart::State::Off => {
                    ui.label(RichText::new("OFF").size(12.0).color(theme::MUTED));
                    ui.add_space(6.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Start with Windows").size(12.0))
                                .fill(theme::SURFACE_2),
                        )
                        .clicked()
                    {
                        autostart_to = Some(true);
                    }
                }
                autostart::State::Stale { command } => {
                    ui.label(
                        RichText::new("ON, but pointing at a different file")
                            .size(12.0)
                            .color(theme::WARN),
                    );
                    ui.label(
                        RichText::new(command.clone())
                            .size(11.0)
                            .monospace()
                            .color(theme::FG_2),
                    );
                    ui.label(
                        RichText::new(
                            "Left alone rather than quietly corrected, because it may be \
                             a copy elsewhere that you meant to start.",
                        )
                        .size(11.0)
                        .color(theme::MUTED),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Point it at this copy").size(12.0),
                                )
                                .fill(theme::SURFACE_2),
                            )
                            .clicked()
                        {
                            autostart_to = Some(true);
                        }
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Remove it").size(12.0))
                                    .fill(theme::SURFACE_2),
                            )
                            .clicked()
                        {
                            autostart_to = Some(false);
                        }
                    });
                }
                autostart::State::Unreadable { code } => {
                    ui.label(
                        RichText::new(format!("Could not read the Run key, code {}", code))
                            .size(12.0)
                            .color(theme::CRIT),
                    );
                }
            }

            ui.add_space(20.0);
        });

        if refresh {
            self.refresh_security();
        }
        if reapply {
            self.reapply_policy();
        }
        if let Some(on) = admins_to {
            self.set_include_admins(on);
        }
        if let Some(index) = select_target {
            self.security.custom_target = index;
        }
        if apply_custom {
            self.apply_custom_sddl();
        }
        if let Some(on) = autostart_to {
            self.set_autostart(on);
        }
    }

    fn draw_footer(&mut self, ui: &mut egui::Ui, last_result: Option<(String, bool)>) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} / 18 KEYS", self.config.assigned_count()))
                    .size(11.0)
                    .color(theme::MUTED),
            );
            ui.add_space(14.0);
            ui.label(
                RichText::new(if self.config.shell_enabled {
                    "SHELL ENABLED"
                } else {
                    "SHELL DISABLED"
                })
                .size(11.0)
                .color(if self.config.shell_enabled {
                    theme::WARN
                } else {
                    theme::MUTED
                }),
            );
            ui.add_space(14.0);
            ui.label(
                RichText::new("LIGHTING UNTOUCHED")
                    .size(11.0)
                    .color(theme::MUTED),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Answers "where is the config?" without making anyone hunt
                // through AppData for it.
                let store = gdeck::store::store_dir().ok();
                let button = ui.add(
                    egui::Button::new(RichText::new("Open config folder").size(11.0))
                        .fill(theme::SURFACE_2),
                );
                let button = match &store {
                    Some(path) => button.on_hover_text(path.display().to_string()),
                    None => button,
                };
                if button.clicked() {
                    if let Some(path) = store {
                        if let Err(err) = gdeck::actions::open_path(&path) {
                            self.status = Some((format!("{}", err), false));
                        }
                    }
                }

                if let Some((text, ok)) = last_result {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(text)
                            .size(11.0)
                            .color(if ok { theme::GOOD } else { theme::CRIT }),
                    );
                }
            });
        });
    }

    fn draw_grid(&mut self, ui: &mut egui::Ui, held: u32, last_press: &[Option<Instant>; 18]) {
        // Lay the tiles out by arithmetic on one rectangle rather than by
        // stacking widgets. Flow layout adds its own spacing on top of the
        // gaps, so the grid ended up wider and shorter than the space it was
        // given, which is why the right and bottom margins did not match the
        // top and left ones. This fills the area exactly, by construction.
        let area = ui.available_rect_before_wrap();
        let gap = 10.0;
        let tile = Vec2::new(
            (area.width() - gap * 2.0) / 3.0,
            (area.height() - gap * 5.0) / 6.0,
        );

        self.grid = Some(Grid {
            origin: area.min,
            tile,
            gap,
        });

        for (index, last) in last_press.iter().enumerate() {
            let row = (index / 3) as f32;
            let column = (index % 3) as f32;
            let rect = egui::Rect::from_min_size(
                area.min + Vec2::new(column * (tile.x + gap), row * (tile.y + gap)),
                tile,
            );
            let key = GKey::from_index(index).expect("18 keys");
            self.draw_tile(ui, key, rect, held, *last);
        }

        self.draw_drag_ghost(ui);
    }

    /// What is in hand during a reorder, following the cursor.
    ///
    /// Without it the only feedback is which tiles are lit, which does not say
    /// what is being carried once the cursor has left the tile it came from.
    fn draw_drag_ghost(&self, ui: &egui::Ui) {
        let (Some(from), Some(pos)) = (self.drag_from, ui.ctx().pointer_latest_pos()) else {
            return;
        };
        let label = match self.config.slot(from) {
            Some(slot) => slot.label.clone(),
            None => "empty".to_string(),
        };
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            ui.scope_id().with("drag-ghost"),
        ));
        let galley = painter.layout_no_wrap(
            format!("{}  {}", from, label),
            FontId::proportional(12.0),
            theme::FG,
        );
        let chip = egui::Rect::from_min_size(
            pos + Vec2::new(14.0, 14.0),
            galley.size() + Vec2::new(18.0, 10.0),
        );
        painter.rect_filled(chip, CornerRadius::same(6), theme::SURFACE_3);
        painter.rect_stroke(
            chip,
            CornerRadius::same(6),
            Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
        painter.galley(chip.min + Vec2::new(9.0, 5.0), galley, theme::FG);
    }

    fn draw_tile(
        &mut self,
        ui: &mut egui::Ui,
        key: GKey,
        rect: egui::Rect,
        held: u32,
        last: Option<Instant>,
    ) {
        // Dragging reorders, and only in the editor. On a touch display in
        // deck mode a swipe across the tiles would otherwise rearrange the
        // deck by accident.
        let response = ui.allocate_rect(
            rect,
            if self.mode == Mode::Edit {
                Sense::click_and_drag()
            } else {
                Sense::click()
            },
        );

        if response.drag_started() {
            self.drag_from = Some(key);
        }
        if response.drag_stopped() {
            let landed = response
                .interact_pointer_pos()
                .and_then(|pos| self.grid.and_then(|grid| grid.key_at(pos)));
            if let Some(target) = landed {
                self.swap_keys(key, target);
            }
            self.drag_from = None;
        }

        if response.clicked() {
            match self.mode {
                // Editing: a click chooses which key to edit.
                Mode::Edit => self.select(key),
                // Deck: a tap firing the action is the entire point.
                Mode::Deck => self.fire_key(key, ui.ctx()),
                // The grid is not drawn in the security view, so this is
                // unreachable rather than deliberately ignored.
                Mode::Security => {}
            }
        }

        let is_held = held & (1 << key.index()) != 0;
        let lit = flash_amount(last, is_held);
        let selected = self.editor.key == key;

        // Taken as owned values on purpose: loading an icon needs `&mut self`,
        // which cannot coexist with a borrow of the config.
        let (label, kind, target) = match self.config.slot(key) {
            Some(slot) => (
                Some(slot.label.clone()),
                slot.action.type_name(),
                match &slot.action {
                    Action::Launch { target, .. } | Action::Open { target } => {
                        Some(target.clone())
                    }
                    _ => None,
                },
            ),
            None => (None, "none", None),
        };
        let texture = target
            .as_deref()
            .and_then(|target| self.icon_for(ui.ctx(), target));

        // A pressed key sinks slightly. Movement reads as a press in a way
        // that a colour change on its own does not.
        let rect = rect.shrink(lit * 3.0);

        let assigned = label.is_some();
        let base = if assigned {
            theme::SURFACE_2
        } else {
            theme::SURFACE
        };
        let mut fill = if response.hovered() {
            theme::SURFACE_3
        } else {
            base
        };
        if lit > 0.0 {
            fill = mix(fill, theme::ACCENT_DIM, lit * 0.85);
        }

        // Reorder feedback: what is in hand recedes, what it would land on
        // is ringed.
        let dragging = self.drag_from;
        let is_source = dragging == Some(key);
        let is_target = dragging.is_some()
            && !is_source
            && ui
                .ctx()
                .pointer_latest_pos()
                .and_then(|pos| self.grid.and_then(|grid| grid.key_at(pos)))
                == Some(key);
        if is_source {
            fill = mix(fill, theme::BG, 0.55);
        } else if is_target {
            fill = mix(theme::SURFACE_3, theme::ACCENT_DIM, 0.3);
        }

        let stroke = if is_target {
            Stroke::new(2.0, theme::ACCENT)
        } else if is_source {
            Stroke::new(1.0, theme::ACCENT_DIM)
        } else if lit > 0.0 {
            Stroke::new(1.0 + lit, theme::ACCENT)
        } else if selected {
            Stroke::new(1.5, theme::ACCENT)
        } else {
            Stroke::new(1.0, theme::LINE)
        };

        // Everything is a proportion of the tile, so one layout serves a
        // window on this desk and a full screen touch display without a
        // second set of numbers to keep in step.
        let pad = (rect.height() * 0.11).clamp(10.0, 26.0).round();
        let id_size = (rect.height() * 0.10).clamp(10.0, 20.0).round();
        let label_size = (rect.height() * 0.145).clamp(12.0, 32.0).round();
        let icon_size = (rect.height() * 0.28).clamp(18.0, 80.0);
        let radius = (rect.height() * 0.09).clamp(8.0, 18.0) as u8;

        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(radius), fill);
        painter.rect_stroke(
            rect,
            CornerRadius::same(radius),
            stroke,
            egui::StrokeKind::Inside,
        );

        painter.text(
            rect.left_top() + Vec2::new(pad, pad * 0.85),
            Align2::LEFT_TOP,
            key.to_string(),
            FontId::monospace(id_size),
            if lit > 0.0 || selected {
                theme::ACCENT
            } else {
                theme::MUTED
            },
        );

        // The program's own icon where the shell has one, the drawn glyph
        // otherwise, so a tile is never blank.
        let icon_top = rect.top() + pad * 0.85 + id_size + pad * 0.5;
        if icon_top + icon_size < rect.bottom() - label_size - pad {
            match &texture {
                Some(texture) => {
                    painter.image(
                        texture.id(),
                        egui::Rect::from_min_size(
                            egui::pos2(rect.left() + pad, icon_top),
                            Vec2::splat(icon_size),
                        ),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                None => {
                    icons::for_action(
                        painter,
                        egui::Rect::from_min_size(
                            egui::pos2(rect.left() + pad, icon_top + icon_size * 0.1),
                            Vec2::splat(icon_size * 0.8),
                        ),
                        kind,
                        if assigned {
                            theme::action_color(kind)
                        } else {
                            theme::LINE
                        },
                    );
                }
            }
        }

        if kind == "shell" {
            let warn = (icon_size * 0.55).clamp(11.0, 26.0);
            icons::warning(
                painter,
                egui::Rect::from_min_size(
                    egui::pos2(rect.right() - pad - warn, rect.top() + pad * 0.85),
                    Vec2::splat(warn),
                ),
                theme::WARN,
            );
        }

        let (text, colour) = match &label {
            Some(label) => (label.as_str(), theme::FG),
            None => ("Unassigned", theme::MUTED),
        };
        painter.text(
            rect.left_bottom() + Vec2::new(pad, -pad * 0.9),
            Align2::LEFT_BOTTOM,
            text,
            FontId::new(label_size, theme::strong()),
            colour,
        );
    }

    fn draw_editor(&mut self, ui: &mut egui::Ui, last_shell: Option<(Option<u32>, String)>) {
        // Claim the action strip before anything else is laid out. A bottom up
        // layout positions from the panel's bottom regardless of how much the
        // content above it used, so a tall output box simply grew down over
        // the buttons and the status line. A panel genuinely reserves the
        // space, leaving the rest as a hard budget for everything above.
        egui::Panel::bottom("editor.actions")
            .resizable(false)
            .frame(
                egui::Frame::default()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin {
                        left: 0,
                        right: 0,
                        top: 12,
                        bottom: 0,
                    }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // The gap between the two buttons is egui's own item
                    // spacing. Adding a second gap of my own on top of it is
                    // what pushed Save off the right edge, and is the same
                    // mistake that made the key grid overflow.
                    let gap = ui.spacing().item_spacing.x;
                    let button = ((ui.available_width() - gap) / 2.0).max(40.0);

                    if ui
                        .add_sized(
                            [button, 34.0],
                            egui::Button::new(RichText::new("Clear").size(13.0)),
                        )
                        .clicked()
                    {
                        self.clear();
                    }
                    if ui
                        .add_sized(
                            [button, 34.0],
                            egui::Button::new(
                                RichText::new("Save").size(13.0).color(Color32::WHITE),
                            )
                            .fill(theme::ACCENT),
                        )
                        .clicked()
                    {
                        self.save();
                    }
                });

                if let Some((text, ok)) = &self.status {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(text)
                            .size(11.0)
                            .color(if *ok { theme::GOOD } else { theme::CRIT }),
                    );
                }

                if let Some(problem) = &self.config_problem {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("Config rejected: {}", problem))
                            .size(11.0)
                            .color(theme::CRIT),
                    );
                }
            });

        ui.label(
            RichText::new("EDITING")
                .size(10.0)
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new(format!("Key {}", self.editor.key))
                .size(21.0)
                .strong()
                .color(theme::FG),
        );

        heading(ui, "ACTION TYPE");
        ui.horizontal_wrapped(|ui| {
            for kind in Kind::ALL {
                let selected = self.editor.kind == kind;
                let text = RichText::new(kind.title()).size(12.0).color(if selected {
                    Color32::WHITE
                } else {
                    theme::FG_2
                });
                if ui.selectable_label(selected, text).clicked() {
                    self.editor.kind = kind;
                    self.status = None;
                }
            }
        });

        if self.editor.kind != Kind::None {
            heading(ui, "LABEL");
            ui.add(
                egui::TextEdit::singleline(&mut self.editor.label)
                    .desired_width(f32::INFINITY)
                    .hint_text("Shown on the key"),
            );

            heading(ui, self.editor.kind.target_label());

            let hint = match self.editor.kind {
                Kind::Launch => r"C:\Program Files\App\app.exe",
                Kind::Open => r"C:\folder  or  https://example.com",
                Kind::Note => "a plain name",
                Kind::Shell => "a command line",
                Kind::None => "",
            };
            let browsable = matches!(self.editor.kind, Kind::Launch | Kind::Open);

            ui.horizontal(|ui| {
                // Same rule as the Clear and Save row: the gap between widgets
                // is egui's item spacing, so budget for that one and never add
                // a second by hand.
                let gap = ui.spacing().item_spacing.x;
                let button = 72.0;
                let reserved = if browsable { button + gap } else { 0.0 };
                let field = (ui.available_width() - reserved).max(60.0);

                ui.add_sized(
                    [field, 22.0],
                    egui::TextEdit::singleline(&mut self.editor.target)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(hint),
                );

                if browsable {
                    let picked = ui
                        .add_sized(
                            [button, 22.0],
                            egui::Button::new(RichText::new("Browse").size(11.0)),
                        )
                        .on_hover_text("Choose the file instead of typing the path")
                        .clicked();
                    if picked {
                        let chosen = if self.editor.kind == Kind::Launch {
                            filedialog::pick_program()
                        } else {
                            filedialog::pick_any_file()
                        };
                        if let Some(path) = chosen {
                            self.editor.target = path;
                            self.status = None;
                        }
                    }
                }
            });

            if self.editor.kind == Kind::Open {
                ui.add_space(4.0);
                if ui
                    .add(egui::Button::new(
                        RichText::new("Pick a folder instead").size(11.0),
                    ))
                    .clicked()
                {
                    if let Some(path) = filedialog::pick_folder() {
                        self.editor.target = path;
                        self.status = None;
                    }
                }
            }

            match self.editor.kind {
                Kind::Launch => {
                    heading(ui, "ARGUMENTS");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.editor.args)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("optional"),
                    );
                    heading(ui, "OPTIONS");
                    ui.checkbox(
                        &mut self.editor.focus_if_running,
                        RichText::new("Focus if already running").size(12.0),
                    );
                }
                Kind::Shell => {
                    heading(ui, "OPTIONS");
                    ui.checkbox(
                        &mut self.editor.hidden,
                        RichText::new("Run hidden, report the result").size(12.0),
                    )
                    .on_hover_text(
                        "Checked: no window. The command is waited for and its exit \
                         code and output appear at the bottom of this window.\n\n\
                         Unchecked: the command gets a console window of its own, so \
                         its output goes there rather than here.",
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Shell actions run an arbitrary command line. They are \
                             refused unless they are turned on for the whole config.",
                        )
                        .size(11.0)
                        .color(theme::WARN),
                    );
                    ui.add_space(5.0);
                    if self.config.shell_enabled {
                        if ui
                            .add(egui::Button::new(
                                RichText::new("Turn shell actions off").size(11.0),
                            ))
                            .clicked()
                        {
                            self.set_shell_enabled(false);
                        }
                    } else if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Turn shell actions on").size(11.0).color(theme::WARN),
                            )
                            .stroke(Stroke::new(1.0, theme::WARN)),
                        )
                        .on_hover_text("Writes shell_enabled to the config and re-seals it")
                        .clicked()
                    {
                        self.set_shell_enabled(true);
                    }

                    // The whole output, not a one line guess at which part of
                    // it mattered.
                    if let Some((code, output)) = &last_shell {
                        ui.add_space(12.0);
                        ui.label(RichText::new("LAST RUN").size(10.0).color(theme::MUTED));
                        ui.add_space(3.0);

                        let (verdict, colour) = match code {
                            Some(0) => ("exit 0, succeeded".to_string(), theme::GOOD),
                            Some(code) => (format!("exit {}, failed", code), theme::CRIT),
                            None => ("still running".to_string(), theme::WARN),
                        };
                        ui.label(RichText::new(verdict).size(11.0).color(colour));

                        if output.is_empty() {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "It printed nothing. With the console shown its \
                                     output goes to that window instead.",
                                )
                                .size(11.0)
                                .color(theme::MUTED),
                            );
                        } else {
                            ui.add_space(5.0);
                            // Whatever room is genuinely left, rather than a
                            // fixed number that can be larger than it. The
                            // action strip below is already reserved, so this
                            // can never grow over it.
                            let room = (ui.available_height() - 18.0).max(56.0);
                            egui::Frame::default()
                                .fill(theme::SURFACE_2)
                                .stroke(Stroke::new(1.0, theme::LINE))
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(9, 7))
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical().max_height(room).show(
                                        ui,
                                        |ui| {
                                            ui.label(
                                                RichText::new(output)
                                                    .size(11.0)
                                                    .monospace()
                                                    .color(theme::FG_2),
                                            );
                                        },
                                    );
                                });
                        }
                    }
                }
                _ => {}
            }
        } else {
            ui.add_space(10.0);
            ui.label(
                RichText::new("Nothing is bound to this key. Pick an action type above.")
                    .size(12.0)
                    .color(theme::MUTED),
            );
        }

    }
}

fn main() -> eframe::Result<()> {
    // One G Deck at a time. Clicking a pinned taskbar button starts a new
    // process, so without this every click leaves another keyboard reader
    // running and one G key press fires the action once per copy.
    let _claim = match single_instance::acquire() {
        Some(claim) => claim,
        None => {
            tray::wake_existing();
            return Ok(());
        }
    };

    // Windows reads an executable's icon from its resource section, which
    // build.rs fills, and that is what Explorer and a pinned button show.
    // This one is the window's own, for the title bar and Alt Tab.
    const ICON: u32 = 64;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 700.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title("G Deck")
            .with_icon(egui::IconData {
                rgba: iconart::rgba(ICON),
                width: ICON,
                height: ICON,
            }),
        ..Default::default()
    };

    eframe::run_native("G Deck", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod grid_tests {
    use super::*;

    /// Tiles 60 by 40 with a 10 gap, so the pitch is 70 by 50.
    fn grid() -> Grid {
        Grid {
            origin: egui::Pos2::new(100.0, 50.0),
            tile: Vec2::new(60.0, 40.0),
            gap: 10.0,
        }
    }

    fn at(x: f32, y: f32) -> Option<GKey> {
        grid().key_at(egui::Pos2::new(x, y))
    }

    #[test]
    fn each_corner_tile_resolves() {
        assert_eq!(at(100.0, 50.0), GKey::from_index(0), "top left");
        assert_eq!(at(240.0, 50.0), GKey::from_index(2), "top right");
        assert_eq!(at(100.0, 300.0), GKey::from_index(15), "bottom left");
        assert_eq!(at(299.0, 339.0), GKey::from_index(17), "bottom right");
    }

    #[test]
    fn the_middle_of_the_grid_resolves() {
        // Second column, third row is G8, counting three to a row.
        assert_eq!(at(200.0, 160.0), GKey::from_index(7));
    }

    #[test]
    fn a_gap_is_not_a_drop_target() {
        // Dropping between two tiles should do nothing rather than pick
        // whichever neighbour the arithmetic happens to round to.
        assert_eq!(at(165.0, 60.0), None, "between column 0 and 1");
        assert_eq!(at(110.0, 95.0), None, "between row 0 and 1");
    }

    #[test]
    fn outside_the_grid_is_nothing() {
        assert_eq!(at(99.0, 60.0), None, "left of it");
        assert_eq!(at(110.0, 49.0), None, "above it");
        assert_eq!(at(310.0, 60.0), None, "right of the third column");
        assert_eq!(at(110.0, 350.0), None, "below the sixth row");
    }
}
