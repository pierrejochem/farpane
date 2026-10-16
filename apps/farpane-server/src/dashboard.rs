//! The server's window: overview, clients, users and the event log
//! (design doc, section 16, "Server dashboard").
//!
//! The window only reads from and acts on a [`Backend`]; the server keeps
//! running without it (`--headless`).

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use egui::{
    Align, Color32, ColorImage, Context, Frame, Layout, Margin, RichText, ScrollArea,
    TextureHandle, TextureOptions, Ui, vec2,
};
use farpane_ui::{Accent, ThemePreference, Tokens, style, widgets};
use rfb_proto::consts::enc;
use rfb_server::ClientInfo;

use crate::{
    backend::{Backend, EventLog, Level, Status},
    settings::{self, ServerSettings, TlsMode},
    usercmd,
    users::{Role, User, UserStore},
};

/// The pages in the side bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Clients,
    Users,
    Security,
    Log,
}

impl Page {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::Clients,
        Self::Users,
        Self::Security,
        Self::Log,
    ];

    /// `overview`, `clients`, `users` or `log`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "overview" => Some(Self::Overview),
            "clients" => Some(Self::Clients),
            "users" => Some(Self::Users),
            "security" => Some(Self::Security),
            "log" => Some(Self::Log),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Clients => "Clients",
            Self::Users => "Users",
            Self::Security => "Security",
            Self::Log => "Event log",
        }
    }
}

/// How often the readings are refreshed.
const SAMPLE_EVERY: f64 = 0.25;
/// Seconds of bandwidth history on the chart (one point per sample).
const HISTORY_LEN: usize = 120;
/// Longest edge of the live preview, in pixels.
const PREVIEW_EDGE: u16 = 360;
/// How often the preview is refreshed.
const PREVIEW_EVERY: f64 = 0.5;

/// Form state of the Users page.
#[derive(Debug)]
struct UsersPanel {
    list: Vec<User>,
    /// A problem with the users file, shown instead of the list.
    problem: Option<String>,
    name: String,
    display_name: String,
    role: Role,
    password: String,
    /// The result of the last action: (was it fine, what to say).
    message: Option<(bool, String)>,
    last_load: f64,
}

impl UsersPanel {
    fn new() -> Self {
        Self {
            list: Vec::new(),
            problem: None,
            name: String::new(),
            display_name: String::new(),
            role: Role::Control,
            password: String::new(),
            message: None,
            last_load: f64::NEG_INFINITY,
        }
    }
}

/// Which key a pending "make a new one" is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forget {
    Certificate,
    Ra2Key,
}

/// Form state of the Security page.
#[derive(Debug, Default)]
struct SecurityPanel {
    loaded: bool,
    /// A problem with `server.toml`, shown instead of the form.
    problem: Option<String>,
    edit: ServerSettings,
    listen_text: String,
    /// Waiting for the user to confirm forgetting a key.
    confirm: Option<Forget>,
    /// The result of the last action: (was it fine, what to say).
    message: Option<(bool, String)>,
    /// Whether any account exists (a reachable server needs one).
    has_users: bool,
}

/// The dashboard's state.
pub struct Dashboard {
    backend: Arc<dyn Backend>,
    log: EventLog,
    /// `users.toml`, which the Users page edits.
    users_file: PathBuf,
    /// Where the log export goes.
    export_dir: PathBuf,
    page: Page,
    theme: ThemePreference,
    accent: Accent,
    status: Status,
    clients: Vec<ClientInfo>,
    last_sample: f64,
    last_totals: (u64, u64),
    /// Bytes per second in and out since the last sample.
    rates: (f64, f64),
    out_history: VecDeque<f32>,
    preview: Option<TextureHandle>,
    last_preview: f64,
    users: UsersPanel,
    security: SecurityPanel,
    /// A line at the bottom of the window (exports, errors).
    notice: Option<String>,
    /// `--ui-screenshot`: where to save a picture of the window, and the
    /// state of that request.
    screenshot: Option<std::path::PathBuf>,
    screenshot_requested: bool,
    screenshot_delay: f64,
    started: Option<f64>,
}

/// `12.3 KB`, `4.56 MB` (decimal units, as network figures are quoted).
#[must_use]
pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1000.0 {
        format!("{bytes:.0} B")
    } else if bytes < 1_000_000.0 {
        format!("{:.1} KB", bytes / 1000.0)
    } else if bytes < 1_000_000_000.0 {
        format!("{:.2} MB", bytes / 1_000_000.0)
    } else {
        format!("{:.2} GB", bytes / 1_000_000_000.0)
    }
}

/// `12.3 KB/s`.
#[must_use]
pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// `45 s`, `3 min`, `2 h 5 min`.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h {} min", s / 3600, s % 3600 / 60)
    }
}

/// The encoding of the last rectangle a client was sent. (`0` is Raw; a
/// client that was sent nothing yet is shown as `-` by the table.)
#[must_use]
pub fn encoding_name(encoding: i32) -> &'static str {
    match encoding {
        enc::RAW => "Raw",
        enc::HEXTILE => "Hextile",
        enc::TRLE => "TRLE",
        enc::ZRLE => "ZRLE",
        enc::TIGHT => "Tight",
        _ => "other",
    }
}

/// A client as the table names it: the account, else the address.
#[must_use]
pub fn client_label(info: &ClientInfo) -> String {
    let peer = info
        .peer
        .map_or_else(|| "local".to_owned(), |p| p.to_string());
    match &info.user {
        Some(user) => format!("{user} ({peer})"),
        None => peer,
    }
}

/// `HH:MM:SS` UTC of `at`.
#[must_use]
pub fn clock(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

impl Dashboard {
    /// A dashboard for `backend`. `users_file` is edited by the Users page;
    /// log exports go to `export_dir`.
    #[must_use]
    pub fn new(
        backend: Arc<dyn Backend>,
        log: EventLog,
        users_file: PathBuf,
        export_dir: PathBuf,
    ) -> Self {
        let status = backend.status();
        Self {
            backend,
            log,
            users_file,
            export_dir,
            page: Page::Overview,
            theme: ThemePreference::Glass,
            accent: Accent::ThemeDefault,
            status,
            clients: Vec::new(),
            last_sample: f64::NEG_INFINITY,
            last_totals: (0, 0),
            rates: (0.0, 0.0),
            out_history: VecDeque::new(),
            preview: None,
            last_preview: f64::NEG_INFINITY,
            users: UsersPanel::new(),
            security: SecurityPanel::default(),
            notice: None,
            screenshot: None,
            screenshot_requested: false,
            screenshot_delay: 1.5,
            started: None,
        }
    }

    /// Save a picture of the window to `path` once it has had time to draw,
    /// then close it; and start on `page`.
    pub fn set_screenshot(&mut self, path: Option<PathBuf>, page: Page, delay_secs: f64) {
        self.screenshot = path;
        self.page = page;
        self.screenshot_delay = delay_secs;
    }

    #[must_use]
    pub fn page(&self) -> Page {
        self.page
    }

    pub fn set_page(&mut self, page: Page) {
        self.page = page;
    }

    /// The last reading of the connected clients.
    #[must_use]
    pub fn clients(&self) -> &[ClientInfo] {
        &self.clients
    }

    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Bytes per second `(in, out)` over the last sample.
    #[must_use]
    pub fn rates(&self) -> (f64, f64) {
        self.rates
    }

    fn tokens(&self, ctx: &Context) -> Tokens {
        let dark = ctx.input(|i| i.raw.system_theme) != Some(egui::Theme::Light);
        Tokens::new(self.theme.resolve(dark), self.accent)
    }

    /// Take new readings when it is time. `now` is egui's clock in seconds.
    pub fn refresh(&mut self, ctx: &Context, now: f64) {
        if now - self.last_sample >= SAMPLE_EVERY {
            let elapsed = (now - self.last_sample).min(5.0);
            self.clients = self.backend.clients();
            self.status = self.backend.status();
            let totals = self
                .clients
                .iter()
                .fold((0u64, 0u64), |(i, o), c| (i + c.bytes_in, o + c.bytes_out));
            // Clients leaving make the sum drop: count that as no traffic.
            // Byte counts stay far below 2^52, where `f64` is exact.
            #[allow(clippy::cast_precision_loss)]
            let per_sec = |new: u64, old: u64| {
                if self.last_sample.is_finite() && elapsed > 0.0 {
                    new.saturating_sub(old) as f64 / elapsed
                } else {
                    0.0
                }
            };
            self.rates = (
                per_sec(totals.0, self.last_totals.0),
                per_sec(totals.1, self.last_totals.1),
            );
            self.last_totals = totals;
            self.last_sample = now;
            if self.out_history.len() >= HISTORY_LEN {
                self.out_history.pop_front();
            }
            #[allow(clippy::cast_possible_truncation)] // a chart value
            self.out_history.push_back(self.rates.1 as f32);
        }
        if self.page == Page::Overview && now - self.last_preview >= PREVIEW_EVERY {
            self.last_preview = now;
            let p = self.backend.preview(PREVIEW_EDGE);
            let image = ColorImage::from_rgba_unmultiplied(
                [usize::from(p.width), usize::from(p.height)],
                &p.rgba,
            );
            match &mut self.preview {
                Some(tex) => tex.set(image, TextureOptions::LINEAR),
                None => {
                    self.preview = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR));
                }
            }
        }
        if self.page == Page::Security && !self.security.loaded {
            self.load_security();
        }
        if self.page == Page::Users && now - self.users.last_load >= 1.0 {
            self.reload_users(now);
        }
    }

    fn settings_path(&self) -> PathBuf {
        self.export_dir.join("server.toml")
    }

    fn load_security(&mut self) {
        self.security.loaded = true;
        self.security.has_users =
            UserStore::load(&self.users_file).is_ok_and(|s| s.has_enabled_users());
        match ServerSettings::load(&self.settings_path()) {
            Ok(s) => {
                self.security.listen_text = s.listen.clone().unwrap_or_default();
                self.security.edit = s;
                self.security.problem = None;
            }
            Err(e) => self.security.problem = Some(e.to_string()),
        }
    }

    /// Save the Security page's settings (they apply at the next start).
    pub fn save_security(&mut self) {
        let listen = self.security.listen_text.trim().to_owned();
        if !listen.is_empty() && listen.parse::<std::net::SocketAddr>().is_err() {
            self.security.message = Some((
                false,
                format!("\"{listen}\" is not an address and port, such as 127.0.0.1:5900"),
            ));
            return;
        }
        let mut s = self.security.edit.clone();
        s.listen = (!listen.is_empty()).then_some(listen);
        self.security.message = Some(match s.save(&self.settings_path()) {
            Ok(()) => {
                self.log.push(
                    Level::Info,
                    "security settings saved (they apply at the next start)",
                );
                self.security.edit = s;
                (true, "Saved. Restart the server to apply.".to_owned())
            }
            Err(e) => (false, e.to_string()),
        });
    }

    /// Forget a key so that a new one is made at the next start.
    pub fn forget_key(&mut self, which: Forget) {
        let (what, result) = match which {
            Forget::Certificate => (
                "certificate",
                settings::forget_certificate(&self.export_dir),
            ),
            Forget::Ra2Key => ("RSA-AES key", settings::forget_ra2_key(&self.export_dir)),
        };
        self.security.confirm = None;
        self.security.message = Some(match result {
            Ok(()) => {
                self.log.push(
                    Level::Warning,
                    format!("the {what} was forgotten; a new one is made at the next start"),
                );
                (
                    true,
                    format!(
                        "The {what} was removed. A new one is made at the next start; viewers will ask to trust it again."
                    ),
                )
            }
            Err(e) => (false, e.to_string()),
        });
    }

    fn reload_users(&mut self, now: f64) {
        self.users.last_load = now;
        match UserStore::load(&self.users_file) {
            Ok(store) => {
                self.users.list = store.users().to_vec();
                self.users.problem = None;
            }
            Err(e) => {
                self.users.list.clear();
                self.users.problem = Some(e.to_string());
            }
        }
    }

    /// Run a `user` command and report its outcome on the Users page.
    fn user_command(&mut self, args: &[&str], password: Option<&str>) {
        let words: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let mut out = Vec::new();
        let result = usercmd::run(
            &words,
            &self.users_file,
            &mut || Ok(password.unwrap_or_default().to_owned()),
            &mut out,
        );
        self.users.message = Some(match result {
            Ok(()) => (true, String::from_utf8_lossy(&out).trim().to_owned()),
            Err(e) => (false, e),
        });
        self.users.last_load = f64::NEG_INFINITY;
    }

    /// End a client's connection and say so in the log.
    pub fn disconnect(&mut self, id: u64) {
        if let Some(info) = self.clients.iter().find(|c| c.id == id) {
            let label = client_label(info);
            if self.backend.disconnect(id) {
                self.log.push(
                    Level::Info,
                    format!("{label} was disconnected from the dashboard"),
                );
            }
        }
    }

    /// Switch a client between view-only and full control.
    pub fn set_view_only(&mut self, id: u64, view_only: bool) {
        if let Some(info) = self.clients.iter().find(|c| c.id == id) {
            let label = client_label(info);
            if self.backend.set_view_only(id, view_only) {
                self.log.push(
                    Level::Info,
                    format!(
                        "{label} is now {}",
                        if view_only {
                            "view only"
                        } else {
                            "allowed to control"
                        }
                    ),
                );
            }
        }
    }

    /// Write the log as JSON next to the settings.
    pub fn export_log(&mut self) {
        let path = self.export_dir.join("event-log.json");
        self.notice = Some(
            match std::fs::create_dir_all(&self.export_dir)
                .and_then(|()| std::fs::write(&path, self.log.to_json()))
            {
                Ok(()) => format!("Event log saved to {}", path.display()),
                Err(e) => format!("Could not save the event log: {e}"),
            },
        );
    }

    /// Draw the window. Call once per frame.
    pub fn show(&mut self, ctx: &Context) {
        let now = ctx.input(|i| i.time);
        self.refresh(ctx, now);
        let tokens = self.tokens(ctx);
        style::apply(ctx, &tokens);

        egui::SidePanel::left("nav")
            .exact_width(190.0)
            .frame(
                Frame::NONE
                    .fill(tokens.card.into())
                    .inner_margin(Margin::same(14)),
            )
            .show(ctx, |ui| self.nav(ui, &tokens));
        egui::TopBottomPanel::bottom("notice")
            .frame(Frame::NONE.inner_margin(Margin::symmetric(14, 6)))
            .show_animated(ctx, self.notice.is_some(), |ui| {
                if let Some(text) = self.notice.clone() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(text).color(tokens.muted));
                        if ui.small_button("Dismiss").clicked() {
                            self.notice = None;
                        }
                    });
                }
            });
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(tokens.bg.into())
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading(self.page.title());
                        ui.add_space(8.0);
                        match self.page {
                            Page::Overview => self.overview(ui, &tokens),
                            Page::Clients => self.clients_page(ui, &tokens),
                            Page::Users => self.users_page(ui, &tokens),
                            Page::Security => self.security_page(ui, &tokens),
                            Page::Log => self.log_page(ui, &tokens),
                        }
                    });
            });
        self.screenshot_driver(ctx, now);
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn screenshot_driver(&mut self, ctx: &Context, now: f64) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        let started = *self.started.get_or_insert(now);
        if now - started > self.screenshot_delay && !self.screenshot_requested {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        if self.screenshot_requested {
            let shot = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = shot {
                if let Err(e) = save_png(&image, &path) {
                    eprintln!("farpane-server: could not save {}: {e}", path.display());
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn nav(&mut self, ui: &mut Ui, tokens: &Tokens) {
        ui.label(RichText::new("Farpane Server").strong().size(18.0));
        ui.add_space(12.0);
        for page in Page::ALL {
            let count = match page {
                Page::Clients if !self.clients.is_empty() => format!(" ({})", self.clients.len()),
                _ => String::new(),
            };
            let label = format!("{}{count}", page.title());
            if ui.selectable_label(self.page == page, label).clicked() {
                self.page = page;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new(env!("CARGO_PKG_VERSION"))
                    .color(tokens.muted)
                    .small(),
            );
            egui::ComboBox::from_id_salt("theme")
                .selected_text(crate::dashboard::theme_label(self.theme))
                .show_ui(ui, |ui| {
                    for t in [
                        ThemePreference::Glass,
                        ThemePreference::Slate,
                        ThemePreference::Light,
                        ThemePreference::System,
                    ] {
                        ui.selectable_value(&mut self.theme, t, theme_label(t));
                    }
                });
            ui.label(RichText::new("Theme").color(tokens.muted).small());
        });
    }

    fn card<R>(ui: &mut Ui, tokens: &Tokens, add: impl FnOnce(&mut Ui) -> R) -> R {
        Frame::NONE
            .fill(tokens.card.into())
            .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
            .corner_radius(style::radius(tokens.radius / 1.5))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, add)
            .inner
    }

    fn overview(&mut self, ui: &mut Ui, tokens: &Tokens) {
        let running = self.status.failure.is_none();
        ui.horizontal_wrapped(|ui| {
            Self::card(ui, tokens, |ui| {
                ui.set_min_width(170.0);
                ui.label(RichText::new("Status").color(tokens.muted).small());
                let (text, colour) = if running {
                    ("Running", tokens.green)
                } else {
                    ("Stopped", tokens.red)
                };
                ui.label(RichText::new(text).strong().size(18.0).color(colour));
                ui.label(RichText::new(format!("Listening on {}", self.status.listen)).small());
                if let Some(why) = &self.status.failure {
                    ui.label(
                        RichText::new(format!("Capture stopped: {why}"))
                            .color(tokens.red)
                            .small(),
                    );
                }
            });
            Self::card(ui, tokens, |ui| {
                ui.set_min_width(130.0);
                ui.label(RichText::new("Clients").color(tokens.muted).small());
                ui.label(
                    RichText::new(self.clients.len().to_string())
                        .strong()
                        .size(18.0),
                );
                let viewing = self.clients.iter().filter(|c| c.view_only).count();
                ui.label(RichText::new(format!("{viewing} view only")).small());
            });
            Self::card(ui, tokens, |ui| {
                ui.set_min_width(200.0);
                ui.label(RichText::new("Traffic").color(tokens.muted).small());
                ui.label(
                    RichText::new(format!("out {}", format_rate(self.rates.1)))
                        .strong()
                        .size(18.0),
                );
                ui.label(RichText::new(format!("in {}", format_rate(self.rates.0))).small());
                let history: Vec<f32> = self.out_history.iter().copied().collect();
                widgets::sparkline(
                    ui,
                    &history,
                    vec2(180.0, 28.0),
                    HISTORY_LEN,
                    None,
                    tokens.accent.into(),
                );
            });
        });
        ui.add_space(10.0);
        Self::card(ui, tokens, |ui| {
            ui.label(RichText::new("Sharing").color(tokens.muted).small());
            let (w, h) = self.status.screen;
            ui.label(format!("{} ({w} × {h})", self.status.source));
            ui.label(RichText::new(&self.status.access).small());
            for (label, fingerprint) in &self.status.fingerprints {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("{label} SHA-256"))
                            .color(tokens.muted)
                            .small(),
                    );
                    ui.label(RichText::new(fingerprint).monospace().small());
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(fingerprint.clone());
                    }
                });
            }
        });
        ui.add_space(10.0);
        Self::card(ui, tokens, |ui| {
            ui.label(RichText::new("Live preview").color(tokens.muted).small());
            if let Some(tex) = &self.preview {
                let size = tex.size_vec2();
                ui.image((tex.id(), size));
            } else {
                ui.label("Waiting for the first frame…");
            }
        });
    }

    fn clients_page(&mut self, ui: &mut Ui, tokens: &Tokens) {
        if self.clients.is_empty() {
            ui.label(RichText::new("Nobody is connected.").color(tokens.muted));
            return;
        }
        let rows = self.clients.clone();
        let mut view_only_changes = Vec::new();
        let mut kick = None;
        egui::Grid::new("clients")
            .striped(true)
            .num_columns(9)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                for head in [
                    "Client",
                    "Protection",
                    "Encoding",
                    "In",
                    "Out",
                    "RTT",
                    "Connected",
                    "Input",
                    "",
                ] {
                    ui.label(RichText::new(head).color(tokens.muted).small());
                }
                ui.end_row();
                for c in &rows {
                    ui.label(client_label(c));
                    ui.label(c.encryption);
                    ui.label(if c.updates == 0 {
                        "-"
                    } else {
                        encoding_name(c.encoding)
                    });
                    ui.label(format_bytes(
                        #[allow(clippy::cast_precision_loss)] // a display figure
                        {
                            c.bytes_in as f64
                        },
                    ));
                    ui.label(format_bytes(
                        #[allow(clippy::cast_precision_loss)] // a display figure
                        {
                            c.bytes_out as f64
                        },
                    ));
                    ui.label(
                        c.rtt
                            .map_or_else(|| "-".to_owned(), |d| format!("{} ms", d.as_millis())),
                    );
                    ui.label(format_duration(c.connected_for));
                    let mut view_only = c.view_only;
                    if ui.checkbox(&mut view_only, "View only").changed() {
                        view_only_changes.push((c.id, view_only));
                    }
                    if ui.button("Disconnect").clicked() {
                        kick = Some(c.id);
                    }
                    ui.end_row();
                }
            });
        for (id, v) in view_only_changes {
            self.set_view_only(id, v);
        }
        if let Some(id) = kick {
            self.disconnect(id);
        }
    }

    #[allow(clippy::too_many_lines)] // one page: the list, the form, the actions
    fn users_page(&mut self, ui: &mut Ui, tokens: &Tokens) {
        if let Some(problem) = &self.users.problem {
            ui.label(
                RichText::new(format!("The users file cannot be read: {problem}"))
                    .color(tokens.red),
            );
            return;
        }
        if self.users.list.is_empty() {
            ui.label(
                RichText::new("No users yet. Without users, clients sign in with the shared password or not at all.")
                    .color(tokens.muted),
            );
        }
        let rows = self.users.list.clone();
        let mut actions: Vec<(Vec<String>, Option<String>)> = Vec::new();
        egui::Grid::new("users")
            .striped(true)
            .num_columns(5)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                for head in ["User", "Name", "Role", "Account", ""] {
                    ui.label(RichText::new(head).color(tokens.muted).small());
                }
                ui.end_row();
                for u in &rows {
                    ui.label(&u.name);
                    ui.label(&u.display_name);
                    let mut role = u.role;
                    egui::ComboBox::from_id_salt(("role", &u.name))
                        .selected_text(role.to_string())
                        .show_ui(ui, |ui| {
                            for r in [Role::Admin, Role::Control, Role::View] {
                                ui.selectable_value(&mut role, r, r.to_string());
                            }
                        });
                    if role != u.role {
                        actions.push((vec!["role".into(), u.name.clone(), role.to_string()], None));
                    }
                    let mut enabled = u.enabled;
                    if ui.checkbox(&mut enabled, "Enabled").changed() {
                        let verb = if enabled { "enable" } else { "disable" };
                        actions.push((vec![verb.into(), u.name.clone()], None));
                    }
                    if ui.button(format!("Remove {}", u.name)).clicked() {
                        actions.push((vec!["remove".into(), u.name.clone()], None));
                    }
                    ui.end_row();
                }
            });
        ui.add_space(12.0);
        Self::card(ui, tokens, |ui| {
            ui.label(RichText::new("Add a user").strong());
            egui::Grid::new("add-user")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("User name");
                    ui.text_edit_singleline(&mut self.users.name);
                    ui.end_row();
                    ui.label("Display name");
                    ui.text_edit_singleline(&mut self.users.display_name);
                    ui.end_row();
                    ui.label("Role");
                    egui::ComboBox::from_id_salt("new-role")
                        .selected_text(self.users.role.to_string())
                        .show_ui(ui, |ui| {
                            for r in [Role::Admin, Role::Control, Role::View] {
                                ui.selectable_value(&mut self.users.role, r, r.to_string());
                            }
                        });
                    ui.end_row();
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.users.password).password(true));
                    ui.end_row();
                });
            if ui.button("Add user").clicked() {
                let mut args = vec![
                    "add".to_owned(),
                    self.users.name.clone(),
                    "--role".to_owned(),
                    self.users.role.to_string(),
                ];
                if !self.users.display_name.is_empty() {
                    args.push("--display-name".to_owned());
                    args.push(self.users.display_name.clone());
                }
                actions.push((args, Some(self.users.password.clone())));
            }
        });
        if let Some((ok, text)) = &self.users.message {
            let colour = if *ok { tokens.green } else { tokens.red };
            ui.add_space(6.0);
            ui.label(RichText::new(text).color(colour));
        }
        for (args, password) in actions {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let adding = refs.first() == Some(&"add");
            self.user_command(&refs, password.as_deref());
            if adding && self.users.message.as_ref().is_some_and(|(ok, _)| *ok) {
                self.users.name.clear();
                self.users.display_name.clear();
                self.users.password.clear();
            }
        }
    }

    #[allow(clippy::too_many_lines)] // four cards, one page
    fn security_page(&mut self, ui: &mut Ui, tokens: &Tokens) {
        if let Some(problem) = &self.security.problem {
            ui.label(
                RichText::new(format!("The settings file cannot be read: {problem}"))
                    .color(tokens.red),
            );
            return;
        }
        Self::card(ui, tokens, |ui| {
            ui.label(RichText::new("Right now").color(tokens.muted).small());
            ui.label(&self.status.access);
            ui.label(RichText::new(format!("Listening on {}", self.status.listen)).small());
            for (label, fingerprint) in &self.status.fingerprints {
                ui.label(
                    RichText::new(format!("{label} SHA-256 {fingerprint}"))
                        .monospace()
                        .small(),
                );
            }
        });
        ui.add_space(10.0);
        Self::card(ui, tokens, |ui| {
            ui.label(
                RichText::new("Settings (applied at the next start)")
                    .color(tokens.muted)
                    .small(),
            );
            let mut tls = self.security.edit.tls == TlsMode::SelfSigned;
            if ui
                .checkbox(&mut tls, "Encrypt with a self-signed certificate (TLS)")
                .changed()
            {
                self.security.edit.tls = if tls {
                    TlsMode::SelfSigned
                } else {
                    TlsMode::Off
                };
            }
            ui.checkbox(&mut self.security.edit.ra2, "Offer RSA-AES (needs users)");
            ui.checkbox(&mut self.security.edit.view_only, "View only for everyone");
            ui.horizontal(|ui| {
                ui.label("Listen on");
                ui.text_edit_singleline(&mut self.security.listen_text);
            });
            let listen = self.security.listen_text.trim();
            let beyond_here = listen
                .parse::<std::net::SocketAddr>()
                .is_ok_and(|a| !a.ip().is_loopback());
            if beyond_here && !self.security.has_users {
                ui.label(
                    RichText::new("Reachable from other machines but no users exist: add users first (Users page), or the server will refuse to start.")
                        .color(tokens.amber),
                );
            }
            if ui.button("Save settings").clicked() {
                self.save_security();
            }
        });
        ui.add_space(10.0);
        Self::card(ui, tokens, |ui| {
            ui.label(RichText::new("Keys").color(tokens.muted).small());
            for (which, label, button) in [
                (
                    Forget::Certificate,
                    "TLS certificate",
                    "Make a new certificate",
                ),
                (Forget::Ra2Key, "RSA-AES key", "Make a new RSA-AES key"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    if self.security.confirm == Some(which) {
                        ui.label(
                            RichText::new("Viewers that trusted the old one will warn.")
                                .color(tokens.amber),
                        );
                        if ui.button("Yes, forget it").clicked() {
                            self.forget_key(which);
                        }
                        if ui.button("Cancel").clicked() {
                            self.security.confirm = None;
                        }
                    } else if ui.button(button).clicked() {
                        self.security.confirm = Some(which);
                    }
                });
            }
        });
        ui.add_space(10.0);
        Self::card(ui, tokens, |ui| {
            ui.label(RichText::new("Always on").color(tokens.muted).small());
            for line in [
                "Passwords are stored as Argon2id hashes.",
                "5 wrong passwords in 15 minutes lock a user name for 15 minutes.",
                "An address that keeps failing waits 1 to 30 seconds before its next attempt.",
                "Signing in by name needs TLS or RSA-AES, so passwords are never sent in the clear.",
            ] {
                ui.label(line);
            }
        });
        if let Some((ok, text)) = &self.security.message {
            let colour = if *ok { tokens.green } else { tokens.red };
            ui.add_space(6.0);
            ui.label(RichText::new(text).color(colour));
        }
    }

    fn log_page(&mut self, ui: &mut Ui, tokens: &Tokens) {
        ui.horizontal(|ui| {
            if ui.button("Export as JSON").clicked() {
                self.export_log();
            }
            if ui.button("Clear").clicked() {
                self.log.clear();
            }
        });
        ui.add_space(6.0);
        let lines = self.log.lines();
        if lines.is_empty() {
            ui.label(RichText::new("Nothing has happened yet.").color(tokens.muted));
        }
        for line in lines.iter().rev() {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(clock(line.at))
                        .monospace()
                        .color(tokens.muted),
                );
                let colour = match line.level {
                    Level::Info => tokens.text,
                    Level::Warning => tokens.amber,
                };
                ui.label(RichText::new(&line.text).color(Color32::from(colour)));
            });
        }
    }
}

fn save_png(image: &ColorImage, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
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

/// The names in the theme drop-down.
#[must_use]
pub fn theme_label(t: ThemePreference) -> &'static str {
    match t {
        ThemePreference::Glass => "Glass",
        ThemePreference::Slate => "Slate",
        ThemePreference::Light => "Light",
        ThemePreference::System => "System",
    }
}

impl std::fmt::Debug for Dashboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dashboard")
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl eframe::App for Dashboard {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.show(ctx);
    }
}
