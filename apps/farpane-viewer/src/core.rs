//! The connection runs on its own thread with a Tokio runtime; the UI thread
//! talks to it through two channels and never blocks on the network.

use std::{
    sync::mpsc::{Receiver, Sender, channel},
    thread::JoinHandle,
};

use rfb_codec::{Cursor, Damage};
use rfb_proto::types::Rect;
use rfb_security::tls::{CertProblem, Fingerprint};
use rfb_tokio::Error as SessionError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{
    ClipboardSent, ConnectError, ConnectOptions, RemoteScreen, ScreenEvent,
    screen::DEFAULT_ENCODINGS, stats::Snapshot,
};

/// How often the UI gets a statistics reading.
const STATS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// How often a Fence round trip is measured (17 bytes each way).
const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// The H.264 codec to use for the next connection, or none. Equal when it is
/// the same codec.
#[derive(Debug, Clone, Default)]
pub struct Video(pub Option<std::sync::Arc<dyn rfb_codec::video::VideoCodec>>);

impl PartialEq for Video {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}

impl Eq for Video {}

/// What the UI asks the connection to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Use this H.264 codec for the connections that follow (none: no H.264).
    SetVideo(Video),
    Connect {
        addr: String,
        password: Option<Vec<u8>>,
        /// For `VeNCrypt` `X509Plain`.
        username: Option<Vec<u8>>,
        /// Certificate fingerprints trusted for this server.
        pins: Vec<Fingerprint>,
    },
    Disconnect,
    /// Ask the server to resize the remote desktop.
    Resize {
        width: u16,
        height: u16,
    },
    Key {
        keysym: u32,
        down: bool,
    },
    Pointer {
        buttons: u8,
        x: u16,
        y: u16,
    },
    CutText(Vec<u8>),
    /// Local clipboard text to share: UTF-8 if the server supports it.
    Clipboard(String),
}

/// What the connection tells the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    Connecting,
    Connected {
        name: String,
        width: u16,
        height: u16,
        /// `RFB 3.8`, and how the session is secured.
        protocol: String,
        security: String,
    },
    /// New pixels for `rect`, as `R, G, B, 0xFF` rows.
    Frame {
        rect: Rect,
        rgba: Vec<u8>,
    },
    /// The remote screen changed size; a full `Frame` follows.
    Resized {
        width: u16,
        height: u16,
    },
    /// The server's cursor shape (empty pixels mean "hidden").
    Cursor(Cursor),
    /// The server announced its screen layout (it supports resizing). When
    /// `reason` is 1 this answers a [`Command::Resize`]; a `status` other
    /// than 0 then means the request was refused.
    Layout {
        reason: u16,
        status: u16,
        width: u16,
        height: u16,
    },
    /// A statistics reading, about four times a second while connected.
    Stats(Snapshot),
    Bell,
    /// Clipboard text from the server (Latin-1 bytes).
    CutText(Vec<u8>),
    /// Clipboard text from the server (UTF-8, Extended Clipboard).
    ClipboardText(String),
    /// Something the user should know about, such as clipboard text that
    /// could not be sent in full.
    Notice(String),
    /// The server's TLS certificate is not trusted. Nothing was sent
    /// besides the handshake. Ask the user; if they accept, connect again with
    /// the fingerprint in `pins`.
    Untrusted {
        addr: String,
        host: String,
        fingerprint: Fingerprint,
        problem: CertProblem,
    },
    /// The connection ended; `Some` carries the reason for an error.
    Disconnected(Option<String>),
}

/// Handle to the connection thread. Dropping it stops the thread.
#[derive(Debug)]
pub struct Core {
    commands: UnboundedSender<Command>,
    events: Receiver<UiEvent>,
    thread: Option<JoinHandle<()>>,
}

impl Core {
    /// Start the thread. `wake` is called after every event is queued (the
    /// UI uses it to request a repaint).
    ///
    /// # Panics
    /// If the OS cannot spawn a thread or build a Tokio runtime.
    #[must_use]
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (event_tx, events) = channel();
        let thread = std::thread::Builder::new()
            .name("farpane-connection".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                runtime.block_on(run(
                    command_rx,
                    Notifier {
                        tx: event_tx,
                        wake: Box::new(wake),
                    },
                ));
            })
            .expect("spawn connection thread");
        Self {
            commands,
            events,
            thread: Some(thread),
        }
    }

    /// Queue a command. Commands are dropped silently once the thread is gone.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// The next pending event, if any.
    #[must_use]
    pub fn try_recv(&self) -> Option<UiEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // Closing the command channel ends the loop.
        let (closed, _) = unbounded_channel();
        self.commands = closed;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Notifier {
    tx: Sender<UiEvent>,
    wake: Box<dyn Fn() + Send>,
}

impl Notifier {
    fn emit(&self, event: UiEvent) {
        let _ = self.tx.send(event);
        (self.wake)();
    }
}

async fn run(mut commands: UnboundedReceiver<Command>, ui: Notifier) {
    let mut video: Option<std::sync::Arc<dyn rfb_codec::video::VideoCodec>> = None;
    loop {
        // Idle: wait for a Connect; other commands have nothing to act on.
        let (addr, options) = loop {
            match commands.recv().await {
                Some(Command::SetVideo(v)) => video = v.0,
                Some(Command::Connect {
                    addr,
                    password,
                    username,
                    pins,
                }) => {
                    let options = ConnectOptions {
                        password,
                        username,
                        pins,
                        video: video.clone(),
                        ..ConnectOptions::default()
                    };
                    break (addr, options);
                }
                Some(_) => {}
                None => return,
            }
        };
        ui.emit(UiEvent::Connecting);
        let connect = RemoteScreen::connect_opts(&addr, &options, &DEFAULT_ENCODINGS);
        tokio::pin!(connect);
        // `Err(None)` means the user cancelled while connecting.
        let screen: Result<RemoteScreen, Option<crate::ConnectError>> = loop {
            tokio::select! {
                result = &mut connect => break result.map_err(Some),
                command = commands.recv() => match command {
                    Some(Command::Disconnect) => break Err(None),
                    Some(_) => {}
                    None => return,
                },
            }
        };
        match screen {
            Ok(screen) => {
                if let Some(reason) = session(screen, &mut commands, &ui).await {
                    ui.emit(UiEvent::Disconnected(reason));
                }
            }
            Err(None) => ui.emit(UiEvent::Disconnected(None)),
            Err(Some(ConnectError::Session(SessionError::Untrusted {
                host,
                fingerprint,
                problem,
            }))) => ui.emit(UiEvent::Untrusted {
                addr: addr.clone(),
                host,
                fingerprint,
                problem,
            }),
            Err(Some(e)) => ui.emit(UiEvent::Disconnected(Some(e.to_string()))),
        }
        if commands.is_closed() {
            return;
        }
    }
}

/// Drive one connected session. Returns `Some(reason)` when it ended and the
/// UI should hear about it, or `None` if the command channel closed.
async fn session(
    mut screen: RemoteScreen,
    commands: &mut UnboundedReceiver<Command>,
    ui: &Notifier,
) -> Option<Option<String>> {
    let (w, h) = (screen.framebuffer().width(), screen.framebuffer().height());
    ui.emit(UiEvent::Connected {
        name: screen.name().to_owned(),
        width: w,
        height: h,
        protocol: screen.protocol(),
        security: screen.security(),
    });
    // Readings four times a second (design doc, section 16); a slow UI thread
    // makes the interval skip, not pile up.
    let mut ticker = tokio::time::interval(STATS_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pinger = tokio::time::interval(PING_INTERVAL);
    pinger.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result: Result<(), String> = async {
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    ui.emit(UiEvent::Stats(screen.sample_stats(std::time::Instant::now())));
                }
                _ = pinger.tick() => {
                    screen.ping().await.map_err(|e| e.to_string())?;
                }
                event = screen.next_event() => {
                    handle_event(&mut screen, event.map_err(|e| e.to_string())?, ui)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                command = commands.recv() => {
                    let Some(command) = command else { return Err(String::new()) };
                    match command {
                        // Read when the next connection starts.
                        Command::SetVideo(_) | Command::Connect { .. } => {}
                        Command::Disconnect => return Ok(()),
                        Command::Resize { width, height } => {
                            screen.request_size(width, height).await.map_err(|e| e.to_string())?;
                        }
                        Command::Key { keysym, down } => {
                            screen.key(keysym, down).await.map_err(|e| e.to_string())?;
                        }
                        Command::Pointer { buttons, x, y } => {
                            screen.pointer(buttons, x, y).await.map_err(|e| e.to_string())?;
                        }
                        Command::CutText(text) => {
                            screen.cut_text(text).await.map_err(|e| e.to_string())?;
                        }
                        Command::Clipboard(text) => {
                            let sent = screen
                                .send_clipboard(&text)
                                .await
                                .map_err(|e| e.to_string())?;
                            match sent {
                                ClipboardSent::Utf8 | ClipboardSent::Latin1 { replaced: 0 } => {}
                                ClipboardSent::Latin1 { replaced } => {
                                    ui.emit(UiEvent::Notice(format!(
                                        "{replaced} character(s) outside Latin-1 were sent as “?”"
                                    )));
                                }
                                ClipboardSent::TooLarge { limit } => {
                                    ui.emit(UiEvent::Notice(format!(
                                        "Clipboard text is too large to send (limit {} MiB)",
                                        limit / (1024 * 1024)
                                    )));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    .await;
    match result {
        Ok(()) => Some(None),
        // An empty error means the command channel closed: shut down quietly.
        Err(reason) if reason.is_empty() => None,
        Err(reason) => Some(Some(reason)),
    }
}

async fn handle_event(
    screen: &mut RemoteScreen,
    event: ScreenEvent,
    ui: &Notifier,
) -> Result<(), crate::ConnectError> {
    match event {
        ScreenEvent::Damage(Damage::Rect(rect)) => {
            if let Ok(rgba) = screen.framebuffer().rgba(rect) {
                ui.emit(UiEvent::Frame { rect, rgba });
            }
        }
        ScreenEvent::Damage(Damage::Resized) => {
            let fb = screen.framebuffer();
            let (width, height) = (fb.width(), fb.height());
            ui.emit(UiEvent::Resized { width, height });
            let full = Rect {
                x: 0,
                y: 0,
                width,
                height,
            };
            if let Ok(rgba) = fb.rgba(full) {
                ui.emit(UiEvent::Frame { rect: full, rgba });
            }
        }
        ScreenEvent::Damage(Damage::None | Damage::Cursor(_) | Damage::Layout(_)) => {}
        ScreenEvent::Layout(layout) => {
            let fb = screen.framebuffer();
            let (width, height) = (fb.width(), fb.height());
            if layout.resized {
                ui.emit(UiEvent::Resized { width, height });
                let full = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                if let Ok(rgba) = fb.rgba(full) {
                    ui.emit(UiEvent::Frame { rect: full, rgba });
                }
            }
            ui.emit(UiEvent::Layout {
                reason: layout.reason,
                status: layout.status,
                width: layout.width,
                height: layout.height,
            });
        }
        ScreenEvent::Cursor(cursor) => ui.emit(UiEvent::Cursor(cursor)),
        ScreenEvent::UpdateEnd => screen.request_update(true).await?,
        ScreenEvent::Bell => ui.emit(UiEvent::Bell),
        ScreenEvent::CutText(text) => ui.emit(UiEvent::CutText(text)),
        ScreenEvent::Text(text) => ui.emit(UiEvent::ClipboardText(text)),
    }
    Ok(())
}
