//! Wayland backend for `rfb-server`: the screen through the xdg-desktop-portal
//! `ScreenCast` interface and `PipeWire`, keyboard and pointer input through
//! the `RemoteDesktop` portal (design doc, section 19, "Linux Wayland,
//! physical screen").
//!
//! Opening a session makes the desktop show an approval dialog; nothing is
//! shared until somebody agrees. The approval can be remembered with a
//! restore token (kept in a file), so later starts need no click while the
//! user stays logged in.
//!
//! The portal hands out one monitor (or window) stream; its pixels arrive in
//! frames, and damage is found by comparing each frame with the previous one.
//! The pointer is drawn into the frames.
//!
//! Linux only: on other systems this crate is empty. Building it needs the
//! `PipeWire` development files (`libpipewire-0.3-dev`), `pkg-config` and
//! `clang`.
#![cfg(target_os = "linux")]

mod capture;

use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

use ashpd::desktop::{
    PersistMode, Session,
    clipboard::{Clipboard, RequestClipboardOptions, SetSelectionOptions},
    remote_desktop::{
        Axis, DeviceType, KeyState, NotifyKeyboardKeysymOptions, NotifyPointerAxisDiscreteOptions,
        NotifyPointerButtonOptions, NotifyPointerMotionAbsoluteOptions, RemoteDesktop,
        SelectDevicesOptions, StartOptions,
    },
    screencast::{
        CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
    },
};
use capture::{Frame, Shared};
use futures_util::StreamExt as _;
use rfb_codec::Framebuffer;
use rfb_proto::types::Rect;
use rfb_server::{Capture, Input, Source, changed_tiles};
use tokio::{runtime::Runtime, sync::mpsc};

/// Linux input event codes for the pointer buttons (`linux/input-event-codes.h`).
const BTN_LEFT: i32 = 0x110;
const BTN_RIGHT: i32 = 0x111;
const BTN_MIDDLE: i32 = 0x112;

/// How long the approval dialog may stay unanswered.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(180);
/// How long to wait for the first frame once approved.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest clipboard text moved either way.
const MAX_CLIPBOARD: u64 = 4 * 1024 * 1024;
/// How long a program may take to hand over or take the clipboard text.
const CLIPBOARD_IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Text types, best first, offered and accepted for the clipboard.
const TEXT_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];

/// Settings for [`open`].
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Where the portal's restore token is kept, so an approval is
    /// remembered. `None` asks every time.
    pub restore_token_file: Option<PathBuf>,
    /// X11 id of a window of this program (through `XWayland`) for the
    /// approval dialog to belong to. Without one, some desktops leave the
    /// dialog unmapped.
    pub parent_window: Option<std::os::raw::c_ulong>,
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// What the negotiation with the portal produced.
struct Negotiated {
    remote_desktop: RemoteDesktop,
    /// `None` when the desktop has no clipboard portal.
    clipboard: Option<Clipboard>,
    session: Session<RemoteDesktop>,
    fd: std::os::fd::OwnedFd,
    node_id: u32,
}

#[allow(clippy::default_trait_access)] // the options type is not exported by ashpd
async fn negotiate(config: &Config) -> Result<Negotiated, io::Error> {
    let remote_desktop = RemoteDesktop::new().await.map_err(io_err)?;
    let screencast = Screencast::new().await.map_err(io_err)?;
    let session = remote_desktop
        .create_session(Default::default())
        .await
        .map_err(io_err)?;
    let parent = config.parent_window.map(ashpd::WindowIdentifier::from_xid);
    let token = config
        .restore_token_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    remote_desktop
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await
        .map_err(io_err)?;
    // The clipboard must be requested before the session starts. A desktop
    // without it still shares the screen.
    let clipboard = match Clipboard::new().await {
        Ok(c) => c
            .request(&session, RequestClipboardOptions::default())
            .await
            .ok()
            .map(|()| c),
        Err(_) => None,
    };
    screencast
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
                .set_multiple(false),
        )
        .await
        .map_err(io_err)?;
    let started = remote_desktop
        .start(&session, parent.as_ref(), StartOptions::default())
        .await
        .map_err(io_err)?
        .response()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("not approved: {e}"),
            )
        })?;
    if let (Some(path), Some(new)) = (&config.restore_token_file, started.restore_token()) {
        // The token is a secret that skips the dialog: keep it private.
        write_private(path, new.as_bytes())?;
    }
    let stream = started
        .streams()
        .first()
        .ok_or_else(|| io::Error::other("the portal did not share a screen"))?;
    let node_id = stream.pipe_wire_node_id();
    let fd = screencast
        .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
        .await
        .map_err(io_err)?;
    Ok(Negotiated {
        remote_desktop,
        clipboard,
        session,
        fd,
        node_id,
    })
}

fn write_private(path: &std::path::Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    // `create_new` refuses a leftover file or a planted symlink, which
    // would keep its own (possibly wider) permissions.
    let _ = std::fs::remove_file(&tmp);
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// The portal session and the threads behind a source and its input; ends
/// them when both are dropped.
struct Backing {
    runtime: Option<Runtime>,
    quit: pipewire::channel::Sender<()>,
}

impl Drop for Backing {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        // This may run inside another runtime's task, where dropping a
        // runtime outright would panic.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// The first frame tells the size.
fn wait_for_first_frame(shared: &Shared) -> io::Result<(u32, u32)> {
    let started = Instant::now();
    loop {
        {
            let frame = shared
                .frame
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if frame.seq > 0 {
                return Ok((frame.width, frame.height));
            }
        }
        if shared.ended.load(Ordering::Relaxed) {
            let why = shared
                .error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .unwrap_or_else(|| "the stream ended".to_owned());
            return Err(io::Error::other(why));
        }
        if started.elapsed() > FIRST_FRAME_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no frame arrived from the portal",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Ask the desktop for permission to share the screen (a dialog the user
/// must answer, unless a remembered approval applies) and open the stream.
///
/// # Errors
/// The portal is missing, the user said no or did not answer in time, or no
/// frame arrived.
pub fn open(config: &Config) -> io::Result<(WaylandSource, WaylandInput)> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let negotiated = runtime.block_on(async {
        tokio::time::timeout(APPROVAL_TIMEOUT, negotiate(config))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the approval dialog was not answered",
                )
            })?
    })?;
    let Negotiated {
        remote_desktop,
        clipboard,
        session,
        fd,
        node_id,
    } = negotiated;

    let shared = Arc::new(Shared::default());
    let (quit, quit_rx) = pipewire::channel::channel::<()>();
    let thread_shared = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("farpane-pipewire".into())
        .spawn(move || capture::run(fd, node_id, &thread_shared, quit_rx))?;

    let (width, height) = wait_for_first_frame(&shared)?;

    let (events, queue) = mpsc::unbounded_channel::<InputEvent>();
    let failure = Arc::new(std::sync::Mutex::new(None::<String>));
    let task_failure = Arc::clone(&failure);
    let outgoing = Arc::new(Mutex::new(None::<String>));
    let has_clipboard = clipboard.is_some();
    let session = Arc::new(session);
    let clipboard = clipboard.map(Arc::new);
    if let Some(clipboard) = &clipboard {
        runtime.spawn(watch_desktop_clipboard(
            Arc::clone(clipboard),
            Arc::clone(&shared),
            Arc::clone(&outgoing),
        ));
        runtime.spawn(serve_clipboard(
            Arc::clone(clipboard),
            Arc::clone(&session),
            Arc::clone(&outgoing),
        ));
    }
    runtime.spawn(input_loop(
        queue,
        Portal {
            remote_desktop,
            session,
            clipboard,
            node_id,
        },
        outgoing,
        task_failure,
    ));
    let backing = Arc::new(Backing {
        runtime: Some(runtime),
        quit,
    });
    let width = u16::try_from(width).map_err(|_| io_err("the screen is too wide"))?;
    let height = u16::try_from(height).map_err(|_| io_err("the screen is too tall"))?;
    Ok((
        WaylandSource {
            shared,
            size: (width, height),
            last_seq: 0,
            scratch: Vec::new(),
            _backing: Arc::clone(&backing),
        },
        WaylandInput {
            events,
            failure,
            has_clipboard,
            _backing: backing,
        },
    ))
}

/// What the input task talks to.
struct Portal {
    remote_desktop: RemoteDesktop,
    session: Arc<Session<RemoteDesktop>>,
    clipboard: Option<Arc<Clipboard>>,
    node_id: u32,
}

/// Deliver queued input events to the portal, in order.
async fn input_loop(
    mut queue: mpsc::UnboundedReceiver<InputEvent>,
    portal: Portal,
    outgoing: Arc<Mutex<Option<String>>>,
    failure: Arc<Mutex<Option<String>>>,
) {
    let Portal {
        remote_desktop,
        session,
        clipboard,
        node_id,
    } = portal;
    let mut buttons = 0u8;
    while let Some(event) = queue.recv().await {
        // A failed call means the session is gone; the capture side ends
        // the server when its stream stops.
        let sent = match event {
            InputEvent::Clipboard(text) => match &clipboard {
                Some(clipboard) => offer_clipboard(clipboard, &session, &outgoing, text).await,
                None => Ok(()),
            },
            event => deliver(&remote_desktop, &session, node_id, &mut buttons, event).await,
        };
        if let Err(e) = sent {
            // Keep the first failure for `WaylandInput::first_error`.
            let mut slot = failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slot.get_or_insert_with(|| e.to_string());
        }
    }
    let _ = session.close().await;
}

#[derive(Debug)]
enum InputEvent {
    Key { keysym: u32, down: bool },
    Clipboard(String),
    Pointer { x: u16, y: u16, buttons: u8 },
}

async fn deliver(
    rd: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    node: u32,
    held: &mut u8,
    event: InputEvent,
) -> ashpd::Result<()> {
    match event {
        // Handled by the caller, which owns the clipboard portal.
        InputEvent::Clipboard(_) => Ok(()),
        InputEvent::Key { keysym, down } => {
            let state = if down {
                KeyState::Pressed
            } else {
                KeyState::Released
            };
            // Keysyms are 32 bit values that fit the portal's signed int.
            let keysym = i32::try_from(keysym).unwrap_or(0);
            rd.notify_keyboard_keysym(
                session,
                keysym,
                state,
                NotifyKeyboardKeysymOptions::default(),
            )
            .await
        }
        InputEvent::Pointer { x, y, buttons } => {
            rd.notify_pointer_motion_absolute(
                session,
                node,
                f64::from(x),
                f64::from(y),
                NotifyPointerMotionAbsoluteOptions::default(),
            )
            .await?;
            // RFB bits: 0 left, 1 middle, 2 right are held states; 3 and 4
            // (wheel up and down) and 5 and 6 (left and right) are clicks.
            for (bit, code) in [(0u8, BTN_LEFT), (1, BTN_MIDDLE), (2, BTN_RIGHT)] {
                let (was, is) = (*held >> bit & 1 == 1, buttons >> bit & 1 == 1);
                if was != is {
                    let state = if is {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    };
                    rd.notify_pointer_button(
                        session,
                        code,
                        state,
                        NotifyPointerButtonOptions::default(),
                    )
                    .await?;
                }
            }
            for (bit, axis, steps) in [
                (3u8, Axis::Vertical, -1),
                (4, Axis::Vertical, 1),
                (5, Axis::Horizontal, -1),
                (6, Axis::Horizontal, 1),
            ] {
                if buttons >> bit & 1 == 1 && *held >> bit & 1 == 0 {
                    rd.notify_pointer_axis_discrete(
                        session,
                        axis,
                        steps,
                        NotifyPointerAxisDiscreteOptions::default(),
                    )
                    .await?;
                }
            }
            *held = buttons;
            Ok(())
        }
    }
}

/// Make the desktop's clipboard ours: a client copied `text`. The text is kept
/// until a program pastes it (see [`serve_clipboard`]).
async fn offer_clipboard(
    clipboard: &Clipboard,
    session: &Session<RemoteDesktop>,
    outgoing: &Mutex<Option<String>>,
    text: String,
) -> ashpd::Result<()> {
    if text.is_empty() || text.len() as u64 > MAX_CLIPBOARD {
        return Ok(());
    }
    *outgoing
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(text);
    clipboard
        .set_selection(
            session,
            SetSelectionOptions::default().set_mime_types(&TEXT_TYPES),
        )
        .await
}

/// Hand the text a client copied to whichever program pastes it.
async fn serve_clipboard(
    clipboard: Arc<Clipboard>,
    session: Arc<Session<RemoteDesktop>>,
    outgoing: Arc<Mutex<Option<String>>>,
) {
    let Ok(transfers) = clipboard
        .receive_selection_transfer::<RemoteDesktop>()
        .await
    else {
        return;
    };
    let mut transfers = std::pin::pin!(transfers);
    while let Some((_, mime, serial)) = transfers.next().await {
        let text = outgoing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let sent = match text {
            Some(text) if TEXT_TYPES.contains(&mime.as_str()) => {
                write_clipboard(&clipboard, &session, serial, text).await
            }
            _ => false,
        };
        let _ = clipboard.selection_write_done(&session, serial, sent).await;
    }
}

async fn write_clipboard(
    clipboard: &Clipboard,
    session: &Session<RemoteDesktop>,
    serial: u32,
    text: String,
) -> bool {
    use tokio::io::AsyncWriteExt as _;
    let Ok(fd) = clipboard.selection_write(session, serial).await else {
        return false;
    };
    let mut file = tokio::fs::File::from_std(std::fs::File::from(std::os::fd::OwnedFd::from(fd)));
    let work = async {
        file.write_all(text.as_bytes()).await?;
        file.shutdown().await
    };
    matches!(
        tokio::time::timeout(CLIPBOARD_IO_TIMEOUT, work).await,
        Ok(Ok(()))
    )
}

/// Pass text that another program copied on the desktop on to the source.
async fn watch_desktop_clipboard(
    clipboard: Arc<Clipboard>,
    shared: Arc<Shared>,
    outgoing: Arc<Mutex<Option<String>>>,
) {
    use tokio::io::AsyncReadExt as _;
    let Ok(changes) = clipboard
        .receive_selection_owner_changed::<RemoteDesktop>()
        .await
    else {
        return;
    };
    let mut changes = std::pin::pin!(changes);
    while let Some((session, change)) = changes.next().await {
        // Our own SetSelection comes back as a change too.
        if change.session_is_owner() == Some(true) {
            continue;
        }
        let Some(mime) = TEXT_TYPES
            .iter()
            .find(|t| change.mime_types().iter().any(|m| m == *t))
        else {
            continue;
        };
        let Ok(fd) = clipboard.selection_read(&session, mime).await else {
            continue;
        };
        let file = tokio::fs::File::from_std(std::fs::File::from(std::os::fd::OwnedFd::from(fd)));
        let mut bytes = Vec::new();
        let mut limited = file.take(MAX_CLIPBOARD + 1);
        let read = limited.read_to_end(&mut bytes);
        let Ok(Ok(_)) = tokio::time::timeout(CLIPBOARD_IO_TIMEOUT, read).await else {
            continue;
        };
        if bytes.is_empty() || bytes.len() as u64 > MAX_CLIPBOARD {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // Text a client just sent us is not news.
        let ours = outgoing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_deref()
            == Some(text.as_str());
        if !ours {
            *shared
                .clipboard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(text);
        }
    }
}

/// The captured screen.
pub struct WaylandSource {
    shared: Arc<Shared>,
    size: (u16, u16),
    last_seq: u64,
    /// The newest frame converted to `B, G, R, X`.
    scratch: Vec<u8>,
    _backing: Arc<Backing>,
}

impl std::fmt::Debug for WaylandSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaylandSource")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Source for WaylandSource {
    fn size(&self) -> (u16, u16) {
        self.size
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let clipboard = self
            .shared
            .clipboard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let frame: Frame = {
            let guard = self
                .shared
                .frame
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.seq == self.last_seq {
                if self.shared.ended.load(Ordering::Relaxed) {
                    let why = self
                        .shared
                        .error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                        .unwrap_or_else(|| "the screen stream ended".to_owned());
                    return Err(io::Error::other(why));
                }
                return Ok(Capture {
                    clipboard,
                    ..Capture::default()
                });
            }
            guard.clone()
        };
        self.last_seq = frame.seq;
        let (Some(layout), Ok(w), Ok(h)) = (
            frame.layout,
            u16::try_from(frame.width),
            u16::try_from(frame.height),
        ) else {
            return Ok(Capture {
                clipboard,
                ..Capture::default()
            });
        };
        self.scratch.clear();
        layout.to_bgrx(&frame.data, &mut self.scratch);
        let whole = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        if (w, h) != (fb.width(), fb.height()) {
            fb.resize(w, h).map_err(io_err)?;
            self.size = (w, h);
            fb.write_bgrx(whole, &self.scratch).map_err(io_err)?;
            return Ok(Capture {
                damage: vec![whole],
                clipboard,
                ..Capture::default()
            });
        }
        let damage = changed_tiles(fb, &self.scratch, usize::from(w), usize::from(h));
        for rect in &damage {
            let mut part =
                Vec::with_capacity(usize::from(rect.width) * usize::from(rect.height) * 4);
            for row in 0..usize::from(rect.height) {
                let at = ((usize::from(rect.y) + row) * usize::from(w) + usize::from(rect.x)) * 4;
                part.extend_from_slice(&self.scratch[at..at + usize::from(rect.width) * 4]);
            }
            fb.write_bgrx(*rect, &part).map_err(io_err)?;
        }
        Ok(Capture {
            damage,
            clipboard,
            ..Capture::default()
        })
    }
}

/// Keyboard and pointer input through the portal.
pub struct WaylandInput {
    events: mpsc::UnboundedSender<InputEvent>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
    has_clipboard: bool,
    _backing: Arc<Backing>,
}

impl std::fmt::Debug for WaylandInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaylandInput").finish_non_exhaustive()
    }
}

impl WaylandInput {
    /// Whether the desktop's portal shares the clipboard.
    #[must_use]
    pub fn has_clipboard(&self) -> bool {
        self.has_clipboard
    }

    /// The first error the portal returned for an input event, if any (the
    /// events are delivered in the background, so this is the way to find out).
    #[must_use]
    pub fn first_error(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Input for WaylandInput {
    fn key(&self, keysym: u32, down: bool) {
        let _ = self.events.send(InputEvent::Key { keysym, down });
    }

    fn pointer(&self, x: u16, y: u16, buttons: u8) {
        let _ = self.events.send(InputEvent::Pointer { x, y, buttons });
    }

    fn clipboard(&self, text: &str) {
        if self.has_clipboard {
            let _ = self.events.send(InputEvent::Clipboard(text.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use rfb_codec::Decoder;
    use rfb_proto::{PixelFormat, consts::enc, msg::server::RectHeader};

    use super::*;

    fn picture(w: u16, h: u16, f: impl Fn(u16, u16) -> [u8; 4]) -> (Framebuffer, Vec<u8>) {
        let mut wire = Vec::new();
        for y in 0..h {
            for x in 0..w {
                wire.extend_from_slice(&f(x, y));
            }
        }
        let mut fb = Framebuffer::new(w, h).unwrap();
        let rect = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        Decoder::new(&PixelFormat::RGB888)
            .unwrap()
            .decode(
                &mut fb,
                &RectHeader {
                    rect,
                    encoding: enc::RAW,
                },
                &wire,
            )
            .unwrap();
        (fb, wire)
    }

    #[test]
    fn only_the_tiles_that_changed_are_reported() {
        let base = |x: u16, y: u16| [(x % 251) as u8, (y % 241) as u8, 7, 0];
        let (fb, mut new) = picture(150, 70, base);
        assert_eq!(changed_tiles(&fb, &new, 150, 70), []);
        // One pixel in the third tile of the first row and one in the last tile.
        new[((5 * 150) + 130) * 4] ^= 0x55;
        new[((69 * 150) + 149) * 4 + 2] ^= 0x01;
        let tiles = changed_tiles(&fb, &new, 150, 70);
        assert_eq!(
            tiles,
            [
                Rect {
                    x: 128,
                    y: 0,
                    width: 22,
                    height: 64
                },
                Rect {
                    x: 128,
                    y: 64,
                    width: 22,
                    height: 6
                },
            ]
        );
    }

    #[test]
    fn pixel_layouts_convert_to_bgrx() {
        use capture::Layout;
        let mut out = Vec::new();
        Layout::Rgbx.to_bgrx(&[1, 2, 3, 0, 4, 5, 6, 0], &mut out);
        assert_eq!(out, [3, 2, 1, 255, 6, 5, 4, 255]);
        out.clear();
        Layout::Rgb.to_bgrx(&[1, 2, 3], &mut out);
        assert_eq!(out, [3, 2, 1, 255]);
        out.clear();
        Layout::Bgr.to_bgrx(&[1, 2, 3], &mut out);
        assert_eq!(out, [1, 2, 3, 255]);
        out.clear();
        Layout::Bgrx.to_bgrx(&[1, 2, 3, 9], &mut out);
        assert_eq!(out, [1, 2, 3, 9]);
        assert_eq!(Layout::Bgr.bytes_per_pixel(), 3);
    }
}
