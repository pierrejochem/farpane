//! The egui window: home screen, session canvas, top bar.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{
    Align, Color32, ColorImage, Context, CursorIcon, EventFilter, Frame, Key, Layout, Margin,
    RichText, Sense, TextEdit, TextureHandle, TextureOptions, UserData, ViewportCommand, vec2,
};
use farpane_grab::GrabState;
use farpane_ui::{Accent, ThemePreference, Tokens, style, widgets};
use rfb_codec::Cursor;
use rfb_proto::types::Rect;
use rfb_security::tls::{CertProblem, Fingerprint, format_fingerprint};

use crate::{
    appearance,
    capture::{CapturePref, ChordTracker, Grabber, Situation, should_capture},
    clipboard,
    core::{Command, Core, UiEvent},
    input::{Canvas, KeyTranslator, PointerTranslator},
    settings::{self, Store},
    stats::{self, Snapshot},
};

/// What the command line asked for.
#[derive(Debug, Default, Clone)]
pub struct Startup {
    /// Prefill the address, and connect right away.
    pub addr: Option<String>,
    pub password: Option<Vec<u8>>,
    /// Save a screenshot of the window to this file and quit (for testing).
    pub ui_screenshot: Option<PathBuf>,
    /// Start the window in full screen.
    pub fullscreen: bool,
    /// Keep the full-screen toolbar open and pinned (for screenshots).
    pub pin_toolbar: bool,
    /// Start with the statistics overlay shown.
    pub show_stats: bool,
    /// Where settings are kept. `None` means nothing is read or written
    /// (tests); the binary passes [`settings::default_path`].
    pub settings_path: Option<PathBuf>,
}

/// Samples kept for the bandwidth graph: 60 s at four readings a second.
const HISTORY_LEN: usize = 240;

/// The window size must stay the same this long (egui seconds) before the
/// remote desktop is asked to follow it.
const RESIZE_SETTLE: f64 = 0.4;

/// Smallest remote size we ask for.
const MIN_REMOTE_SIZE: u16 = 64;

/// Below this window width, in points, the session controls fold into a menu.
const NARROW_BAR: f32 = 1080.0;

/// Cursors wider or taller than this are not drawn (the server still thinks
/// we show them, so we hide ours rather than the local one).
const MAX_CURSOR_PX: u16 = 512;

/// A server whose TLS certificate is not trusted yet, waiting for the user.
#[derive(Debug, Clone)]
struct TrustPrompt {
    addr: String,
    host: String,
    fingerprint: Fingerprint,
    problem: CertProblem,
}

/// The full-screen toolbar: a slim tab at the top edge that opens a pill of
/// controls. Times are egui's clock, in seconds, so tests can drive them.
#[derive(Debug, Default)]
struct Toolbar {
    open: bool,
    pinned: bool,
    /// When the pointer last touched the tab, the top edge or the pill.
    last_active: f64,
    /// Where the pill was drawn last frame, to know if the pointer is on it.
    rect: Option<egui::Rect>,
}

/// The pill closes this long after the pointer leaves it, unless pinned.
const TOOLBAR_HIDE_AFTER: f64 = 2.0;
/// The pointer counts as "at the top edge" within this many points.
const TOP_EDGE: f32 = 4.0;

/// The server's cursor, drawn by us at the pointer.
enum RemoteCursor {
    /// An empty shape: show no cursor at all.
    Hidden,
    Shape {
        texture: TextureHandle,
        hotspot: (u16, u16),
        size: (u16, u16),
    },
}

#[derive(Debug)]
enum Phase {
    Home,
    Connecting,
    Connected { name: String },
}

/// The viewer window.
#[allow(clippy::struct_excessive_bools)] // independent UI switches
pub struct ViewerApp {
    core: Core,
    phase: Phase,
    addr: String,
    password: String,
    /// For servers that ask for a username as well (`VeNCrypt` `X509Plain`).
    username: String,
    trust_prompt: Option<TrustPrompt>,
    error: Option<String>,
    theme: ThemePreference,
    accent: Accent,
    interface_size: u32,
    reduce_transparency: bool,
    applied: Option<Tokens>,
    applied_zoom: Option<u32>,
    store: Store,
    settings_open: bool,
    /// The address of the connection attempt in progress.
    last_addr: String,
    one_to_one: bool,
    /// Copy text between the local and the remote clipboard.
    clipboard_sync: bool,
    toolbar: Toolbar,
    /// The keyboard grabber, created on the first frame of a real window (or
    /// injected by tests).
    grab: Option<Box<dyn Grabber>>,
    grab_tried: bool,
    /// Why capture cannot work here, if it cannot.
    grab_unavailable: Option<String>,
    grab_state: GrabState,
    capture_pref: CapturePref,
    /// The user let go of the keyboard with the release chord.
    released_by_user: bool,
    chord: ChordTracker,
    was_capturing: bool,
    /// Resize the remote desktop to the window.
    resize_remote: bool,
    /// The server announced a layout, so it understands resize requests.
    server_can_resize: bool,
    /// Protocol version and security of the current session.
    session_info: Option<(String, String)>,
    /// Whether the bar under the picture is shown.
    show_footer: bool,
    /// Ask for H.264, when Cisco's `OpenH264` library is installed.
    h264: bool,
    video: Option<Arc<farpane_h264::OpenH264>>,
    /// A download of the library in progress, and the last thing to tell about it.
    h264_installer: Option<farpane_h264::Installer>,
    h264_note: Option<String>,
    /// The server refused a request: stop asking for this connection.
    resize_refused: bool,
    /// A size we want to ask for, and when the window first had it.
    resize_wanted: Option<((u16, u16), f64)>,
    /// The last size we asked for (never asked twice in a row).
    last_requested: Option<(u16, u16)>,
    stats_visible: bool,
    stats: Option<Snapshot>,
    /// Received bytes per second, one value per reading, oldest first.
    rx_history: VecDeque<f32>,
    /// Whether the window was in full screen at the last frame.
    fullscreen: bool,
    /// A short message for the top bar, with the time it expires.
    notice: Option<(String, Instant)>,
    texture: Option<TextureHandle>,
    remote_size: (u16, u16),
    /// `None` until the server sends a shape: then the local arrow is shown.
    cursor: Option<RemoteCursor>,
    keys: KeyTranslator,
    pointer: PointerTranslator,
    last_remote_pos: (u16, u16),
    startup_password: Option<Vec<u8>>,
    ui_screenshot: Option<PathBuf>,
    started: Instant,
    connected_at: Option<Instant>,
    screenshot_requested: bool,
}

impl std::fmt::Debug for ViewerApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerApp")
            .field("phase", &self.phase)
            .field("remote_size", &self.remote_size)
            .finish_non_exhaustive()
    }
}

impl ViewerApp {
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        Self::with_context(&cc.egui_ctx, startup)
    }

    /// Build the app for an egui context (tests drive it without a window).
    #[must_use]
    pub fn with_context(ctx: &Context, startup: Startup) -> Self {
        let ctx = ctx.clone();
        let core = Core::spawn(move || ctx.request_repaint());
        let auto = startup.addr.clone();
        let (store, settings_warning) = Store::load(startup.settings_path.clone());
        let look = store.settings.appearance.clone();
        let input = store.settings.input.clone();
        let h264 = store.settings.video.h264;
        let mut app = Self {
            core,
            phase: Phase::Home,
            addr: startup.addr.clone().unwrap_or_default(),
            password: String::new(),
            username: String::new(),
            trust_prompt: None,
            error: None,
            theme: appearance::theme_from(&look.theme),
            accent: appearance::accent_from(&look.accent),
            interface_size: appearance::size_from(look.interface_size),
            reduce_transparency: look.reduce_transparency,
            applied: None,
            applied_zoom: None,
            store,
            settings_open: false,
            last_addr: String::new(),
            one_to_one: false,
            clipboard_sync: true,
            toolbar: Toolbar {
                open: startup.pin_toolbar,
                pinned: startup.pin_toolbar,
                ..Toolbar::default()
            },
            grab: None,
            grab_tried: false,
            grab_unavailable: None,
            grab_state: GrabState::Released,
            capture_pref: CapturePref::from_key(&input.capture),
            released_by_user: false,
            chord: ChordTracker::default(),
            was_capturing: false,
            resize_remote: input.resize_remote,
            server_can_resize: false,
            session_info: None,
            show_footer: look.show_footer,
            h264,
            video: None,
            h264_installer: None,
            h264_note: None,
            resize_refused: false,
            resize_wanted: None,
            last_requested: None,
            stats_visible: startup.show_stats,
            stats: None,
            rx_history: VecDeque::new(),
            fullscreen: startup.fullscreen,
            notice: None,
            texture: None,
            remote_size: (0, 0),
            cursor: None,
            keys: KeyTranslator::default(),
            pointer: PointerTranslator::default(),
            last_remote_pos: (0, 0),
            startup_password: startup.password,
            ui_screenshot: startup.ui_screenshot,
            started: Instant::now(),
            connected_at: None,
            screenshot_requested: false,
        };
        if let Some(warning) = settings_warning {
            app.say(warning);
        }
        app.refresh_video();
        if auto.is_some() {
            app.connect_with(app.startup_password.clone());
        }
        app
    }

    /// True once the server has answered `ServerInit`.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(self.phase, Phase::Connected { .. })
    }

    /// Size of the remote screen as last announced.
    #[must_use]
    pub fn remote_size(&self) -> (u16, u16) {
        self.remote_size
    }

    /// Show or hide the statistics overlay.
    pub fn set_stats_visible(&mut self, visible: bool) {
        self.stats_visible = visible;
    }

    /// Whether the statistics overlay is shown.
    #[must_use]
    pub fn stats_visible(&self) -> bool {
        self.stats_visible
    }

    /// The latest statistics reading, once the connection has produced one.
    #[must_use]
    pub fn stats(&self) -> Option<&Snapshot> {
        self.stats.as_ref()
    }

    /// Readings in the bandwidth graph.
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.rx_history.len()
    }

    /// Fill in the address field, as if typed.
    pub fn set_address(&mut self, addr: &str) {
        addr.clone_into(&mut self.addr);
    }

    /// Connect to the address in the field, as if the button was clicked.
    pub fn start_connect(&mut self) {
        self.connect();
    }

    /// Fill in the credential fields, as if typed.
    pub fn set_credentials(&mut self, username: &str, password: &str) {
        username.clone_into(&mut self.username);
        password.clone_into(&mut self.password);
    }

    /// Whether the remote desktop follows the window size.
    #[must_use]
    pub fn resize_remote(&self) -> bool {
        self.resize_remote
    }

    /// Turn "resize the remote desktop to the window" on or off.
    pub fn set_resize_remote(&mut self, on: bool) {
        self.resize_remote = on;
        self.resize_wanted = None;
        // Asking again after the user turned it on is allowed even after a refusal.
        if on {
            self.resize_refused = false;
            self.last_requested = None;
        }
    }

    /// Whether the bar under the picture is shown.
    #[must_use]
    pub fn show_footer(&self) -> bool {
        self.show_footer
    }

    pub fn set_show_footer(&mut self, on: bool) {
        self.show_footer = on;
    }

    /// Whether the server has shown that it supports resizing.
    #[must_use]
    pub fn server_can_resize(&self) -> bool {
        self.server_can_resize
    }

    /// Tooltip for the resize toggle.
    fn resize_hint(&self) -> &'static str {
        if self.resize_refused {
            "The server refused to resize. Switch off and on to try again."
        } else if self.server_can_resize {
            "Make the remote desktop the same size as this window"
        } else {
            "The server does not say it can resize its desktop"
        }
    }

    fn reset_resize(&mut self) {
        self.server_can_resize = false;
        self.resize_refused = false;
        self.resize_wanted = None;
        self.last_requested = None;
    }

    /// Ask the server for the size of `canvas` (in physical pixels) once the
    /// window has kept that size for a moment. Never asks twice for the same
    /// size, so a server that picks another size cannot cause a loop.
    fn follow_window_size(&mut self, ctx: &Context, canvas: egui::Rect) {
        if !(self.resize_remote && self.server_can_resize && !self.resize_refused) {
            self.resize_wanted = None;
            return;
        }
        let px = |points: f32| {
            let v = (points * ctx.pixels_per_point()).round();
            // Clamped to the protocol's range first, so the conversion is exact.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let v = v.clamp(
                f32::from(MIN_REMOTE_SIZE),
                f32::from(rfb_proto::consts::limits::MAX_FB_DIMENSION),
            ) as u16;
            v
        };
        let desired = (px(canvas.width()), px(canvas.height()));
        if desired == self.remote_size || Some(desired) == self.last_requested {
            self.resize_wanted = None;
            return;
        }
        let now = ctx.input(|i| i.time);
        match self.resize_wanted {
            Some((size, since)) if size == desired => {
                if now - since >= RESIZE_SETTLE {
                    self.core.send(Command::Resize {
                        width: desired.0,
                        height: desired.1,
                    });
                    self.last_requested = Some(desired);
                    self.resize_wanted = None;
                } else {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            }
            _ => {
                self.resize_wanted = Some((desired, now));
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    /// Turn clipboard sharing on or off.
    pub fn set_clipboard_sync(&mut self, on: bool) {
        self.clipboard_sync = on;
    }

    /// The message currently shown in the top bar, if any.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(_, until)| Instant::now() < *until)
            .map(|(message, _)| message.as_str())
    }

    /// Size of the remote cursor shape: `None` before the server sent one,
    /// `Some((0, 0))` when it asked to hide the cursor.
    #[must_use]
    pub fn remote_cursor_size(&self) -> Option<(u16, u16)> {
        match self.cursor.as_ref()? {
            RemoteCursor::Hidden => Some((0, 0)),
            RemoteCursor::Shape { size, .. } => Some(*size),
        }
    }

    /// Draw one frame: handle connection events, draw, forward input.
    pub fn ui(&mut self, ctx: &Context) {
        let tokens = self.apply_theme(ctx);
        self.fullscreen = ctx
            .input(|i| i.viewport().fullscreen)
            .unwrap_or(self.fullscreen);
        // F11 belongs to the viewer, not the remote machine.
        if ctx.input(|i| i.key_pressed(Key::F11)) {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
        // Ctrl+Alt+Shift+S toggles the statistics, locally.
        if ctx.input(is_stats_hotkey) {
            self.stats_visible = !self.stats_visible;
        }
        self.drain_events(ctx);
        let title = match &self.phase {
            Phase::Connected { name } => format!("{name} — Farpane Viewer"),
            _ => "Farpane Viewer".to_owned(),
        };
        ctx.send_viewport_cmd(ViewportCommand::Title(title));
        let immersive = self.fullscreen && self.is_connected();
        if !immersive {
            self.top_bar(ctx, &tokens);
        }
        if !immersive && self.show_footer && self.is_connected() {
            self.session_footer(ctx, &tokens);
        }
        match self.phase {
            Phase::Connected { .. } => self.session(ctx),
            Phase::Home | Phase::Connecting => self.home(ctx, &tokens),
        }
        if immersive {
            self.toolbar_overlay(ctx, &tokens);
        }
        if self.stats_visible && self.is_connected() {
            self.stats_overlay(ctx, &tokens);
        }
        self.sync_grab(ctx);
        self.trust_window(ctx, &tokens);
        self.settings_window(ctx);
        self.toast(ctx, &tokens);
        self.sync_appearance();
        self.screenshot_driver(ctx);
    }

    /// Whether the window is in full screen.
    #[must_use]
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// Whether the full-screen toolbar pill is open (or sliding open).
    #[must_use]
    pub fn toolbar_open(&self) -> bool {
        self.toolbar.open
    }

    /// Pin the toolbar open, or let it auto-hide again.
    pub fn set_toolbar_pinned(&mut self, pinned: bool) {
        self.toolbar.pinned = pinned;
    }

    fn set_fullscreen(&mut self, ctx: &Context, on: bool) {
        self.fullscreen = on;
        self.toolbar.open = false;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(on));
    }

    /// Send Ctrl+Alt+Delete: the local OS would otherwise swallow it.
    pub fn send_ctrl_alt_del(&self) {
        use crate::input::keysym::{ALT_L, CONTROL_L, DELETE};
        for (keysym, down) in [
            (CONTROL_L, true),
            (ALT_L, true),
            (DELETE, true),
            (DELETE, false),
            (ALT_L, false),
            (CONTROL_L, false),
        ] {
            self.core.send(Command::Key { keysym, down });
        }
    }

    fn connect(&mut self) {
        let password = (!self.password.is_empty()).then(|| self.password.clone().into_bytes());
        self.connect_with(password);
    }

    fn connect_with(&mut self, password: Option<Vec<u8>>) {
        self.last_addr = self.addr.trim().to_owned();
        self.trust_prompt = None;
        let username = (!self.username.is_empty()).then(|| self.username.clone().into_bytes());
        self.core.send(Command::SetVideo(crate::core::Video(
            self.video
                .clone()
                .map(|v| v as Arc<dyn rfb_codec::video::VideoCodec>),
        )));
        self.core.send(Command::Connect {
            addr: self.last_addr.clone(),
            password,
            username,
            pins: self.store.settings.pins_for(&self.last_addr),
        });
    }

    /// The certificate waiting for a decision: its fingerprint as shown to the
    /// user, and why it is not trusted yet.
    #[must_use]
    pub fn pending_trust(&self) -> Option<(String, CertProblem)> {
        self.trust_prompt
            .as_ref()
            .map(|p| (format_fingerprint(&p.fingerprint), p.problem.clone()))
    }

    /// The user trusts the waiting certificate: remember it (replacing any
    /// older one for that server) and connect again.
    pub fn trust_pending(&mut self) {
        if let Some(prompt) = self.trust_prompt.take() {
            self.store.settings.pin(&prompt.addr, &prompt.fingerprint);
            self.persist();
            self.addr = prompt.addr;
            let password = (!self.password.is_empty()).then(|| self.password.clone().into_bytes());
            self.connect_with(password);
        }
    }

    /// The user does not trust the waiting certificate.
    pub fn reject_pending(&mut self) {
        self.trust_prompt = None;
    }

    fn tokens(&self, ctx: &Context) -> Tokens {
        let os_dark = ctx.input(|i| i.raw.system_theme) != Some(egui::Theme::Light);
        let tokens = Tokens::new(self.theme.resolve(os_dark), self.accent);
        if self.reduce_transparency {
            tokens.reduce_transparency()
        } else {
            tokens
        }
    }

    /// Apply the theme to egui whenever the tokens change.
    fn apply_theme(&mut self, ctx: &Context) -> Tokens {
        let tokens = self.tokens(ctx);
        if self.applied_zoom != Some(self.interface_size) {
            #[allow(clippy::cast_precision_loss)] // 90..=115
            ctx.set_zoom_factor(self.interface_size as f32 / 100.0);
            self.applied_zoom = Some(self.interface_size);
        }
        if self.applied != Some(tokens) {
            style::apply(ctx, &tokens);
            self.applied = Some(tokens);
        }
        style::paint_backdrop(ctx, &tokens);
        tokens
    }

    /// Load Cisco's `OpenH264` library when H.264 is switched on and it is
    /// installed, and say what is missing when it is not.
    fn refresh_video(&mut self) {
        self.video = None;
        self.h264_note = None;
        if !self.h264 {
            return;
        }
        let Some(dir) = self.store.dir().map(|d| farpane_h264::library_dir(&d)) else {
            self.h264_note = Some("There is no settings folder to keep the library in.".into());
            return;
        };
        match farpane_h264::OpenH264::open(&dir) {
            Ok(Some(codec)) => self.video = Some(Arc::new(codec)),
            Ok(None) => {
                self.h264_note = Some("OpenH264 is not installed yet.".into());
            }
            Err(e) => self.h264_note = Some(e.to_string()),
        }
    }

    /// The H.264 setting: a switch, the state of the library, and the way to
    /// download it.
    fn h264_row(&mut self, ui: &mut egui::Ui) {
        if let Some(done) = self
            .h264_installer
            .as_ref()
            .and_then(farpane_h264::Installer::finished)
        {
            self.h264_installer = None;
            self.h264_note = Some(match done {
                Ok(_) => "Installed.".to_owned(),
                Err(why) => format!("Could not install: {why}"),
            });
            self.refresh_video();
        }
        ui.vertical(|ui| {
            if ui
                .checkbox(&mut self.h264, "Ask servers for H.264")
                .changed()
            {
                self.refresh_video();
            }
            if self.h264_installer.is_some() {
                ui.label("Downloading OpenH264 from Cisco…");
            } else if self.h264 && self.video.is_some() {
                ui.label("OpenH264 is installed. It applies to the next connection.");
            } else if let Some(note) = &self.h264_note {
                ui.label(note);
            }
            if self.h264
                && self.video.is_none()
                && self.h264_installer.is_none()
                && farpane_h264::platform_file().is_some()
                && ui
                    .button("Download OpenH264 from Cisco")
                    .on_hover_text(farpane_h264::NOTICE)
                    .clicked()
                && let Some(dir) = self.store.dir().map(|d| farpane_h264::library_dir(&d))
            {
                self.h264_installer = Some(farpane_h264::Installer::start(dir));
            }
        });
    }

    fn drain_events(&mut self, ctx: &Context) {
        while let Some(event) = self.core.try_recv() {
            match event {
                UiEvent::Connecting => {
                    self.phase = Phase::Connecting;
                    self.error = None;
                }
                UiEvent::Connected {
                    name,
                    width,
                    height,
                    protocol,
                    security,
                } => {
                    self.session_info = Some((protocol, security));
                    self.reset_resize();
                    self.remember_connection(&name);
                    self.phase = Phase::Connected { name };
                    self.new_texture(ctx, width, height);
                    self.connected_at = Some(Instant::now());
                }
                UiEvent::Resized { width, height } => self.new_texture(ctx, width, height),
                UiEvent::Frame { rect, rgba } => self.upload(rect, &rgba),
                UiEvent::Cursor(cursor) => self.set_cursor(ctx, &cursor),
                UiEvent::CutText(text) => {
                    if self.clipboard_sync {
                        let text = clipboard::from_remote(&text);
                        if !text.is_empty() {
                            ctx.copy_text(text);
                        }
                    }
                }
                UiEvent::ClipboardText(text) => {
                    if self.clipboard_sync {
                        let text = clipboard::from_remote_utf8(&text);
                        if !text.is_empty() {
                            ctx.copy_text(text);
                        }
                    }
                }
                UiEvent::Notice(message) => self.say(message),
                UiEvent::Untrusted {
                    addr,
                    host,
                    fingerprint,
                    problem,
                } => {
                    self.phase = Phase::Home;
                    self.trust_prompt = Some(TrustPrompt {
                        addr,
                        host,
                        fingerprint,
                        problem,
                    });
                }
                UiEvent::Layout { reason, status, .. } => {
                    self.server_can_resize = true;
                    // Reason 1 answers our request; status 4 means "forwarded,
                    // may still happen", anything else non-zero is a refusal.
                    if reason == 1 && status != 0 && status != 4 {
                        self.resize_refused = true;
                        self.say(format!(
                            "The server did not resize the screen ({}).",
                            resize_refusal(status)
                        ));
                    }
                }
                UiEvent::Stats(snapshot) => {
                    if self.rx_history.len() == HISTORY_LEN {
                        self.rx_history.pop_front();
                    }
                    #[allow(clippy::cast_possible_truncation)] // a graph needs no more precision
                    self.rx_history.push_back(snapshot.rx_per_sec as f32);
                    self.stats = Some(snapshot);
                }
                UiEvent::Bell => {}
                UiEvent::Disconnected(reason) => {
                    self.phase = Phase::Home;
                    if self.fullscreen {
                        self.set_fullscreen(ctx, false);
                    }
                    self.error = reason;
                    self.texture = None;
                    self.cursor = None;
                    self.reset_resize();
                    self.stats = None;
                    self.rx_history.clear();
                    self.connected_at = None;
                    self.release_input();
                }
            }
        }
    }

    fn set_cursor(&mut self, ctx: &Context, cursor: &Cursor) {
        let (w, h) = (cursor.width, cursor.height);
        self.cursor = Some(
            if w == 0 || h == 0 || w > MAX_CURSOR_PX || h > MAX_CURSOR_PX {
                RemoteCursor::Hidden
            } else {
                let image = ColorImage::from_rgba_unmultiplied(
                    [usize::from(w), usize::from(h)],
                    &cursor.rgba,
                );
                RemoteCursor::Shape {
                    texture: ctx.load_texture("remote-cursor", image, TextureOptions::NEAREST),
                    hotspot: cursor.hotspot,
                    size: (w, h),
                }
            },
        );
    }

    /// Save the settings; a failure is shown once, and "no config directory"
    /// (tests, odd environments) is not an error worth showing.
    fn persist(&mut self) {
        match self.store.save() {
            Ok(()) | Err(settings::SaveError::NoPath) => {}
            Err(e) => self.say(format!("Could not save settings: {e}")),
        }
    }

    /// Add or refresh the address book entry for the connection that just
    /// succeeded.
    fn remember_connection(&mut self, name: &str) {
        let addr = self.last_addr.clone();
        self.store.settings.remember(name, &addr, unix_now());
        self.persist();
    }

    /// Write the appearance fields back to the settings if they changed.
    fn sync_appearance(&mut self) {
        let now = settings::Appearance {
            theme: appearance::theme_key(self.theme).to_owned(),
            accent: appearance::accent_key(self.accent).to_owned(),
            interface_size: self.interface_size,
            reduce_transparency: self.reduce_transparency,
            show_footer: self.show_footer,
        };
        let input = settings::Input {
            capture: self.capture_pref.key().to_owned(),
            resize_remote: self.resize_remote,
        };
        let video = settings::Video { h264: self.h264 };
        if now != self.store.settings.appearance
            || input != self.store.settings.input
            || video != self.store.settings.video
        {
            self.store.settings.appearance = now;
            self.store.settings.input = input;
            self.store.settings.video = video;
            self.persist();
        }
    }

    /// The saved connections as `(name, address)`, most recent first.
    #[must_use]
    pub fn saved_connections(&self) -> Vec<(String, String)> {
        self.store
            .settings
            .recent()
            .into_iter()
            .map(|c| (c.name.clone(), c.addr.clone()))
            .collect()
    }

    /// Connect to the `index`-th saved connection (as listed by
    /// [`ViewerApp::saved_connections`]).
    pub fn connect_saved(&mut self, index: usize) {
        if let Some((_, addr)) = self.saved_connections().into_iter().nth(index) {
            self.addr = addr;
            self.connect();
        }
    }

    /// Remove a saved connection.
    pub fn forget_saved(&mut self, addr: &str) {
        if self.store.settings.forget(addr) {
            self.persist();
        }
    }

    /// The chosen theme.
    #[must_use]
    pub fn theme(&self) -> ThemePreference {
        self.theme
    }

    /// Change the theme (saved with the next frame).
    pub fn set_theme(&mut self, theme: ThemePreference) {
        self.theme = theme;
    }

    fn say(&mut self, message: String) {
        self.say_for(message, Duration::from_secs(5));
    }

    fn say_for(&mut self, message: String, how_long: Duration) {
        self.notice = Some((message, Instant::now() + how_long));
    }

    /// Use `grabber` for keyboard capture (the real one is created from the
    /// window on the first frame; tests inject a fake).
    pub fn set_grabber(&mut self, grabber: Box<dyn Grabber>) {
        self.grab = Some(grabber);
        self.grab_tried = true;
    }

    /// The user's capture setting.
    #[must_use]
    pub fn capture_pref(&self) -> CapturePref {
        self.capture_pref
    }

    /// Change the capture setting (saved with the next frame).
    pub fn set_capture_pref(&mut self, pref: CapturePref) {
        self.capture_pref = pref;
        self.released_by_user = false;
    }

    /// Whether the keyboard is being captured, as far as the windowing system
    /// has confirmed.
    #[must_use]
    pub fn grab_state(&self) -> &GrabState {
        &self.grab_state
    }

    /// Whether the user gave the keyboard back with the release chord and has
    /// not taken it again.
    #[must_use]
    pub fn released_by_user(&self) -> bool {
        self.released_by_user
    }

    /// Try to create the real grabber from the window (once).
    fn ensure_grabber(&mut self, frame: &eframe::Frame) {
        if self.grab.is_some() || self.grab_tried {
            return;
        }
        self.grab_tried = true;
        match farpane_grab::KeyboardGrab::from_window(frame) {
            Ok(grab) => self.grab = Some(Box::new(grab)),
            Err(e) => self.grab_unavailable = Some(e.to_string()),
        }
    }

    /// Decide whether the keyboard should be captured now, tell the grabber,
    /// and handle the release chord and the hints.
    fn sync_grab(&mut self, ctx: &Context) {
        let (focused, mods, other_key) = ctx.input(|i| {
            (
                i.focused,
                i.modifiers,
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
            )
        });
        if self.chord.update(mods, other_key) && self.was_capturing {
            self.released_by_user = true;
            self.say("Keyboard released. Click the picture to capture it again.".to_owned());
        }
        let wanted = should_capture(Situation {
            connected: self.is_connected(),
            window_focused: focused,
            fullscreen: self.fullscreen,
            pref: self.capture_pref,
            released_by_user: self.released_by_user,
        });
        if let Some(grab) = &mut self.grab {
            grab.set_wanted(wanted);
            let state = grab.poll();
            if let (true, GrabState::Failed(why)) = (wanted, &state)
                && self.grab_state != state
            {
                let message = format!("Could not capture the keyboard: {why}");
                self.say(message);
            }
            self.grab_state = state;
        } else if wanted
            && !self.was_capturing
            && let Some(why) = self.grab_unavailable.clone()
        {
            self.say(format!("Keyboard capture is not available: {why}"));
        }
        if wanted
            && !self.was_capturing
            && self.grab.is_some()
            && matches!(self.grab_state, GrabState::Requested | GrabState::Active)
        {
            self.say_for(
                "Keyboard captured. Press and release Ctrl+Alt+Shift to give it back.".to_owned(),
                Duration::from_secs(6),
            );
        }
        self.was_capturing = wanted;
    }

    /// Whether capture is wanted at the moment, for the controls.
    fn capture_wanted(&self) -> bool {
        self.was_capturing
    }

    /// One short phrase for the overlay.
    fn capture_summary(&self) -> String {
        match (&self.grab, &self.grab_state) {
            (None, _) => "capture unavailable".to_owned(),
            (Some(g), GrabState::Active) => format!("captured ({})", g.backend()),
            (Some(_), GrabState::Requested) => "capture requested, not confirmed".to_owned(),
            (Some(_), GrabState::Failed(_)) => "capture failed".to_owned(),
            (Some(_), GrabState::Released) => "not captured".to_owned(),
        }
    }

    /// A short status for the controls: what the keyboard is doing.
    fn capture_status(&self) -> String {
        match (&self.grab, &self.grab_state) {
            (None, _) => match &self.grab_unavailable {
                Some(why) => format!("Not available: {why}"),
                None => "Not available".to_owned(),
            },
            (Some(_), GrabState::Active) => "System keys go to the remote machine".to_owned(),
            (Some(_), GrabState::Requested) => "Waiting for the system to confirm".to_owned(),
            (Some(_), GrabState::Failed(why)) => format!("Failed: {why}"),
            (Some(g), GrabState::Released) => {
                format!("Off ({} backend); system shortcuts stay local", g.backend())
            }
        }
    }

    /// Send the local clipboard text to the remote machine (on paste).
    fn send_clipboard(&mut self, text: &str) {
        self.core.send(Command::Clipboard(text.to_owned()));
    }

    fn new_texture(&mut self, ctx: &Context, width: u16, height: u16) {
        self.remote_size = (width, height);
        let size = [usize::from(width), usize::from(height)];
        self.texture = Some(ctx.load_texture(
            "remote-screen",
            ColorImage::filled(size, Color32::BLACK),
            TextureOptions::LINEAR,
        ));
    }

    /// Upload only the changed rectangle.
    fn upload(&mut self, rect: Rect, rgba: &[u8]) {
        if let Some(texture) = &mut self.texture {
            let size = [usize::from(rect.width), usize::from(rect.height)];
            let image = ColorImage::from_rgba_unmultiplied(size, rgba);
            texture.set_partial(
                [usize::from(rect.x), usize::from(rect.y)],
                image,
                TextureOptions::LINEAR,
            );
        }
    }

    /// Let go of every key and button the remote believes is held.
    fn release_input(&mut self) {
        for k in self.keys.release_all() {
            self.core.send(Command::Key {
                keysym: k.keysym,
                down: k.down,
            });
        }
        if let Some(p) = self.pointer.release_all(self.last_remote_pos) {
            self.core.send(Command::Pointer {
                buttons: p.buttons,
                x: p.x,
                y: p.y,
            });
        }
    }

    #[allow(clippy::too_many_lines)] // one bar, easier to read whole
    fn top_bar(&mut self, ctx: &Context, tokens: &Tokens) {
        egui::TopBottomPanel::top("top-bar")
            .show_separator_line(false)
            .frame(
                Frame::NONE
                    .fill(style::page_fill(tokens))
                    .inner_margin(Margin {
                        left: 14,
                        right: 14,
                        top: 12,
                        bottom: 2,
                    }),
            )
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(tokens.card.into())
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
                    .corner_radius(style::radius(tokens.radius * 0.7))
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        self.top_bar_contents(ctx, ui, tokens);
                    });
            });
    }

    #[allow(clippy::too_many_lines)] // one bar, easier to read whole
    fn top_bar_contents(&mut self, ctx: &Context, ui: &mut egui::Ui, tokens: &Tokens) {
        {
            {
                ui.horizontal(|ui| {
                    // The home side bar already carries the brand.
                    if matches!(self.phase, Phase::Connected { .. }) {
                        widgets::brand_mark(ui, tokens, 26.0);
                        ui.label(
                            RichText::new("Farpane")
                                .family(style::semibold(ui.ctx()))
                                .size(16.0),
                        );
                        ui.add_space(8.0);
                    }
                    let (text, colour) = match &self.phase {
                        Phase::Home => ("Not connected", tokens.muted),
                        Phase::Connecting => ("Connecting…", tokens.amber),
                        Phase::Connected { .. } => ("Connected", tokens.green),
                    };
                    // A drawn dot: egui's default font has no "●" glyph.
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, colour);
                    // The controls fill the bar in a narrow window.
                    let roomy = ctx.screen_rect().width() > 1280.0;
                    if roomy || !matches!(self.phase, Phase::Connected { .. }) {
                        ui.label(RichText::new(text).color(colour));
                    }
                    if let (true, Phase::Connected { name }, Some(since)) =
                        (roomy, &self.phase, self.connected_at)
                    {
                        ui.label(RichText::new(name).strong());
                        let secs = since.elapsed().as_secs();
                        widgets::badge(
                            ui,
                            &format!(
                                "Live · {}:{:02}:{:02}",
                                secs / 3600,
                                secs / 60 % 60,
                                secs % 60
                            ),
                            tokens.green.into(),
                        );
                        if let Some((_, security)) = &self.session_info {
                            let encrypted = (security.starts_with("VeNCrypt")
                                || security.starts_with("RSA-AES"))
                                && !security.contains("unencrypted");
                            if encrypted {
                                widgets::badge(ui, "Encrypted", tokens.green.into());
                            } else {
                                widgets::badge(ui, "Not encrypted", tokens.amber.into());
                            }
                        }
                        ctx.request_repaint_after(Duration::from_secs(1));
                    }
                    // In a narrow window the less used controls move into a
                    // menu, so the bar never runs out of room.
                    let narrow = ctx.screen_rect().width() < NARROW_BAR;
                    let connected = matches!(self.phase, Phase::Connected { .. });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("theme")
                            .selected_text(appearance::theme_label(self.theme))
                            .show_ui(ui, |ui| {
                                for t in appearance::THEMES {
                                    ui.selectable_value(
                                        &mut self.theme,
                                        t,
                                        appearance::theme_label(t),
                                    );
                                }
                            });
                        if !narrow {
                            ui.label(RichText::new("Theme").color(tokens.muted));
                        }
                        if ui.button("Settings").clicked() {
                            self.settings_open = !self.settings_open;
                        }
                        if !connected {
                            return;
                        }
                        if narrow {
                            if ui.button("Disconnect").clicked() {
                                self.core.send(Command::Disconnect);
                            }
                            ui.menu_button("More", |ui| {
                                self.session_controls(ctx, ui, true);
                            });
                            return;
                        }
                        ui.add_space(8.0);
                        ui.checkbox(&mut self.clipboard_sync, "Clipboard")
                            .on_hover_text("Share text between this computer and the remote one");
                        ui.add_space(12.0);
                        if ui.button("Disconnect").clicked() {
                            self.core.send(Command::Disconnect);
                        }
                        self.session_controls(ctx, ui, false);
                    });
                });
            }
        }
    }

    /// The session's buttons, in the bar (right to left) or in the "More" menu.
    fn session_controls(&mut self, ctx: &Context, ui: &mut egui::Ui, in_menu: bool) {
        if ui.button("Full screen").clicked() {
            self.set_fullscreen(ctx, true);
            if in_menu {
                ui.close();
            }
        }
        if ui.button("Ctrl+Alt+Del").clicked() {
            self.send_ctrl_alt_del();
            if in_menu {
                ui.close();
            }
        }
        let label = if self.one_to_one {
            "Fit to window"
        } else {
            "Actual size"
        };
        if ui.button(label).clicked() {
            self.one_to_one = !self.one_to_one;
            if in_menu {
                ui.close();
            }
        }
        ui.toggle_value(&mut self.stats_visible, "Stats")
            .on_hover_text("Ctrl+Alt+Shift+S");
        let mut resize = self.resize_remote;
        if ui
            .toggle_value(&mut resize, "Resize remote")
            .on_hover_text(self.resize_hint())
            .changed()
        {
            self.set_resize_remote(resize);
        }
        let mut capturing = self.capture_wanted();
        let status = self.capture_status();
        if ui
            .toggle_value(&mut capturing, "Capture keys")
            .on_hover_text(status)
            .changed()
        {
            self.set_capture_pref(if capturing {
                CapturePref::Always
            } else {
                CapturePref::Never
            });
        }
        // In the bar the clipboard has its own box and the footer is a
        // setting; in the menu both are here.
        if in_menu {
            ui.checkbox(&mut self.clipboard_sync, "Clipboard");
            ui.checkbox(&mut self.show_footer, "Footer");
        }
    }

    /// The bar under the picture: protocol, security, size, what the server
    /// showed it can do, and the pinned server key.
    fn session_footer(&self, ctx: &Context, tokens: &Tokens) {
        egui::TopBottomPanel::bottom("session-footer")
            .show_separator_line(false)
            .frame(
                Frame::NONE
                    .fill(style::page_fill(tokens))
                    .inner_margin(Margin {
                        left: 14,
                        right: 14,
                        top: 2,
                        bottom: 12,
                    }),
            )
            .show(ctx, |ui| {
                Self::card_frame(tokens, 16, 8)
                    .corner_radius(style::radius(14.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let text =
                            |t: String| RichText::new(t).monospace().color(tokens.muted).size(12.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 18.0;
                            if let Some((protocol, security)) = &self.session_info {
                                ui.label(text(protocol.clone()));
                                ui.label(text(security.clone()));
                            }
                            let (w, h) = self.remote_size;
                            let fit = if self.one_to_one { "1:1" } else { "fit" };
                            ui.label(text(format!("{w}×{h} → {fit}")));
                            let mut seen = Vec::new();
                            if self.server_can_resize {
                                seen.push("ExtendedDesktopSize");
                            }
                            if self.stats.as_ref().is_some_and(|s| s.rtt.is_some()) {
                                seen.push("Fence");
                            }
                            if self.stats.as_ref().is_some_and(|s| s.ext_clipboard) {
                                seen.push("ExtClipboard");
                            }
                            if !seen.is_empty() && ui.available_width() > 420.0 {
                                ui.label(text(seen.join(" · ")));
                            }
                            if let Some(key) = self.pinned_key_summary() {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(text(format!("Server key SHA-256 {key}")));
                                });
                            }
                        });
                    });
            });
    }

    /// The pinned certificate or key fingerprint of the current server, as
    /// `3F:9A:…:C4:12`.
    fn pinned_key_summary(&self) -> Option<String> {
        let pin = &self
            .store
            .settings
            .known_hosts
            .iter()
            .find(|k| k.host == self.addr.trim())?
            .sha256;
        let parts: Vec<&str> = pin.split(':').collect();
        if parts.len() < 5 {
            return Some(pin.clone());
        }
        Some(format!(
            "{}:{}:…:{}:{}",
            parts[0],
            parts[1],
            parts[parts.len() - 2],
            parts[parts.len() - 1]
        ))
    }

    /// The slim tab and the pill of controls, drawn over the picture.
    #[allow(clippy::too_many_lines)] // one layout, easier to read whole
    fn toolbar_overlay(&mut self, ctx: &Context, tokens: &Tokens) {
        // Over the remote picture the panels must be opaque, or the text is
        // unreadable: use the "reduce transparency" variant of the tokens.
        let tokens = &tokens.reduce_transparency();
        let now = ctx.input(|i| i.time);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let at_edge = pointer.is_some_and(|p| p.y <= TOP_EDGE);
        let on_pill = pointer
            .zip(self.toolbar.rect)
            .is_some_and(|(p, r)| r.expand(4.0).contains(p));
        if at_edge || on_pill {
            self.toolbar.open = true;
            self.toolbar.last_active = now;
        } else if self.toolbar.open
            && !self.toolbar.pinned
            && now - self.toolbar.last_active > TOOLBAR_HIDE_AFTER
        {
            self.toolbar.open = false;
        }
        if self.toolbar.open && !self.toolbar.pinned {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let slide =
            ctx.animate_bool_with_time(egui::Id::new("toolbar-slide"), self.toolbar.open, 0.15);

        if slide < 1.0 {
            let tab = egui::Area::new(egui::Id::new("toolbar-tab"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    let (rect, response) = ui.allocate_exact_size(vec2(72.0, 10.0), Sense::click());
                    let radius = egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 6,
                        se: 6,
                    };
                    ui.painter()
                        .rect_filled(rect, radius, Color32::from(tokens.card));
                    response
                });
            if tab.inner.clicked() {
                self.toolbar.open = true;
                self.toolbar.last_active = now;
            }
        }

        if slide > 0.0 {
            let name = match &self.phase {
                Phase::Connected { name } => name.clone(),
                _ => String::new(),
            };
            let (w, h) = self.remote_size;
            let pill = egui::Area::new(egui::Id::new("toolbar-pill"))
                .order(egui::Order::Foreground)
                .anchor(
                    egui::Align2::CENTER_TOP,
                    vec2(0.0, 8.0 - (1.0 - slide) * 64.0),
                )
                .show(ctx, |ui| {
                    Frame::NONE
                        .fill(tokens.card.into())
                        .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
                        .corner_radius(style::radius(tokens.radius))
                        .inner_margin(Margin::symmetric(16, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{name}  ·  {w}×{h}")).strong());
                                ui.separator();
                                if ui.button("Ctrl+Alt+Del").clicked() {
                                    self.send_ctrl_alt_del();
                                }
                                let size = if self.one_to_one {
                                    "Fit to window"
                                } else {
                                    "Actual size"
                                };
                                if ui.button(size).clicked() {
                                    self.one_to_one = !self.one_to_one;
                                }
                                ui.checkbox(&mut self.clipboard_sync, "Clipboard");
                                ui.toggle_value(&mut self.stats_visible, "Stats");
                                let mut resize = self.resize_remote;
                                if ui
                                    .toggle_value(&mut resize, "Resize remote")
                                    .on_hover_text(self.resize_hint())
                                    .changed()
                                {
                                    self.set_resize_remote(resize);
                                }
                                let mut capturing = self.capture_wanted();
                                let status = self.capture_status();
                                if ui
                                    .toggle_value(&mut capturing, "Capture keys")
                                    .on_hover_text(status)
                                    .changed()
                                {
                                    self.set_capture_pref(if capturing {
                                        CapturePref::Always
                                    } else {
                                        CapturePref::Never
                                    });
                                }
                                ui.toggle_value(&mut self.toolbar.pinned, "Pin");
                                ui.separator();
                                if ui.button("Exit full screen").clicked() {
                                    self.set_fullscreen(ctx, false);
                                }
                                if ui.button("Disconnect").clicked() {
                                    self.core.send(Command::Disconnect);
                                }
                            });
                        });
                });
            self.toolbar.rect = Some(pill.response.rect);
        } else {
            self.toolbar.rect = None;
        }
    }

    /// The statistics card in the top-right corner of the picture.
    /// The "Live stats" card of the session board: four figures, the
    /// bandwidth chart and the pixel encoding mix, over the picture.
    #[allow(clippy::too_many_lines)] // one card, easier to read whole
    fn stats_overlay(&self, ctx: &Context, tokens: &Tokens) {
        let tokens = &tokens.reduce_transparency();
        // Below the top bar in a window; clear of the toolbar tab in full screen.
        let top = if self.fullscreen { 16.0 } else { 70.0 };
        egui::Area::new(egui::Id::new("stats-overlay"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, vec2(-18.0, top))
            .interactable(false)
            .show(ctx, |ui| {
                Self::card_frame(tokens, 18, 18).show(ui, |ui| {
                    ui.set_width(284.0);
                    ui.spacing_mut().item_spacing.y = 12.0;
                    ui.label(RichText::new("SESSION").color(tokens.muted).size(10.5));
                    ui.label(
                        RichText::new("Live stats")
                            .family(style::semibold(ui.ctx()))
                            .size(16.0),
                    );
                    let Some(s) = &self.stats else {
                        ui.label(RichText::new("Waiting for data…").color(tokens.muted));
                        return;
                    };
                    let (rtt_value, rtt_unit) = match s.rtt {
                        Some(d) => (d.as_millis().to_string(), "ms"),
                        None => ("n/a".to_owned(), ""),
                    };
                    let (rx_value, rx_unit) = split_rate(stats::format_rate(s.rx_per_sec));
                    let (tx_value, tx_unit) = split_rate(stats::format_rate(s.tx_per_sec));
                    let figures = [
                        ("Round trip", rtt_value, rtt_unit.to_owned()),
                        ("Received", rx_value, rx_unit),
                        ("Sent", tx_value, tx_unit),
                        (
                            "Updates",
                            format!("{:.0}", s.updates_per_sec),
                            "per s".to_owned(),
                        ),
                    ];
                    egui::Grid::new("stats-tiles")
                        .spacing([10.0, 10.0])
                        .show(ui, |ui| {
                            for (i, (label, value, unit)) in figures.iter().enumerate() {
                                Frame::NONE
                                    .fill(widgets::inset(tokens))
                                    .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
                                    .corner_radius(style::radius(12.0))
                                    .inner_margin(Margin::same(12))
                                    .show(ui, |ui| {
                                        ui.set_width(113.0);
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        ui.label(
                                            RichText::new(*label).color(tokens.muted).size(11.5),
                                        );
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            ui.label(
                                                RichText::new(value)
                                                    .family(style::semibold(ui.ctx()))
                                                    .size(20.0),
                                            );
                                            ui.label(
                                                RichText::new(unit).color(tokens.muted).size(11.0),
                                            );
                                        });
                                    });
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Bandwidth, last 60 s")
                                .color(tokens.muted)
                                .size(12.0),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(stats::format_rate(s.rx_per_sec))
                                    .strong()
                                    .size(12.0),
                            );
                        });
                    });
                    let history: Vec<f32> = self.rx_history.iter().copied().collect();
                    widgets::area_chart(
                        ui,
                        vec2(284.0, 90.0),
                        &[(&history, Color32::from(tokens.accent))],
                        HISTORY_LEN,
                        16_384.0,
                        Color32::from(tokens.line),
                    );
                    ui.separator();
                    let (w, h) = self.remote_size;
                    let mix = stats::pixel_mix(s, 3)
                        .into_iter()
                        .map(|(e, pct)| format!("{} {pct}%", stats::encoding_name(e)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mix = if mix.is_empty() {
                        "no pixel data yet".to_owned()
                    } else {
                        mix
                    };
                    let rows = [
                        ("Encoding", mix),
                        ("Screen", format!("{w}×{h}")),
                        (
                            "Transferred",
                            format!(
                                "{} in, {} out",
                                stats::format_bytes(precise(s.total_rx)),
                                stats::format_bytes(precise(s.total_tx))
                            ),
                        ),
                        ("Keyboard", self.capture_summary()),
                        ("Rate", format!("{:.0} fps", s.updates_per_sec)),
                    ];
                    ui.spacing_mut().item_spacing.y = 8.0;
                    for (key, value) in rows {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(key).color(tokens.muted).size(12.5));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(value).strong().size(12.5));
                            });
                        });
                    }
                });
            });
    }

    /// A short-lived message at the bottom of the window.
    fn toast(&self, ctx: &Context, tokens: &Tokens) {
        let tokens = &tokens.reduce_transparency();
        let Some(message) = self.notice() else { return };
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -16.0))
            .interactable(false)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(tokens.card.into())
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
                    .corner_radius(style::radius(tokens.radius / 2.0))
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).color(tokens.amber));
                    });
            });
    }

    fn card_frame(tokens: &Tokens, x: i8, y: i8) -> Frame {
        Frame::NONE
            .fill(tokens.card.into())
            .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
            .corner_radius(style::radius(tokens.radius))
            .inner_margin(Margin::symmetric(x, y))
    }

    #[allow(clippy::too_many_lines)] // side bar and the form, easier to read whole
    fn home(&mut self, ctx: &Context, tokens: &Tokens) {
        egui::SidePanel::left("home-nav")
            .exact_width(264.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::NONE
                    .fill(style::page_fill(tokens))
                    .inner_margin(Margin {
                        left: 14,
                        right: 0,
                        top: 12,
                        bottom: 14,
                    }),
            )
            .show(ctx, |ui| {
                Self::card_frame(tokens, 14, 22).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.horizontal(|ui| {
                        widgets::brand_mark(ui, tokens, 38.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(
                                RichText::new("Farpane Viewer")
                                    .family(style::semibold(ui.ctx()))
                                    .size(15.0),
                            );
                            ui.label(RichText::new("Remote desktop").color(tokens.muted).small());
                        });
                    });
                    ui.add_space(14.0);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let saved = self.store.settings.connections.len();
                    widgets::nav_item(
                        ui,
                        tokens,
                        true,
                        farpane_ui::icons::Icon::Displays,
                        "Connections",
                        Some(saved),
                    );
                    if widgets::nav_item(
                        ui,
                        tokens,
                        false,
                        farpane_ui::icons::Icon::Appearance,
                        "Appearance",
                        None,
                    ) {
                        self.settings_open = true;
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(style::page_fill(tokens))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 18.0;
                        Self::card_frame(tokens, 24, 20).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.scope(|ui| {
                                ui.set_max_width(460.0);
                                ui.label(
                                    RichText::new("Quick connect")
                                        .size(18.0)
                                        .family(style::semibold(ui.ctx())),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new("Any VNC-compatible server").color(tokens.muted),
                                );
                                ui.add_space(18.0);
                                let connecting = matches!(self.phase, Phase::Connecting);
                                ui.label(RichText::new("Address").color(tokens.muted));
                                let addr = ui.add_enabled(
                                    !connecting,
                                    TextEdit::singleline(&mut self.addr)
                                        .hint_text("host:5900")
                                        .desired_width(f32::INFINITY),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new("Username (only if the server asks)")
                                        .color(tokens.muted),
                                );
                                ui.add_enabled(
                                    !connecting,
                                    TextEdit::singleline(&mut self.username)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new("Password (if the server asks)")
                                        .color(tokens.muted),
                                );
                                let password = ui.add_enabled(
                                    !connecting,
                                    TextEdit::singleline(&mut self.password)
                                        .password(true)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.add_space(16.0);
                                let submit = (addr.lost_focus() || password.lost_focus())
                                    && ui.input(|i| i.key_pressed(Key::Enter));
                                ui.horizontal(|ui| {
                                    let label = if connecting {
                                        "Connecting…"
                                    } else {
                                        "Connect"
                                    };
                                    let button = egui::Button::new(
                                        RichText::new(label).color(Color32::WHITE).strong(),
                                    )
                                    .fill(Color32::from(tokens.accent))
                                    .min_size(vec2(120.0, 32.0));
                                    let can = !connecting && !self.addr.trim().is_empty();
                                    let clicked = ui.add_enabled(can, button).clicked();
                                    if can && (clicked || submit) {
                                        self.connect();
                                    }
                                    if connecting && ui.button("Cancel").clicked() {
                                        self.core.send(Command::Disconnect);
                                    }
                                });
                                if let Some(error) = &self.error {
                                    ui.add_space(12.0);
                                    ui.label(RichText::new(error).color(tokens.red));
                                }
                            });
                        });
                        if !self.store.settings.connections.is_empty() {
                            self.address_book(ui, tokens);
                        }
                    });
            });
    }

    /// The saved connections as rows: Connect to use one, × to forget it.
    fn address_book(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let now = unix_now();
        let connecting = matches!(self.phase, Phase::Connecting);
        let entries: Vec<_> = self
            .store
            .settings
            .recent()
            .into_iter()
            .take(50)
            .cloned()
            .collect();
        let mut connect = None;
        let mut forget = None;
        Self::card_frame(tokens, 24, 18).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                RichText::new("Saved connections")
                    .family(style::semibold(ui.ctx()))
                    .size(16.0),
            );
            ui.label(
                RichText::new("Sorted by last use")
                    .color(tokens.muted)
                    .size(12.5),
            );
            ui.add_space(8.0);
            for entry in &entries {
                ui.separator();
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        style::radius(9.0),
                        Color32::from(tokens.accent).gamma_multiply(0.85),
                    );
                    let initial: String = entry
                        .name
                        .chars()
                        .take(1)
                        .flat_map(char::to_uppercase)
                        .collect();
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        initial,
                        egui::FontId::proportional(13.0),
                        Color32::from(tokens.bg),
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(&entry.name).strong());
                        ui.label(RichText::new(&entry.addr).color(tokens.muted).size(12.0));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("×")
                            .on_hover_text("Forget this connection")
                            .clicked()
                        {
                            forget = Some(entry.addr.clone());
                        }
                        let button =
                            egui::Button::new(RichText::new("Connect").color(Color32::WHITE))
                                .fill(Color32::from(tokens.accent));
                        if ui.add_enabled(!connecting, button).clicked() {
                            connect = Some(entry.addr.clone());
                        }
                        ui.label(
                            RichText::new(settings::ago(now, entry.last_connected))
                                .color(tokens.muted)
                                .size(12.0),
                        );
                        if self
                            .store
                            .settings
                            .known_hosts
                            .iter()
                            .any(|k| k.host == entry.addr)
                        {
                            widgets::badge(ui, "Verified", tokens.green.into());
                        }
                    });
                });
            }
        });
        if let Some(addr) = forget {
            self.forget_saved(&addr);
        }
        if let Some(addr) = connect {
            self.addr = addr;
            self.connect();
        }
    }

    /// The "do you trust this server?" dialog.
    fn trust_window(&mut self, ctx: &Context, tokens: &Tokens) {
        let Some(prompt) = self.trust_prompt.clone() else {
            return;
        };
        let changed = prompt.problem == CertProblem::Changed;
        let mut decision = None;
        // A dialog over the connect form must be opaque, or both texts mix.
        let solid = tokens.reduce_transparency();
        egui::Window::new(if changed {
            "Certificate changed"
        } else {
            "Trust this server?"
        })
        .frame(
            Frame::NONE
                .fill(solid.card.into())
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    Color32::from(if changed { tokens.red } else { tokens.line }),
                ))
                .corner_radius(style::radius(tokens.radius / 1.5))
                .inner_margin(Margin::same(18)),
        )
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_max_width(460.0);
            match &prompt.problem {
                CertProblem::Changed => {
                    ui.label(
                        RichText::new(format!(
                            "WARNING: the certificate of {} is different from the one you trusted.",
                            prompt.host
                        ))
                        .color(tokens.red)
                        .strong(),
                    );
                    ui.label(
                        "This can mean the server's certificate was renewed. It can also mean \
                             someone is intercepting the connection. Only continue if you expected \
                             this change and the fingerprint below matches what the server's \
                             administrator gave you.",
                    );
                }
                CertProblem::Unknown => {
                    ui.label(format!(
                        "{} has not been seen before and its certificate is not signed by a \
                             known authority.",
                        prompt.host
                    ));
                    ui.label(
                        "Compare the fingerprint with the one the server's administrator gave you.",
                    );
                }
                CertProblem::Invalid(why) => {
                    ui.label(
                        RichText::new(format!(
                            "The certificate of {} has a problem: {why}.",
                            prompt.host
                        ))
                        .color(tokens.amber),
                    );
                }
            }
            ui.add_space(8.0);
            ui.label(RichText::new("SHA-256 fingerprint").color(tokens.muted));
            ui.label(
                RichText::new(format_fingerprint(&prompt.fingerprint))
                    .monospace()
                    .size(12.0),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let label = if changed {
                    "Replace and connect"
                } else {
                    "Trust and connect"
                };
                let button = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(
                    Color32::from(if changed { tokens.red } else { tokens.accent }),
                );
                if ui.add(button).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => self.trust_pending(),
            Some(false) => self.reject_pending(),
            None => {}
        }
    }

    /// The Settings window: appearance options, applied and saved at once.
    fn settings_window(&mut self, ctx: &Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_TOP, vec2(-16.0, 56.0))
            .show(ctx, |ui| {
                egui::Grid::new("settings-grid")
                    .num_columns(2)
                    .spacing([16.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("Theme");
                        egui::ComboBox::from_id_salt("settings-theme")
                            .selected_text(appearance::theme_label(self.theme))
                            .show_ui(ui, |ui| {
                                for t in appearance::THEMES {
                                    ui.selectable_value(
                                        &mut self.theme,
                                        t,
                                        appearance::theme_label(t),
                                    );
                                }
                            });
                        ui.end_row();
                        ui.label("Accent colour");
                        egui::ComboBox::from_id_salt("settings-accent")
                            .selected_text(appearance::accent_label(self.accent))
                            .show_ui(ui, |ui| {
                                for a in appearance::ACCENTS {
                                    ui.selectable_value(
                                        &mut self.accent,
                                        a,
                                        appearance::accent_label(a),
                                    );
                                }
                            });
                        ui.end_row();
                        ui.label("Interface size");
                        ui.horizontal(|ui| {
                            for size in appearance::SIZES {
                                ui.selectable_value(
                                    &mut self.interface_size,
                                    size,
                                    format!("{size}%"),
                                );
                            }
                        });
                        ui.end_row();
                        ui.label("Reduce transparency");
                        ui.checkbox(&mut self.reduce_transparency, "Solid panels");
                        ui.end_row();
                        ui.label("Session footer");
                        ui.checkbox(&mut self.show_footer, "Show under the picture");
                        ui.end_row();
                        ui.label("H.264 video");
                        self.h264_row(ui);
                        ui.end_row();
                        ui.label("Capture keyboard");
                        ui.horizontal(|ui| {
                            for pref in [CapturePref::Auto, CapturePref::Always, CapturePref::Never]
                            {
                                ui.selectable_value(&mut self.capture_pref, pref, pref.label());
                            }
                        });
                        ui.end_row();
                    });
            });
        self.settings_open = open;
    }

    fn session(&mut self, ctx: &Context) {
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(Color32::BLACK))
            .show(ctx, |ui| {
                let available = ui.available_rect_before_wrap();
                let (rect, response) =
                    ui.allocate_exact_size(available.size(), Sense::click_and_drag());
                self.follow_window_size(ctx, rect);
                let canvas = Canvas::fit(rect, self.remote_size, self.one_to_one);
                if let Some(texture) = &self.texture {
                    let painter = ui.painter_at(rect);
                    painter.image(
                        texture.id(),
                        canvas.image,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                // Keep Tab, arrows and Escape for the remote machine.
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        response.id,
                        EventFilter {
                            tab: true,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    );
                });
                if !response.has_focus() && !ctx.memory(|m| m.focused().is_some()) {
                    response.request_focus();
                }
                if response.clicked() {
                    response.request_focus();
                    self.released_by_user = false;
                }
                self.draw_cursor(ctx, &response, &canvas);
                self.forward_input(ctx, &response, &canvas);
            });
    }

    /// Draw the server's cursor at the pointer and hide the OS cursor; until
    /// the server has sent a shape, the local arrow stays.
    fn draw_cursor(&self, ctx: &Context, response: &egui::Response, canvas: &Canvas) {
        if !response.hovered() {
            return;
        }
        let Some(cursor) = &self.cursor else {
            ctx.set_cursor_icon(CursorIcon::Default);
            return;
        };
        ctx.set_cursor_icon(CursorIcon::None);
        let RemoteCursor::Shape {
            texture,
            hotspot,
            size,
        } = cursor
        else {
            return;
        };
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        // Points per remote pixel: the cursor scales with the picture.
        let scale = canvas.image.width() / f32::from(self.remote_size.0.max(1));
        let top_left = pos - vec2(f32::from(hotspot.0), f32::from(hotspot.1)) * scale;
        let rect =
            egui::Rect::from_min_size(top_left, vec2(f32::from(size.0), f32::from(size.1)) * scale);
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("remote-cursor"),
        ))
        .image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    fn forward_input(&mut self, ctx: &Context, response: &egui::Response, canvas: &Canvas) {
        let (mut events, mods, pos, focused) = ctx.input(|i| {
            (
                i.events.clone(),
                i.modifiers,
                i.pointer.hover_pos(),
                i.focused,
            )
        });
        events.retain(|e| {
            !matches!(e, egui::Event::Key { key: Key::F11, .. })
                && !matches!(e, egui::Event::Key { key: Key::S, modifiers, .. }
                    if modifiers.ctrl && modifiers.alt && modifiers.shift)
        });
        // egui-winit reports focus changes both as a flag and as an event; the
        // last event of the frame wins.
        let focused = events
            .iter()
            .rev()
            .find_map(|e| match e {
                egui::Event::WindowFocused(f) => Some(*f),
                _ => None,
            })
            .unwrap_or(focused);
        if !focused {
            self.release_input();
            return;
        }
        if response.has_focus() {
            // The remote machine must have the text before it sees Ctrl+V.
            let pasted = events.iter().rev().find_map(|e| match e {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            });
            if let (true, Some(text)) = (self.clipboard_sync, pasted) {
                self.send_clipboard(&text);
            }
            for k in self.keys.translate(&events, mods) {
                self.core.send(Command::Key {
                    keysym: k.keysym,
                    down: k.down,
                });
            }
        }
        if response.hovered() || self.pointer.has_buttons() {
            for p in self.pointer.translate(&events, canvas, pos) {
                self.last_remote_pos = (p.x, p.y);
                self.core.send(Command::Pointer {
                    buttons: p.buttons,
                    x: p.x,
                    y: p.y,
                });
            }
        }
    }

    /// `--ui-screenshot`: save the window once it has had time to draw, then
    /// quit. Waits for a connection when an address was given.
    fn screenshot_driver(&mut self, ctx: &Context) {
        let Some(path) = self.ui_screenshot.clone() else {
            return;
        };
        let ready = match self.connected_at {
            Some(at) => at.elapsed() > Duration::from_millis(2500),
            // A trust question on screen is what the screenshot is for.
            None if self.trust_prompt.is_some() => {
                self.started.elapsed() > Duration::from_millis(1500)
            }
            None => {
                self.startup_password.is_none()
                    && self.addr.is_empty()
                    && self.started.elapsed() > Duration::from_millis(1200)
            }
        };
        if ready && !self.screenshot_requested {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(UserData::default()));
        }
        if self.screenshot_requested {
            let shot = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = shot {
                if let Err(e) = save_screenshot(&image, &path) {
                    eprintln!("farpane-viewer: could not save {}: {e}", path.display());
                }
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

/// What a resize refusal code means (spec, `ExtendedDesktopSize`).
fn resize_refusal(status: u16) -> &'static str {
    match status {
        1 => "resizing is prohibited on this server",
        2 => "out of resources",
        3 => "invalid screen layout",
        _ => "unknown reason",
    }
}

/// The Ctrl+Alt+Shift+S chord that toggles the statistics overlay.
fn is_stats_hotkey(i: &egui::InputState) -> bool {
    i.modifiers.ctrl && i.modifiers.alt && i.modifiers.shift && i.key_pressed(Key::S)
}

/// A byte count as `f64` for display (counts stay far below 2^52).
#[allow(clippy::cast_precision_loss)]
fn precise(n: u64) -> f64 {
    n as f64
}

/// Seconds since the Unix epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn save_screenshot(
    image: &ColorImage,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(
        file,
        u32::try_from(image.width())?,
        u32::try_from(image.height())?,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(image.as_raw())?;
    Ok(())
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        self.ensure_grabber(frame);
        self.ui(ctx);
    }
}

/// `1.5 MB/s` as `("1.5", "MB/s")`.
fn split_rate(text: String) -> (String, String) {
    match text.rsplit_once(' ') {
        Some((value, unit)) => (value.to_owned(), unit.to_owned()),
        None => (text, String::new()),
    }
}
