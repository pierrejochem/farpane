//! The service's side of the session worker: a [`Source`] and an [`Input`]
//! that forward to whichever worker process is attached right now.
//!
//! The server engine wants one source for its whole life, but workers come
//! and go (a user logs out, another logs in, a worker crashes). [`Hub`] is the
//! join: the launcher attaches the pipes of each new worker, and the source
//! and input keep working across the changes. While no worker is attached
//! the picture stays as it was and input is dropped.

use std::{
    io::{self, Read, Write},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};

use rfb_codec::Framebuffer;
use rfb_proto::types::Rect;
use rfb_server::{Capture, Input, Source};

use crate::ipc::{self, Command, Frame};

/// Size of the picture before the first worker reports one.
const WAITING_SIZE: (u16, u16) = (1024, 768);

enum Event {
    Frame(Frame),
    /// The worker with this generation closed its pipe or sent garbage.
    Gone(u64),
}

type Writer = Box<dyn Write + Send>;

#[derive(Default)]
struct Link {
    generation: u64,
    writer: Option<Writer>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("generation", &self.generation)
            .field("attached", &self.writer.is_some())
            .finish()
    }
}

/// Joins the launcher to the source and input. Cloning shares it.
#[derive(Clone)]
pub struct Hub {
    link: Arc<Mutex<Link>>,
    events: Sender<Event>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").finish_non_exhaustive()
    }
}

impl Hub {
    /// A hub with the source and input that use it.
    #[must_use]
    pub fn new() -> (Self, RemoteSource, RemoteInput) {
        let (tx, rx) = mpsc::channel();
        let link = Arc::new(Mutex::new(Link::default()));
        let hub = Self {
            link: Arc::clone(&link),
            events: tx,
        };
        let source = RemoteSource {
            events: rx,
            link: Arc::clone(&link),
            size: WAITING_SIZE,
            resized: true,
        };
        (hub, source, RemoteInput { link })
    }

    /// Start using a new worker: frames are read from `from` on a thread of
    /// their own, commands go to `to`. A worker attached earlier is dropped
    /// (its writer closes).
    pub fn attach(&self, mut from: impl Read + Send + 'static, to: impl Write + Send + 'static) {
        let generation = {
            let mut link = self.link.lock().unwrap_or_else(PoisonError::into_inner);
            link.generation += 1;
            link.writer = Some(Box::new(to));
            link.generation
        };
        let events = self.events.clone();
        let link = Arc::clone(&self.link);
        thread::Builder::new()
            .name("farpane-worker-reader".into())
            .spawn(move || {
                // A frame from a worker that was replaced is discarded below.
                while let Ok(Some(frame)) = ipc::read_frame(&mut from) {
                    let current = link
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .generation;
                    if current != generation || events.send(Event::Frame(frame)).is_err() {
                        break;
                    }
                }
                let _ = events.send(Event::Gone(generation));
            })
            .ok();
    }

    /// Stop sending to the current worker (it has exited or is being replaced).
    pub fn detach(&self) {
        let mut link = self.link.lock().unwrap_or_else(PoisonError::into_inner);
        link.generation += 1;
        link.writer = None;
    }

    /// Whether a worker is attached.
    #[must_use]
    pub fn attached(&self) -> bool {
        self.link
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer
            .is_some()
    }
}

/// The screen as the worker reports it.
#[derive(Debug)]
pub struct RemoteSource {
    events: Receiver<Event>,
    link: Arc<Mutex<Link>>,
    size: (u16, u16),
    /// The framebuffer must be brought to `size` on the next capture.
    resized: bool,
}

impl Source for RemoteSource {
    fn size(&self) -> (u16, u16) {
        self.size
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let mut capture = Capture::default();
        let mut whole = false;
        loop {
            let frame = match self.events.try_recv() {
                Ok(Event::Frame(frame)) => frame,
                Ok(Event::Gone(generation)) => {
                    // Keep the last picture until the next worker shows up.
                    let mut link = self.link.lock().unwrap_or_else(PoisonError::into_inner);
                    if link.generation == generation {
                        link.writer = None;
                    }
                    continue;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("the session worker hub is gone"));
                }
            };
            if self.resized || (fb.width(), fb.height()) != (frame.width, frame.height) {
                fb.resize(frame.width, frame.height)
                    .map_err(io::Error::other)?;
                self.size = (frame.width, frame.height);
                self.resized = false;
                whole = true;
            }
            for (rect, pixels) in &frame.rects {
                fb.write_bgrx(*rect, pixels).map_err(io::Error::other)?;
                capture.damage.push(*rect);
            }
            if frame.cursor.is_some() {
                capture.cursor = frame.cursor;
            }
            if frame.clipboard.is_some() {
                capture.clipboard = frame.clipboard;
            }
        }
        if whole {
            capture.damage = vec![Rect {
                x: 0,
                y: 0,
                width: fb.width(),
                height: fb.height(),
            }];
        }
        Ok(capture)
    }
}

/// Input for the attached worker.
#[derive(Debug, Clone)]
pub struct RemoteInput {
    link: Arc<Mutex<Link>>,
}

impl RemoteInput {
    fn send(&self, command: &Command) {
        let Ok(bytes) = command.encode() else { return };
        let mut link = self.link.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(writer) = link.writer.as_mut()
            && ipc::send(writer, &bytes).is_err()
        {
            link.writer = None;
        }
    }
}

impl Input for RemoteInput {
    fn key(&self, keysym: u32, down: bool) {
        self.send(&Command::Key { keysym, down });
    }
    fn pointer(&self, x: u16, y: u16, buttons: u8) {
        self.send(&Command::Pointer { x, y, buttons });
    }
    fn clipboard(&self, text: &str) {
        self.send(&Command::Clipboard(text.to_owned()));
    }
}
