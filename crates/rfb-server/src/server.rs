use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rfb_codec::{Cursor, Framebuffer};
use rfb_proto::types::Rect;
use rfb_tokio::{Error, ServerOptions, ServerSession};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
    sync::broadcast,
};

use crate::{
    client,
    source::{Input, Source},
    throttle::Throttle,
};

/// What the capture thread tells the clients.
#[derive(Debug, Clone)]
pub(crate) struct Update {
    pub rects: Vec<Rect>,
    /// The pointer has a new shape.
    pub cursor: Option<Arc<Cursor>>,
    /// New text on the server's clipboard.
    pub clipboard: Option<Arc<str>>,
    /// Capturing has stopped for good; clients should disconnect.
    pub ended: bool,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub fb: RwLock<Framebuffer>,
    pub updates: broadcast::Sender<Arc<Update>>,
    /// The pointer's current shape, for clients that join later.
    pub cursor: RwLock<Option<Arc<Cursor>>>,
    pub input: Arc<dyn Input>,
    stop: AtomicBool,
    /// Why capturing stopped, if it did.
    failure: Mutex<Option<String>>,
    clients: AtomicUsize,
}

/// Server behaviour that is not about one connection.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Capture frequency.
    pub fps: u32,
    /// Ignore keyboard, pointer and clipboard input from clients.
    pub view_only: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 30,
            view_only: false,
        }
    }
}

/// A running server: the capture thread and the shared picture. Dropping it
/// stops capturing; running connections end.
pub struct Server {
    shared: Arc<Shared>,
    template: ServerOptions,
    view_only: bool,
    policy: Option<Policy>,
    events: Option<Sink>,
    throttle: Mutex<Throttle>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("view_only", &self.view_only)
            .field("clients", &self.client_count())
            .finish_non_exhaustive()
    }
}

type Sink = Arc<dyn Fn(&Event) + Send + Sync>;

type Policy = Arc<dyn Fn(Option<&str>) -> Permission + Send + Sync>;

/// What a signed-in user may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Look and control.
    Full,
    /// Look only: keyboard, pointer and clipboard input is dropped.
    ViewOnly,
}

/// Something worth logging, from [`Server::set_events`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A client signed in. `user` is the account name for password logins
    /// with a user name, `None` for the shared-password and no-password types.
    Connected {
        peer: Option<SocketAddr>,
        user: Option<String>,
        permission: Permission,
    },
    /// A client's session ended; `error` says why if it was not a plain hang-up.
    Closed {
        peer: Option<SocketAddr>,
        user: Option<String>,
        error: Option<String>,
    },
    /// Wrong or missing credentials.
    AuthFailed { peer: Option<SocketAddr> },
    /// The handshake failed for another reason (timeout, bad protocol, TLS).
    HandshakeFailed {
        peer: Option<SocketAddr>,
        error: String,
    },
}

impl Server {
    /// Take a first frame from `source` and start capturing.
    ///
    /// `template` gives the authentication, TLS and name; the size is taken
    /// from the screen for every connection.
    ///
    /// # Errors
    /// The first capture failing, or a screen too large for the protocol.
    pub fn new(
        mut source: impl Source,
        input: impl Input,
        template: ServerOptions,
        settings: Settings,
    ) -> io::Result<Self> {
        let (w, h) = source.size();
        let mut fb = Framebuffer::new(w, h).map_err(io::Error::other)?;
        let first = source.capture(&mut fb)?;
        let (updates, _) = broadcast::channel(64);
        let shared = Arc::new(Shared {
            fb: RwLock::new(fb),
            updates,
            cursor: RwLock::new(first.cursor.map(Arc::new)),
            input: Arc::new(input),
            stop: AtomicBool::new(false),
            failure: Mutex::new(None),
            clients: AtomicUsize::new(0),
        });
        let weak = Arc::clone(&shared);
        let interval = Duration::from_secs(1) / settings.fps.clamp(1, 240);
        thread::Builder::new()
            .name("farpane-capture".into())
            .spawn(move || capture_loop(source, &weak, interval))?;
        Ok(Self {
            shared,
            template,
            view_only: settings.view_only,
            policy: None,
            events: None,
            throttle: Mutex::new(Throttle::new()),
        })
    }

    /// Decide per user what a signed-in client may do. Without a policy every
    /// client gets [`Permission::Full`] (unless the server is view-only).
    pub fn set_policy(
        &mut self,
        policy: impl Fn(Option<&str>) -> Permission + Send + Sync + 'static,
    ) {
        self.policy = Some(Arc::new(policy));
    }

    /// Receive [`Event`]s: sign-ins, hang-ups, failures.
    pub fn set_events(&mut self, sink: impl Fn(&Event) + Send + Sync + 'static) {
        self.events = Some(Arc::new(sink));
    }

    fn emit(&self, event: &Event) {
        if let Some(sink) = &self.events {
            sink(event);
        }
    }

    /// The picture size now.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let fb = self
            .shared
            .fb
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (fb.width(), fb.height())
    }

    /// Why capturing stopped, if it did. Clients are disconnected then.
    #[must_use]
    pub fn capture_failure(&self) -> Option<String> {
        self.shared
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Clients currently past the handshake.
    #[must_use]
    pub fn client_count(&self) -> usize {
        self.shared.clients.load(Ordering::Relaxed)
    }

    /// Serve one connection until it ends. Same as [`Server::serve`] without
    /// a peer address (no per-address delays, events carry no address).
    ///
    /// # Errors
    /// Handshake and protocol errors; [`Error::Closed`] when the client hangs up.
    pub async fn serve_connection<S>(&self, stream: S) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.serve(stream, None).await
    }

    /// Serve one connection from `peer` until it ends. A peer that has
    /// failed to sign in recently waits before its handshake starts.
    ///
    /// # Errors
    /// Handshake and protocol errors; [`Error::Closed`] when the client hangs up.
    pub async fn serve<S>(&self, stream: S, peer: Option<SocketAddr>) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        if let Some(p) = peer {
            let wait = self
                .throttle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .delay(p.ip(), Instant::now());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
        let (w, h) = self.size();
        let mut opts = self.template.clone();
        opts.width = w;
        opts.height = h;
        let session = match ServerSession::accept(stream, opts).await {
            Ok(s) => s,
            Err(e) => {
                if let Some(p) = peer {
                    if matches!(e, Error::AuthFailed) {
                        self.throttle
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .record_failure(p.ip(), Instant::now());
                    }
                }
                if matches!(e, Error::AuthFailed) {
                    self.emit(&Event::AuthFailed { peer });
                } else if !is_hang_up(&e) {
                    self.emit(&Event::HandshakeFailed {
                        peer,
                        error: e.to_string(),
                    });
                }
                return Err(e);
            }
        };
        if let Some(p) = peer {
            self.throttle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_success(p.ip());
        }
        let user = session
            .username()
            .map(|u| String::from_utf8_lossy(u).into_owned());
        let permission = if self.view_only {
            Permission::ViewOnly
        } else {
            self.policy
                .as_ref()
                .map_or(Permission::Full, |f| f(user.as_deref()))
        };
        self.emit(&Event::Connected {
            peer,
            user: user.clone(),
            permission,
        });
        self.shared.clients.fetch_add(1, Ordering::Relaxed);
        let result = client::run(
            session,
            Arc::clone(&self.shared),
            permission == Permission::ViewOnly,
        )
        .await;
        self.shared.clients.fetch_sub(1, Ordering::Relaxed);
        self.emit(&Event::Closed {
            peer,
            user,
            error: match &result {
                Err(e) if !is_hang_up(e) => Some(e.to_string()),
                _ => None,
            },
        });
        result
    }

    /// Accept connections on `listener` for ever, one task each. Use
    /// [`Server::set_events`] to hear about them.
    ///
    /// # Errors
    /// The listener failing.
    pub async fn listen(self: Arc<Self>, listener: TcpListener) -> io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                // Everything worth reporting went to the event sink.
                let _ = server.serve(stream, Some(peer)).await;
            });
        }
    }
}

/// A client going away is normal, not an error to report.
fn is_hang_up(e: &Error) -> bool {
    matches!(e, Error::Closed)
        || matches!(e, Error::Io(io) if matches!(
            io.kind(),
            io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::UnexpectedEof
        ))
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

fn capture_loop(mut source: impl Source, shared: &Arc<Shared>, interval: Duration) {
    // The only strong holders besides this thread are the `Server` and its
    // connections; `stop` is set when the `Server` goes away.
    while !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(interval);
        let result = {
            let mut fb = shared
                .fb
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let before = (fb.width(), fb.height());
            let result = source.capture(&mut fb);
            let after = (fb.width(), fb.height());
            // A resize damages the whole picture whatever the source said.
            result.map(|mut c| {
                if before != after {
                    c.damage = vec![Rect {
                        x: 0,
                        y: 0,
                        width: after.0,
                        height: after.1,
                    }];
                }
                c
            })
        };
        match result {
            Ok(capture)
                if capture.damage.is_empty()
                    && capture.cursor.is_none()
                    && capture.clipboard.is_none() => {}
            Ok(capture) => {
                let cursor = capture.cursor.map(Arc::new);
                if let Some(c) = &cursor {
                    *shared
                        .cursor
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(c));
                }
                // No receivers just means nobody is connected.
                let _ = shared.updates.send(Arc::new(Update {
                    rects: capture.damage,
                    cursor,
                    clipboard: capture.clipboard.map(Arc::from),
                    ended: false,
                }));
            }
            Err(e) => {
                *shared
                    .failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e.to_string());
                let _ = shared.updates.send(Arc::new(Update {
                    rects: Vec::new(),
                    cursor: None,
                    clipboard: None,
                    ended: true,
                }));
                return;
            }
        }
    }
}
