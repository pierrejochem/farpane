//! The server's window: overview, clients, users and the event log
//! (design doc, section 16, "Server dashboard").
//!
//! The window only reads from and acts on a [`Backend`]; the server keeps
//! running without it (`--headless`).

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use egui::{
    Align, Color32, ColorImage, Context, Frame, Layout, Margin, RichText, ScrollArea,
    TextureHandle, TextureOptions, Ui, vec2,
};
use farpane_ui::{
    Accent, ThemePreference, Tokens, style,
    widgets::{self, badge, brand_mark, inset, nav_item, selected},
};
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
    Displays,
    Log,
}

impl Page {
    const ALL: [Self; 6] = [
        Self::Overview,
        Self::Clients,
        Self::Users,
        Self::Security,
        Self::Displays,
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
            "displays" => Some(Self::Displays),
            "log" => Some(Self::Log),
            _ => None,
        }
    }

    fn icon(self) -> farpane_ui::icons::Icon {
        use farpane_ui::icons::Icon;
        match self {
            Self::Overview => Icon::Overview,
            Self::Clients => Icon::Clients,
            Self::Users => Icon::Users,
            Self::Security => Icon::Security,
            Self::Displays => Icon::Displays,
            Self::Log => Icon::Log,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Clients => "Clients",
            Self::Users => "Users",
            Self::Security => "Security",
            Self::Displays => "Displays",
            Self::Log => "Event log",
        }
    }
}

/// How much history the traffic chart shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Range {
    Minute,
    FiveMinutes,
    Hour,
}

impl Range {
    const ALL: [Self; 3] = [Self::Minute, Self::FiveMinutes, Self::Hour];

    /// Seconds, which is also how many points the chart holds.
    fn seconds(self) -> usize {
        match self {
            Self::Minute => 60,
            Self::FiveMinutes => 300,
            Self::Hour => 3600,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Minute => "1 m",
            Self::FiveMinutes => "5 m",
            Self::Hour => "1 h",
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
    in_history: VecDeque<f32>,
    /// Bytes per second out and in, one point per second, for the chart.
    long_out: VecDeque<f32>,
    long_in: VecDeque<f32>,
    /// Sum and count of the samples of the second in progress, and when it began.
    second: (f32, f32, u32, f64),
    range: Range,
    /// The top bar's search: filters the clients table and the event log.
    search: String,
    /// Per client: bytes sent at the last sample and bytes per second since.
    client_rates: HashMap<u64, (u64, f64)>,
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
            in_history: VecDeque::new(),
            long_out: VecDeque::new(),
            long_in: VecDeque::new(),
            second: (0.0, 0.0, 0, 0.0),
            range: Range::FiveMinutes,
            search: String::new(),
            client_rates: HashMap::new(),
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

    /// Start with this theme.
    pub fn set_theme(&mut self, theme: ThemePreference) {
        self.theme = theme;
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
            self.client_rates = self
                .clients
                .iter()
                .map(|c| {
                    let old = self.client_rates.get(&c.id).map_or(c.bytes_out, |r| r.0);
                    (c.id, (c.bytes_out, per_sec(c.bytes_out, old)))
                })
                .collect();
            self.last_sample = now;
            #[allow(clippy::cast_possible_truncation)] // chart values
            {
                self.second.0 += self.rates.1 as f32;
                self.second.1 += self.rates.0 as f32;
                self.second.2 += 1;
            }
            if now - self.second.3 >= 1.0 {
                #[allow(clippy::cast_precision_loss)] // a handful of samples
                let n = self.second.2.max(1) as f32;
                for (history, total) in [
                    (&mut self.long_out, self.second.0),
                    (&mut self.long_in, self.second.1),
                ] {
                    if history.len() >= Range::Hour.seconds() {
                        history.pop_front();
                    }
                    history.push_back(total / n);
                }
                self.second = (0.0, 0.0, 0, now);
            }
            for (history, value) in [
                (&mut self.out_history, self.rates.1),
                (&mut self.in_history, self.rates.0),
            ] {
                if history.len() >= HISTORY_LEN {
                    history.pop_front();
                }
                #[allow(clippy::cast_possible_truncation)] // a chart value
                history.push_back(value as f32);
            }
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
        style::paint_backdrop(ctx, &tokens);

        egui::SidePanel::left("nav")
            .exact_width(268.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::NONE
                    .fill(style::page_fill(&tokens))
                    .inner_margin(Margin {
                        left: 18,
                        right: 0,
                        top: 18,
                        bottom: 18,
                    }),
            )
            .show(ctx, |ui| self.nav(ui, &tokens));
        egui::TopBottomPanel::bottom("notice")
            .frame(
                Frame::NONE
                    .fill(style::page_fill(&tokens))
                    .inner_margin(Margin::symmetric(18, 6)),
            )
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
                    .fill(style::page_fill(&tokens))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                self.top_bar(ui, &tokens);
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = GAP;
                        match self.page {
                            Page::Overview => self.overview(ui, &tokens),
                            Page::Clients => self.clients_card(ui, &tokens),
                            Page::Users => self.users_page(ui, &tokens),
                            Page::Security => self.security_page(ui, &tokens),
                            Page::Displays => self.displays_page(ui, &tokens),
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

    fn top_bar(&mut self, ui: &mut Ui, tokens: &Tokens) {
        let running = self.status.failure.is_none();
        let paused = self.status.paused;
        Self::card_frame(tokens, 16, 12).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(self.page.title())
                        .family(style::semibold(ui.ctx()))
                        .size(18.0),
                );
                let (text, tone) = if !running {
                    ("Stopped", tokens.red)
                } else if paused {
                    ("Paused", tokens.amber)
                } else {
                    ("Sharing", tokens.green)
                };
                badge(ui, text, tone.into());
                // Window width, not what is left: the controls only fit in a wide window.
                let width = ui.ctx().screen_rect().width();
                if width >= 1100.0 {
                    egui::Frame::NONE
                        .fill(inset(tokens))
                        .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
                        .corner_radius(style::radius(10.0))
                        .inner_margin(Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .hint_text("Search clients and events")
                                    .frame(false)
                                    .desired_width(220.0),
                            );
                        });
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.theme_switch(ui, tokens);
                    if running {
                        let label = if paused {
                            "Resume sharing"
                        } else {
                            "Pause sharing"
                        };
                        let button =
                            egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
                                .fill(Color32::from(tokens.accent))
                                .min_size(vec2(0.0, 32.0));
                        if ui.add(button).clicked() {
                            self.backend.set_paused(!paused);
                            self.log.push(
                                Level::Info,
                                if paused {
                                    "sharing resumed from the dashboard"
                                } else {
                                    "sharing paused from the dashboard"
                                },
                            );
                            self.status.paused = !paused;
                        }
                    }
                    if self.page == Page::Overview && width >= 1280.0 {
                        self.range_switch(ui, tokens);
                    }
                });
            });
        });
    }

    /// The time range of the traffic chart as a segmented control.
    fn range_switch(&mut self, ui: &mut Ui, tokens: &Tokens) {
        Frame::NONE
            .fill(inset(tokens))
            .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
            .corner_radius(style::radius(10.0))
            .inner_margin(Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    // Laid out right to left, so listed in reverse.
                    for r in Range::ALL.into_iter().rev() {
                        let on = self.range == r;
                        let colour = if on { tokens.text } else { tokens.muted };
                        let button =
                            egui::Button::new(RichText::new(r.label()).size(12.0).color(colour))
                                .fill(if on {
                                    selected(tokens)
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .stroke(egui::Stroke::NONE)
                                .corner_radius(style::radius(7.0));
                        if ui.add(button).clicked() {
                            self.range = r;
                        }
                    }
                });
            });
    }

    /// The theme choice as one segmented control.
    fn theme_switch(&mut self, ui: &mut Ui, tokens: &Tokens) {
        Frame::NONE
            .fill(inset(tokens))
            .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
            .corner_radius(style::radius(10.0))
            .inner_margin(Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    // Laid out right to left, so listed in reverse.
                    for t in [
                        ThemePreference::System,
                        ThemePreference::Light,
                        ThemePreference::Slate,
                        ThemePreference::Glass,
                    ] {
                        let on = self.theme == t;
                        let colour = if on { tokens.text } else { tokens.muted };
                        let button = egui::Button::new(
                            RichText::new(theme_label(t)).size(12.0).color(colour),
                        )
                        .fill(if on {
                            selected(tokens)
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(style::radius(7.0));
                        if ui.add(button).clicked() {
                            self.theme = t;
                        }
                    }
                });
            });
    }

    fn nav(&mut self, ui: &mut Ui, tokens: &Tokens) {
        Self::card_frame(tokens, 14, 22).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                brand_mark(ui, tokens, 38.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        RichText::new("Farpane Server")
                            .family(style::semibold(ui.ctx()))
                            .size(15.0),
                    );
                    ui.label(
                        RichText::new(env!("CARGO_PKG_VERSION"))
                            .color(tokens.muted)
                            .small(),
                    );
                });
            });
            ui.add_space(14.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            for page in Page::ALL {
                let count = match page {
                    Page::Clients if !self.clients.is_empty() => Some(self.clients.len()),
                    _ => None,
                };
                if nav_item(
                    ui,
                    tokens,
                    self.page == page,
                    page.icon(),
                    page.title(),
                    count,
                ) {
                    self.page = page;
                }
            }
            ui.add_space(14.0);
            ui.label(eyebrow("Listeners", tokens));
            ui.horizontal(|ui| {
                ui.label(RichText::new("RFB").color(tokens.muted).size(12.5));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(&self.status.listen).monospace().size(12.5));
                });
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

    /// A panel as wide as the space it is in.
    fn card<R>(ui: &mut Ui, tokens: &Tokens, add: impl FnOnce(&mut Ui) -> R) -> R {
        Self::card_frame(tokens, 24, 22)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                add(ui)
            })
            .inner
    }

    /// Two columns side by side when there is room, stacked when not.
    fn columns(
        ui: &mut Ui,
        left_share: f32,
        left: impl FnOnce(&mut Ui),
        right: impl FnOnce(&mut Ui),
    ) {
        let total = ui.available_width();
        if total < 760.0 {
            left(ui);
            right(ui);
            return;
        }
        let left_width = ((total - GAP) * left_share).floor();
        let right_width = total - GAP - left_width;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.allocate_ui_with_layout(vec2(left_width, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left_width);
                ui.spacing_mut().item_spacing.y = GAP;
                left(ui);
            });
            ui.allocate_ui_with_layout(
                vec2(right_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(right_width);
                    ui.spacing_mut().item_spacing.y = GAP;
                    right(ui);
                },
            );
        });
    }

    fn overview(&mut self, ui: &mut Ui, tokens: &Tokens) {
        if let Some(why) = &self.status.failure {
            Self::card(ui, tokens, |ui| {
                ui.label(
                    RichText::new(format!("Capture stopped: {why}"))
                        .color(tokens.red)
                        .strong(),
                );
            });
        }
        self.tiles(ui, tokens);
        let change = std::cell::Cell::new(false);
        Self::columns(
            ui,
            0.64,
            |ui| self.traffic_card(ui, tokens),
            |ui| {
                self.sharing_card(ui, tokens);
                self.security_card(ui, tokens, &change);
            },
        );
        if change.get() {
            self.page = Page::Security;
        }
        self.clients_card(ui, tokens);
        // The preview stays out of the columns closure so the card can borrow
        // the texture while the log reads the shared state.
        Self::columns(
            ui,
            0.5,
            |ui| {
                Self::card(ui, tokens, |ui| {
                    ui.label(heading(ui, "Live preview"));
                    ui.add_space(8.0);
                    if let Some(tex) = &self.preview {
                        let size = tex.size_vec2();
                        let scale = (ui.available_width() / size.x).min(1.0);
                        ui.image((tex.id(), size * scale));
                    } else {
                        ui.label("Waiting for the first frame…");
                    }
                });
            },
            |ui| {
                Self::card(ui, tokens, |ui| {
                    ui.label(heading(ui, "Recent events"));
                    ui.add_space(8.0);
                    self.log_lines(ui, tokens, Some(6));
                });
            },
        );
    }

    fn tiles(&self, ui: &mut Ui, tokens: &Tokens) {
        let viewing = self.clients.iter().filter(|c| c.view_only).count();
        let peak = self.out_history.iter().copied().fold(0.0_f32, f32::max);
        let (out_value, out_unit) = split_rate(self.rates.1);
        let (in_value, in_unit) = split_rate(self.rates.0);
        let worst = self.clients.iter().filter_map(|c| c.rtt).max();
        let (rtt_value, rtt_unit, rtt_note) = worst.map_or_else(
            || ("n/a".to_owned(), "", "no Fence yet".to_owned()),
            |d| (d.as_millis().to_string(), "ms", "slowest client".to_owned()),
        );
        let tiles = [
            Tile {
                label: "Connected clients",
                value: self.clients.len().to_string(),
                unit: "now".into(),
                note: format!("{viewing} view only"),
                foot: String::new(),
                tone: tokens.blue,
            },
            Tile {
                label: "Outgoing traffic",
                value: out_value,
                unit: out_unit,
                note: format!("peak {}", format_rate(f64::from(peak))),
                foot: "recent".into(),
                tone: tokens.green,
            },
            Tile {
                label: "Incoming input",
                value: in_value,
                unit: in_unit,
                note: "from viewers".into(),
                foot: String::new(),
                tone: tokens.violet,
            },
            Tile {
                label: "Round trip",
                value: rtt_value,
                unit: rtt_unit.into(),
                note: rtt_note,
                foot: String::new(),
                tone: tokens.amber,
            },
        ];
        let total = ui.available_width();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1 to 4
        let per_row = (((total + GAP) / (170.0 + GAP)).floor() as usize).clamp(1, tiles.len());
        #[allow(clippy::cast_precision_loss)] // at most 4
        let width = ((total - GAP * (per_row - 1) as f32) / per_row as f32).floor();
        for row in tiles.chunks(per_row) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for tile in row {
                    ui.allocate_ui_with_layout(
                        vec2(width, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| tile.show(ui, tokens, width),
                    );
                }
            });
        }
    }

    fn traffic_card(&self, ui: &mut Ui, tokens: &Tokens) {
        Self::card(ui, tokens, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(heading(ui, "Traffic"));
                    ui.label(
                        RichText::new("Outgoing frames vs. incoming input")
                            .color(tokens.muted)
                            .size(12.5),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    legend(ui, tokens, tokens.blue.into(), "Incoming");
                    legend(ui, tokens, tokens.accent.into(), "Outgoing");
                });
            });
            ui.add_space(8.0);
            let span = self.range.seconds();
            let last = |h: &VecDeque<f32>| -> Vec<f32> {
                h.iter()
                    .skip(h.len().saturating_sub(span))
                    .copied()
                    .collect()
            };
            let out = last(&self.long_out);
            let incoming = last(&self.long_in);
            widgets::area_chart(
                ui,
                vec2(ui.available_width(), 190.0),
                &[
                    (&incoming, tokens.blue.into()),
                    (&out, tokens.accent.into()),
                ],
                span,
                16_384.0,
                Color32::from(tokens.line),
            );
            ui.add_space(6.0);
            ui.separator();
            let peak = out.iter().copied().fold(0.0_f32, f32::max);
            #[allow(clippy::cast_precision_loss)] // a short history
            let average = out.iter().sum::<f32>() / out.len().max(1) as f32;
            #[allow(clippy::cast_precision_loss)] // a display figure
            let sent = self.clients.iter().map(|c| c.bytes_out).sum::<u64>() as f64;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for (key, value) in [
                    ("Peak", format_rate(f64::from(peak))),
                    ("Average", format_rate(f64::from(average))),
                    ("Sent", format_bytes(sent)),
                ] {
                    ui.label(RichText::new(key).color(tokens.muted).size(12.5));
                    ui.label(RichText::new(value).strong().size(12.5));
                }
                let (text, tone) = if self.status.failure.is_none() {
                    ("healthy", tokens.green)
                } else {
                    ("stopped", tokens.red)
                };
                ui.label(RichText::new("Capture").color(tokens.muted).size(12.5));
                ui.label(RichText::new(text).strong().size(12.5).color(tone));
            });
        });
    }

    /// Whether `text` matches the top bar's search (always, when it is empty).
    fn matches_search(&self, text: &str) -> bool {
        let needle = self.search.trim().to_lowercase();
        needle.is_empty() || text.to_lowercase().contains(&needle)
    }

    /// The Displays page: what is shared. One display for now.
    fn displays_page(&mut self, ui: &mut Ui, tokens: &Tokens) {
        let (w, h) = self.status.screen;
        let running = self.status.failure.is_none();
        Self::card(ui, tokens, |ui| {
            ui.label(heading(ui, "Shared display"));
            ui.label(
                RichText::new("Farpane shares one display; several screens are not built yet.")
                    .color(tokens.muted)
                    .size(12.5),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(44.0, 34.0), egui::Sense::hover());
                ui.painter().rect_stroke(
                    rect,
                    style::radius(6.0),
                    egui::Stroke::new(2.0_f32, Color32::from(tokens.accent_text)),
                    egui::StrokeKind::Inside,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(&self.status.source).strong());
                    ui.label(
                        RichText::new(format!("{w} × {h}"))
                            .color(tokens.muted)
                            .size(12.5),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (text, tone) = if !running {
                        ("Stopped", tokens.red)
                    } else if self.status.paused {
                        ("Paused", tokens.amber)
                    } else {
                        ("Sharing", tokens.green)
                    };
                    badge(ui, text, tone.into());
                });
            });
            if let Some(why) = &self.status.failure {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("Capture stopped: {why}")).color(tokens.red));
            }
        });
    }

    /// The ways to sign in, as read-only switches; they change on the
    /// Security page and take effect at the next start.
    fn security_card(&self, ui: &mut Ui, tokens: &Tokens, change: &std::cell::Cell<bool>) {
        Self::card(ui, tokens, |ui| {
            ui.label(heading(ui, "Security"));
            ui.label(
                RichText::new("Offered sign-in methods")
                    .color(tokens.muted)
                    .size(12.5),
            );
            ui.add_space(8.0);
            for (name, note, on) in &self.status.offered {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(name).strong().size(13.5));
                        ui.label(RichText::new(note).color(tokens.muted).size(11.5));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (text, tone) = if *on {
                            ("On", tokens.green)
                        } else {
                            ("Off", tokens.muted)
                        };
                        badge(ui, text, tone.into());
                    });
                });
                ui.add_space(4.0);
            }
            ui.add_space(4.0);
            if ui.button("Change on the Security page").clicked() {
                change.set(true);
            }
        });
    }

    fn sharing_card(&self, ui: &mut Ui, tokens: &Tokens) {
        Self::card(ui, tokens, |ui| {
            ui.label(heading(ui, "Shared screen"));
            let (w, h) = self.status.screen;
            ui.label(
                RichText::new(format!("{} ({w} × {h})", self.status.source))
                    .color(tokens.muted)
                    .size(12.5),
            );
            ui.add_space(8.0);
            ui.label(eyebrow("Sign-in and protection", tokens));
            ui.label(RichText::new(&self.status.access).strong());
            for (label, fingerprint) in &self.status.fingerprints {
                ui.add_space(6.0);
                ui.label(eyebrow(&format!("{label} SHA-256"), tokens));
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(fingerprint).monospace().size(11.5));
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(fingerprint.clone());
                    }
                });
            }
        });
    }

    #[allow(clippy::too_many_lines)] // filter, table and actions in one card
    fn clients_card(&mut self, ui: &mut Ui, tokens: &Tokens) {
        let all = self.clients.len();
        let rows: Vec<_> = self
            .clients
            .iter()
            .filter(|c| {
                self.matches_search(&format!(
                    "{} {} {}",
                    client_label(c),
                    c.encryption,
                    encoding_name(c.encoding)
                ))
            })
            .cloned()
            .collect();
        let mut view_only_changes = Vec::new();
        let mut kick = None;
        Self::card(ui, tokens, |ui| {
            ui.label(heading(ui, "Connected clients"));
            ui.add_space(4.0);
            if rows.is_empty() {
                let text = if all == 0 {
                    "Nobody is connected."
                } else {
                    "No client matches the search."
                };
                ui.label(RichText::new(text).color(tokens.muted));
                return;
            }
            let top = self
                .client_rates
                .values()
                .map(|r| r.1)
                .fold(1.0_f64, f64::max);
            ScrollArea::horizontal().show(ui, |ui| {
                egui::Grid::new("clients")
                    .num_columns(8)
                    .spacing([16.0, 14.0])
                    .show(ui, |ui| {
                        for head in [
                            "Client",
                            "Protection",
                            "Encoding",
                            "Bitrate",
                            "RTT",
                            "Connected",
                            "Input",
                            "",
                        ] {
                            ui.label(eyebrow(head, tokens));
                        }
                        ui.end_row();
                        for c in &rows {
                            ui.horizontal(|ui| {
                                avatar(ui, tokens, c);
                                ui.label(RichText::new(client_label(c)).strong());
                            });
                            let tone = if c.encryption == "none" {
                                tokens.red
                            } else {
                                tokens.green
                            };
                            ui.vertical(|ui| {
                                ui.add_space(2.0);
                                badge(ui, c.encryption, tone.into());
                            });
                            ui.label(
                                RichText::new(if c.updates == 0 {
                                    "-"
                                } else {
                                    encoding_name(c.encoding)
                                })
                                .monospace(),
                            );
                            let rate = self.client_rates.get(&c.id).map_or(0.0, |r| r.1);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(RichText::new(format_rate(rate)).size(12.5));
                                bar(ui, tokens, rate / top);
                            });
                            ui.label(c.rtt.map_or_else(
                                || "-".to_owned(),
                                |d| format!("{} ms", d.as_millis()),
                            ));
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
            });
        });
        for (id, v) in view_only_changes {
            self.set_view_only(id, v);
        }
        if let Some(id) = kick {
            self.disconnect(id);
        }
    }

    /// The newest `limit` log lines (all when `None`), newest first.
    fn log_lines(&self, ui: &mut Ui, tokens: &Tokens, limit: Option<usize>) {
        let lines = self.log.lines();
        if lines.is_empty() {
            ui.label(RichText::new("Nothing has happened yet.").color(tokens.muted));
        }
        for line in lines
            .iter()
            .rev()
            .filter(|l| self.matches_search(&l.text))
            .take(limit.unwrap_or(usize::MAX))
        {
            let tone = match line.level {
                Level::Info => tokens.green,
                Level::Warning => tokens.amber,
            };
            ui.horizontal(|ui| {
                chip(ui, tokens, tone.into(), 24.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(&line.text).size(13.0));
                    ui.label(RichText::new(clock(line.at)).color(tokens.muted).size(11.5));
                });
            });
            ui.add_space(6.0);
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
        Self::card(ui, tokens, |ui| {
            ui.horizontal(|ui| {
                ui.label(heading(ui, "Event log"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Clear").clicked() {
                        self.log.clear();
                    }
                    if ui.button("Export as JSON").clicked() {
                        self.export_log();
                    }
                });
            });
            ui.add_space(10.0);
            self.log_lines(ui, tokens, None);
        });
    }
}

/// Space between panels.
const GAP: f32 = 18.0;

/// One figure on the overview.
struct Tile {
    label: &'static str,
    value: String,
    unit: String,
    note: String,
    foot: String,
    tone: farpane_ui::theme::Rgba,
}

impl Tile {
    fn show(&self, ui: &mut Ui, tokens: &Tokens, width: f32) {
        Dashboard::card_frame(tokens, 20, 16).show(ui, |ui| {
            ui.set_width(width - 42.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new(self.label).color(tokens.muted).size(12.5));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    chip(ui, tokens, self.tone.into(), 28.0);
                });
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(&self.value)
                        .family(style::semibold(ui.ctx()))
                        .size(26.0),
                );
                ui.label(RichText::new(&self.unit).color(tokens.muted).size(13.0));
            });
            ui.horizontal_wrapped(|ui| {
                badge(ui, &self.note, self.tone.into());
                if !self.foot.is_empty() {
                    ui.label(RichText::new(&self.foot).color(tokens.muted).size(12.0));
                }
            });
        });
    }
}

fn heading(ui: &Ui, text: &str) -> RichText {
    RichText::new(text)
        .family(style::semibold(ui.ctx()))
        .size(16.0)
}

fn eyebrow(text: &str, tokens: &Tokens) -> RichText {
    RichText::new(text.to_uppercase())
        .color(tokens.muted)
        .size(10.5)
}

/// A rounded square with a dot in `tone`.
fn chip(ui: &mut Ui, tokens: &Tokens, tone: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        style::radius(size * 0.3),
        tone.gamma_multiply(0.14),
        egui::Stroke::new(1.0_f32, tone.gamma_multiply(0.32)),
        egui::StrokeKind::Inside,
    );
    painter.circle_filled(rect.center(), size * 0.14, tone);
    let _ = tokens;
}

/// The legend entry of a chart series.
fn legend(ui: &mut Ui, tokens: &Tokens, colour: Color32, text: &str) {
    ui.label(RichText::new(text).color(tokens.muted).size(12.5));
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, colour);
}

/// A horizontal bar filled to `fraction` (0 to 1).
fn bar(ui: &mut Ui, tokens: &Tokens, fraction: f64) {
    let (rect, _) = ui.allocate_exact_size(vec2(90.0, 6.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, style::radius(3.0), inset(tokens));
    #[allow(clippy::cast_possible_truncation)] // a pixel width
    let filled = (rect.width() * fraction.clamp(0.0, 1.0) as f32).max(0.0);
    if filled > 0.5 {
        let mut fill = rect;
        fill.set_width(filled);
        ui.painter()
            .rect_filled(fill, style::radius(3.0), Color32::from(tokens.blue));
    }
}

/// The client's initials on a coloured square.
fn avatar(ui: &mut Ui, tokens: &Tokens, c: &ClientInfo) {
    let name = c.user.as_deref().unwrap_or("?");
    let initials: String = name.chars().take(2).flat_map(char::to_uppercase).collect();
    let palette = [
        tokens.violet,
        tokens.blue,
        tokens.amber,
        tokens.green,
        tokens.accent,
    ];
    let colour = palette[usize::try_from(c.id).unwrap_or(0) % palette.len()];
    let (rect, _) = ui.allocate_exact_size(vec2(30.0, 30.0), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        style::radius(9.0),
        Color32::from(colour).gamma_multiply(0.85),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initials,
        egui::FontId::proportional(11.5),
        Color32::from(tokens.bg),
    );
}

/// `1.5 MB/s` as `("1.5", "MB/s")`.
fn split_rate(bytes_per_sec: f64) -> (String, String) {
    let text = format_rate(bytes_per_sec);
    match text.rsplit_once(' ') {
        Some((value, unit)) => (value.to_owned(), unit.to_owned()),
        None => (text, String::new()),
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
