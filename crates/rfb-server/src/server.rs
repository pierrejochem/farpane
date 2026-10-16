use std::{
    io,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use rfb_codec::Framebuffer;
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
};

/// What the capture thread tells the clients.
#[derive(Debug, Clone)]
pub(crate) struct Update {
    pub rects: Vec<Rect>,
    /// Capturing has stopped for good; clients should disconnect.
    pub ended: bool,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub fb: RwLock<Framebuffer>,
    pub updates: broadcast::Sender<Arc<Update>>,
    pub input: Arc<dyn Input>,
    pub view_only: bool,
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
#[derive(Debug)]
pub struct Server {
    shared: Arc<Shared>,
    template: ServerOptions,
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
        source.capture(&mut fb)?;
        let (updates, _) = broadcast::channel(64);
        let shared = Arc::new(Shared {
            fb: RwLock::new(fb),
            updates,
            input: Arc::new(input),
            view_only: settings.view_only,
            stop: AtomicBool::new(false),
            failure: Mutex::new(None),
            clients: AtomicUsize::new(0),
        });
        let weak = Arc::clone(&shared);
        let interval = Duration::from_secs(1) / settings.fps.clamp(1, 240);
        thread::Builder::new()
            .name("farpane-capture".into())
            .spawn(move || capture_loop(source, &weak, interval))?;
        Ok(Self { shared, template })
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

    /// Serve one connection until it ends.
    ///
    /// # Errors
    /// Handshake and protocol errors; [`Error::Closed`] when the client hangs up.
    pub async fn serve_connection<S>(&self, stream: S) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (w, h) = self.size();
        let mut opts = self.template.clone();
        opts.width = w;
        opts.height = h;
        let session = ServerSession::accept(stream, opts).await?;
        self.shared.clients.fetch_add(1, Ordering::Relaxed);
        let result = client::run(session, Arc::clone(&self.shared)).await;
        self.shared.clients.fetch_sub(1, Ordering::Relaxed);
        result
    }

    /// Accept connections on `listener` for ever, one task each. Errors of
    /// single connections are passed to `on_error` with the peer address.
    ///
    /// # Errors
    /// The listener failing.
    pub async fn listen(
        self: Arc<Self>,
        listener: TcpListener,
        on_error: impl Fn(std::net::SocketAddr, &Error) + Send + Sync + 'static,
    ) -> io::Result<()> {
        let on_error = Arc::new(on_error);
        loop {
            let (stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            let server = Arc::clone(&self);
            let on_error = Arc::clone(&on_error);
            tokio::spawn(async move {
                if let Err(e) = server.serve_connection(stream).await {
                    // A client going away is normal, not an error to report.
                    let hung_up = matches!(e, Error::Closed)
                        || matches!(&e, Error::Io(io) if matches!(
                            io.kind(),
                            io::ErrorKind::ConnectionReset
                                | io::ErrorKind::BrokenPipe
                                | io::ErrorKind::UnexpectedEof
                        ));
                    if !hung_up {
                        on_error(peer, &e);
                    }
                }
            });
        }
    }
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
            Ok(capture) if capture.damage.is_empty() => {}
            Ok(capture) => {
                // No receivers just means nobody is connected.
                let _ = shared.updates.send(Arc::new(Update {
                    rects: capture.damage,
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
                    ended: true,
                }));
                return;
            }
        }
    }
}
