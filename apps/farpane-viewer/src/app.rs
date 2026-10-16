//! The egui window: home screen, session canvas, top bar.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use egui::{
    Align, Color32, ColorImage, Context, CursorIcon, EventFilter, Frame, Key, Layout, Margin,
    RichText, Sense, TextEdit, TextureHandle, TextureOptions, UserData, ViewportCommand, vec2,
};
use farpane_ui::{Accent, ThemePreference, Tokens, style};
use rfb_proto::types::Rect;

use crate::{
    core::{Command, Core, UiEvent},
    input::{Canvas, KeyTranslator, PointerTranslator},
};

/// What the command line asked for.
#[derive(Debug, Default, Clone)]
pub struct Startup {
    /// Prefill the address, and connect right away.
    pub addr: Option<String>,
    pub password: Option<Vec<u8>>,
    /// Save a screenshot of the window to this file and quit (for testing).
    pub ui_screenshot: Option<PathBuf>,
}

#[derive(Debug)]
enum Phase {
    Home,
    Connecting,
    Connected { name: String },
}

/// The viewer window.
pub struct ViewerApp {
    core: Core,
    phase: Phase,
    addr: String,
    password: String,
    error: Option<String>,
    theme: ThemePreference,
    applied: Option<Tokens>,
    one_to_one: bool,
    texture: Option<TextureHandle>,
    remote_size: (u16, u16),
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
        let app = Self {
            core,
            phase: Phase::Home,
            addr: startup.addr.clone().unwrap_or_default(),
            password: String::new(),
            error: None,
            theme: ThemePreference::default(),
            applied: None,
            one_to_one: false,
            texture: None,
            remote_size: (0, 0),
            keys: KeyTranslator::default(),
            pointer: PointerTranslator::default(),
            last_remote_pos: (0, 0),
            startup_password: startup.password,
            ui_screenshot: startup.ui_screenshot,
            started: Instant::now(),
            connected_at: None,
            screenshot_requested: false,
        };
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

    /// Draw one frame: handle connection events, draw, forward input.
    pub fn ui(&mut self, ctx: &Context) {
        let tokens = self.apply_theme(ctx);
        self.drain_events(ctx);
        let title = match &self.phase {
            Phase::Connected { name } => format!("{name} — Farpane Viewer"),
            _ => "Farpane Viewer".to_owned(),
        };
        ctx.send_viewport_cmd(ViewportCommand::Title(title));
        self.top_bar(ctx, &tokens);
        match self.phase {
            Phase::Connected { .. } => self.session(ctx),
            Phase::Home | Phase::Connecting => self.home(ctx, &tokens),
        }
        self.screenshot_driver(ctx);
    }

    fn connect(&self) {
        let password = (!self.password.is_empty()).then(|| self.password.clone().into_bytes());
        self.connect_with(password);
    }

    fn connect_with(&self, password: Option<Vec<u8>>) {
        self.core.send(Command::Connect {
            addr: self.addr.trim().to_owned(),
            password,
        });
    }

    fn tokens(&self, ctx: &Context) -> Tokens {
        let os_dark = ctx.input(|i| i.raw.system_theme) != Some(egui::Theme::Light);
        Tokens::new(self.theme.resolve(os_dark), Accent::default())
    }

    /// Apply the theme to egui whenever the tokens change.
    fn apply_theme(&mut self, ctx: &Context) -> Tokens {
        let tokens = self.tokens(ctx);
        if self.applied != Some(tokens) {
            style::apply(ctx, &tokens);
            self.applied = Some(tokens);
        }
        tokens
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
                } => {
                    self.phase = Phase::Connected { name };
                    self.new_texture(ctx, width, height);
                    self.connected_at = Some(Instant::now());
                }
                UiEvent::Resized { width, height } => self.new_texture(ctx, width, height),
                UiEvent::Frame { rect, rgba } => self.upload(rect, &rgba),
                UiEvent::Bell | UiEvent::CutText(_) => {}
                UiEvent::Disconnected(reason) => {
                    self.phase = Phase::Home;
                    self.error = reason;
                    self.texture = None;
                    self.connected_at = None;
                    self.release_input();
                }
            }
        }
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

    fn top_bar(&mut self, ctx: &Context, tokens: &Tokens) {
        egui::TopBottomPanel::top("top-bar")
            .frame(
                Frame::NONE
                    .fill(tokens.card.into())
                    .inner_margin(Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Farpane").strong().size(16.0));
                    ui.add_space(8.0);
                    let (text, colour) = match &self.phase {
                        Phase::Home => ("Not connected", tokens.muted),
                        Phase::Connecting => ("Connecting…", tokens.amber),
                        Phase::Connected { .. } => ("Connected", tokens.green),
                    };
                    // A drawn dot: egui's default font has no "●" glyph.
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, colour);
                    ui.label(RichText::new(text).color(colour));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("theme")
                            .selected_text(theme_name(self.theme))
                            .show_ui(ui, |ui| {
                                for t in [
                                    ThemePreference::Glass,
                                    ThemePreference::Slate,
                                    ThemePreference::Light,
                                    ThemePreference::System,
                                ] {
                                    ui.selectable_value(&mut self.theme, t, theme_name(t));
                                }
                            });
                        ui.label(RichText::new("Theme").color(tokens.muted));
                        if matches!(self.phase, Phase::Connected { .. }) {
                            ui.add_space(12.0);
                            if ui.button("Disconnect").clicked() {
                                self.core.send(Command::Disconnect);
                            }
                            let fullscreen =
                                ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                            if ui
                                .button(if fullscreen {
                                    "Exit full screen"
                                } else {
                                    "Full screen"
                                })
                                .clicked()
                            {
                                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
                            }
                            let label = if self.one_to_one {
                                "Fit to window"
                            } else {
                                "Actual size"
                            };
                            if ui.button(label).clicked() {
                                self.one_to_one = !self.one_to_one;
                            }
                        }
                    });
                });
            });
    }

    fn home(&mut self, ctx: &Context, tokens: &Tokens) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.22).max(24.0));
                Frame::NONE
                    .fill(tokens.card.into())
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from(tokens.line)))
                    .corner_radius(style::radius(tokens.radius))
                    .inner_margin(Margin::same(28))
                    .show(ui, |ui| {
                        ui.set_width(380.0);
                        ui.label(
                            RichText::new("Connect to a remote desktop")
                                .size(22.0)
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(RichText::new("Any VNC-compatible server").color(tokens.muted));
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
                            RichText::new("Password (if the server asks)").color(tokens.muted),
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
        });
    }

    fn session(&mut self, ctx: &Context) {
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(Color32::BLACK))
            .show(ctx, |ui| {
                let available = ui.available_rect_before_wrap();
                let (rect, response) =
                    ui.allocate_exact_size(available.size(), Sense::click_and_drag());
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
                }
                if response.hovered() {
                    ctx.set_cursor_icon(CursorIcon::Default);
                }
                self.forward_input(ctx, &response, &canvas);
            });
    }

    fn forward_input(&mut self, ctx: &Context, response: &egui::Response, canvas: &Canvas) {
        let (events, mods, pos, focused) = ctx.input(|i| {
            (
                i.events.clone(),
                i.modifiers,
                i.pointer.hover_pos(),
                i.focused,
            )
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

fn theme_name(t: ThemePreference) -> &'static str {
    match t {
        ThemePreference::Glass => "Glass",
        ThemePreference::Slate => "Slate",
        ThemePreference::Light => "Light",
        ThemePreference::System => "System",
    }
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
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}
