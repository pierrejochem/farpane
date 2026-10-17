//! One client connection: its pending changes, encoder state and requests.

use std::sync::{Arc, atomic::Ordering};

use rfb_codec::{
    Cursor, PixelPacker, TIGHT_MAX_WIDTH, TightEncoder, ZrleEncoder,
    clipboard::{ExtClipboard, Role},
    encode_cursor, encode_hextile, encode_raw, encode_trle,
};
use rfb_proto::{
    conn::EncodedRect,
    consts::{enc, fence, pseudo},
    msg::{
        client::ClientMessage,
        server::{RectHeader, ServerMessage},
    },
    types::{Rect, Screen},
};
use rfb_tokio::{Error, ServerSession};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::broadcast::error::RecvError,
};

use crate::{
    region::{self, Region},
    registry::ClientHandle,
    server::Shared,
};

/// Updates that may be unconfirmed before the next one waits.
const MAX_IN_FLIGHT: usize = 2;
/// How long an unanswered fence counts before it is given up.
const FENCE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// `ExtendedDesktopSize` reasons and results (rfbproto.rst).
const REASON_SERVER: u16 = 0;
const REASON_CLIENT: u16 = 1;
const STATUS_OK: u16 = 0;
const STATUS_PROHIBITED: u16 = 1;

/// A single update that takes longer than this to write ends the connection.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[allow(clippy::struct_excessive_bools)] // independent one-shot flags
struct State {
    /// Live state shared with the dashboard (view-only switch, counters).
    handle: Arc<ClientHandle>,
    dirty: Region,
    /// The area of the outstanding update request, if any.
    request: Option<Rect>,
    zrle: ZrleEncoder,
    tight: TightEncoder,
    clipboard: ExtClipboard,
    /// Whether the client has been told the layout (`ExtendedDesktopSize`).
    layout_told: bool,
    /// A reply to a `SetDesktopSize`, sent with the next update.
    layout_reply: Option<(u16, u16)>,
    /// The client asked for the `Cursor` pseudo-encoding.
    wants_cursor: bool,
    /// The client asked for `CopyRect`.
    wants_copy: bool,
    /// Scrolls to send as copies with the next update, in order.
    copies: Vec<crate::scroll::Copy>,
    /// A cursor shape not yet sent.
    cursor: Option<Arc<Cursor>>,
    announced_clipboard: bool,
    announced_fence: bool,
    /// Fence requests sent after updates and not yet answered: the number
    /// carried in the payload and when it was sent. They pace the updates.
    in_flight: std::collections::VecDeque<(u32, std::time::Instant)>,
    next_fence: u32,
    /// Keys and pointer buttons this client holds down on the shared
    /// machine, so they can be let go when it leaves or is made view-only.
    held_keys: std::collections::HashSet<u32>,
    pointer: (u16, u16, u8),
    /// The H.264 encoder of this client, once it has been sent a frame.
    h264: Option<H264Context>,
}

/// The H.264 stream of one client: one context over the whole screen.
struct H264Context {
    encoder: Box<dyn rfb_codec::video::VideoEncoder>,
    size: (u16, u16),
}

impl State {
    fn holds_input(&self) -> bool {
        !self.held_keys.is_empty() || self.pointer.2 != 0
    }

    /// Release everything the client still holds down. Without this a client
    /// that disconnects (or is switched to view only) with a key or button
    /// down would leave it stuck on the shared machine.
    fn release_input(&mut self, shared: &Shared) {
        for key in self.held_keys.drain() {
            shared.input.key(key, false);
        }
        if self.pointer.2 != 0 {
            shared.input.pointer(self.pointer.0, self.pointer.1, 0);
            self.pointer.2 = 0;
        }
    }
}

/// Input from this client is ignored: it is view only, or sharing is paused.
fn input_blocked(st: &State, shared: &Shared) -> bool {
    st.handle.view_only.load(Ordering::Relaxed) || shared.paused.load(Ordering::Relaxed)
}

fn whole(size: (u16, u16)) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    }
}

/// The rectangle that announces the layout: `x` is the reason, `y` the
/// status, the size is the picture's, and the payload lists the screens.
fn layout_rect(size: (u16, u16), reason: u16, status: u16) -> (RectHeader, Vec<u8>) {
    let mut payload = vec![1, 0, 0, 0];
    Screen {
        id: 0,
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
        flags: 0,
    }
    .write(&mut payload);
    (
        RectHeader {
            rect: Rect {
                x: reason,
                y: status,
                width: size.0,
                height: size.1,
            },
            encoding: pseudo::EXT_DESKTOP_SIZE,
        },
        payload,
    )
}

#[allow(clippy::too_many_lines)] // the client's event loop
pub(crate) async fn run<S>(
    mut session: ServerSession<S>,
    shared: Arc<Shared>,
    handle: Arc<ClientHandle>,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut updates = shared.updates.subscribe();
    let size = {
        let fb = shared
            .fb
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (fb.width(), fb.height())
    };
    let mut st = State {
        handle: Arc::clone(&handle),
        dirty: Region::new(),
        request: None,
        zrle: ZrleEncoder::new(),
        tight: TightEncoder::new(),
        clipboard: ExtClipboard::new(Role::Server),
        layout_told: false,
        layout_reply: None,
        wants_cursor: false,
        wants_copy: false,
        copies: Vec::new(),
        cursor: None,
        announced_clipboard: false,
        announced_fence: false,
        in_flight: std::collections::VecDeque::new(),
        next_fence: 0,
        held_keys: std::collections::HashSet::new(),
        pointer: (0, 0, 0),
        h264: None,
    };
    st.dirty.add(whole(size));
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result: Result<(), Error> = tokio::select! {
        // A kick (the dashboard's Disconnect) ends the task wherever it waits,
        // including inside a write to a client that has stopped reading.
        () = handle.kick.notified() => Err(Error::Closed),
        result = async {
    loop {
        // A client that never answers a fence must not stall for ever.
        let now = std::time::Instant::now();
        while st
            .in_flight
            .front()
            .is_some_and(|&(_, at)| now.duration_since(at) > FENCE_PATIENCE)
        {
            st.in_flight.pop_front();
        }
        if let Some(request) = st.request {
            let ready = !shared.paused.load(Ordering::Relaxed)
                && (st.dirty.touches(request)
                || st.layout_reply.is_some()
                || st.cursor.is_some()
                || !st.copies.is_empty());
            // With fences, send the next update only once the client has
            // caught up with the earlier ones: a slow link then skips
            // intermediate frames (the changes pile up in `dirty`) instead of
            // queueing them.
            let paced = st.in_flight.len() < MAX_IN_FLIGHT;
            if ready && paced {
                // A client that stops reading must not hold this task (and
                // its slot) for ever; `kick` is handled around the loop.
                let sent = tokio::time::timeout(
                    WRITE_TIMEOUT,
                    send_update(&mut session, &shared, &mut st, request),
                )
                .await
                .map_err(|_| Error::Io(std::io::ErrorKind::TimedOut.into()))??;
                if sent {
                    st.request = None;
                }
            }
        }
        if st.holds_input() && input_blocked(&st, &shared) {
            st.release_input(&shared);
        }
        handle
            .bytes_in
            .store(session.bytes_received(), Ordering::Relaxed);
        handle
            .bytes_out
            .store(session.bytes_sent(), Ordering::Relaxed);
        tokio::select! {
            _ = tick.tick() => {}
            message = session.next_message() => {
                handle_message(&mut session, &shared, &mut st, message?).await?;
            }
            update = updates.recv() => match update {
                Ok(u) if u.ended => {
                    return Err(Error::Io(std::io::Error::other("screen capture stopped")));
                }
                Ok(u) => {
                    if let Some(text) = &u.clipboard {
                        send_clipboard(&mut session, &mut st, text).await?;
                    }
                    // Copies are only right if the client's own picture has the
                    // old content at the source: it must not be waiting for
                    // changes there.
                    let accepted: Vec<_> = u
                        .copies
                        .iter()
                        .filter(|c| {
                            st.wants_copy
                                && !st.dirty.touches(Rect {
                                    x: c.src_x,
                                    y: c.src_y,
                                    ..c.dst
                                })
                        })
                        .copied()
                        .collect();
                    for r in &u.rects {
                        st.dirty.add(*r);
                    }
                    for c in accepted {
                        // The copy fills the destination: no pixels needed.
                        let _ = st.dirty.take_within(c.dst);
                        st.copies.push(c);
                    }
                    if st.wants_cursor
                        && let Some(c) = &u.cursor {
                            st.cursor = Some(Arc::clone(c));
                        }
                }
                Err(RecvError::Lagged(_)) => {
                    let fb = shared.fb.read().unwrap_or_else(std::sync::PoisonError::into_inner);
                    st.dirty.add(whole((fb.width(), fb.height())));
                }
                Err(RecvError::Closed) => return Ok(()),
            },
        }
    }
        } => result,
    };
    st.release_input(&shared);
    result
}

#[allow(clippy::too_many_lines)] // one arm per client message
async fn handle_message<S>(
    session: &mut ServerSession<S>,
    shared: &Shared,
    st: &mut State,
    message: ClientMessage,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match message {
        ClientMessage::SetPixelFormat(pf) => {
            // Colour-mapped formats are not supported: end the session.
            PixelPacker::new(&pf).map_err(|e| Error::Io(std::io::Error::other(e)))?;
            // Everything must be sent again in the new format.
            let fb = shared
                .fb
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            st.dirty.add(whole((fb.width(), fb.height())));
        }
        ClientMessage::SetEncodings(list) => {
            st.wants_copy = list.contains(&enc::COPY_RECT);
            if !st.wants_copy {
                st.copies.clear();
            }
            let wants = list.contains(&pseudo::CURSOR);
            if wants && !st.wants_cursor {
                // Tell a client that just asked for it what the pointer looks like.
                st.cursor.clone_from(
                    &shared
                        .cursor
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            }
            st.wants_cursor = wants;
            if !wants {
                st.cursor = None;
            }
            if list.contains(&pseudo::EXT_CLIPBOARD) && !st.announced_clipboard {
                st.announced_clipboard = true;
                let (flags, payload) = st.clipboard.announce();
                session
                    .send(&ServerMessage::ExtendedClipboard { flags, payload })
                    .await?;
            }
            if list.contains(&pseudo::FENCE) && !st.announced_fence {
                st.announced_fence = true;
                // A request with no flags and no payload: every client has to
                // answer it, and none mistakes it for a stray reply (noVNC
                // closes the connection on a fence that is not a request).
                session
                    .send(&ServerMessage::Fence {
                        flags: fence::REQUEST,
                        payload: Vec::new(),
                    })
                    .await?;
            }
        }
        ClientMessage::FramebufferUpdateRequest { incremental, rect } => {
            if !incremental {
                st.dirty.add(rect);
            }
            st.request = Some(st.request.map_or(rect, |old| region::union(old, rect)));
        }
        ClientMessage::KeyEvent { down, keysym } => {
            if !input_blocked(st, shared) {
                if down {
                    st.held_keys.insert(keysym);
                } else {
                    st.held_keys.remove(&keysym);
                }
                shared.input.key(keysym, down);
            }
        }
        ClientMessage::PointerEvent { buttons, x, y } => {
            if !input_blocked(st, shared) {
                st.pointer = (x, y, buttons);
                shared.input.pointer(x, y, buttons);
            }
        }
        ClientMessage::ClientCutText(latin1) => {
            if !input_blocked(st, shared) {
                let text: String = latin1.iter().map(|&b| char::from(b)).collect();
                shared.input.clipboard(&text);
            }
        }
        ClientMessage::ExtendedClipboard { flags, payload } => {
            let reaction = st
                .clipboard
                .on_message(flags, &payload)
                .map_err(|e| Error::Io(std::io::Error::other(e)))?;
            for (flags, payload) in reaction.send {
                session
                    .send(&ServerMessage::ExtendedClipboard { flags, payload })
                    .await?;
            }
            if let Some(text) = reaction.received
                && !input_blocked(st, shared)
            {
                // The wire has CR LF; backends get plain LF.
                shared.input.clipboard(&text.replace("\r\n", "\n"));
            }
        }
        ClientMessage::Fence { flags, payload } => {
            if flags & fence::REQUEST != 0 {
                // Messages are handled in order, so everything before this
                // fence is done: answer at once with the flags we know.
                let known = fence::BLOCK_BEFORE | fence::BLOCK_AFTER | fence::SYNC_NEXT;
                session
                    .send(&ServerMessage::Fence {
                        flags: flags & known,
                        payload,
                    })
                    .await?;
            } else if let Ok(seq) = <[u8; 4]>::try_from(payload.as_slice()) {
                // The answer to one of ours: the client has everything sent
                // before it. Older unanswered ones are covered too.
                let seq = u32::from_be_bytes(seq);
                while let Some((n, at)) = st.in_flight.pop_front() {
                    if n == seq {
                        let micros = u64::try_from(at.elapsed().as_micros()).unwrap_or(u64::MAX);
                        st.handle.rtt_micros.store(micros.max(1), Ordering::Relaxed);
                        break;
                    }
                }
            }
        }
        ClientMessage::SetDesktopSize { .. } => {
            // Resizing the real screen is not supported yet: say so with the
            // next update.
            st.layout_reply = Some((REASON_CLIENT, STATUS_PROHIBITED));
        }
    }
    Ok(())
}

/// Send what changed inside `request`. Returns `false` if nothing could be
/// sent after all (so the request stays).
#[allow(clippy::too_many_lines)] // one pass: choose, encode under the lock, send
async fn send_update<S>(
    session: &mut ServerSession<S>,
    shared: &Shared,
    st: &mut State,
    request: Rect,
) -> Result<bool, Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let encodings = session.conn().client_encodings().to_vec();
    let pf = *session.conn().client_pixel_format();
    let announced = session.conn().framebuffer_size();
    let packer = PixelPacker::new(&pf).map_err(|e| Error::Io(std::io::Error::other(e)))?;
    let wants_ext = encodings.contains(&pseudo::EXT_DESKTOP_SIZE);
    let wants_size = wants_ext || encodings.contains(&pseudo::DESKTOP_SIZE);
    // The client lists its encodings by preference: take the first one we
    // can produce. Raw is understood by every client.
    // H.264 needs a codec, and the whole screen in one request: its context
    // is the whole picture, and a client that asks for less would get frames
    // outside what it asked for.
    let video = shared
        .video
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let fb_size = {
        let fb = shared
            .fb
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (fb.width(), fb.height())
    };
    let h264_ok = video.is_some() && request == whole(fb_size) && fb_size == announced;
    let chosen = encodings
        .iter()
        .copied()
        .find(|e| match *e {
            enc::OPEN_H264 => h264_ok,
            other => matches!(
                other,
                enc::ZRLE | enc::TIGHT | enc::TRLE | enc::HEXTILE | enc::RAW
            ),
        })
        .unwrap_or(enc::RAW);
    let mut h264_frame: Option<(Vec<u8>, (u16, u16))> = None;

    // Encode under the read lock, send after releasing it.
    let (headers, payloads) = {
        let fb = shared
            .fb
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let size = (fb.width(), fb.height());
        let mut headers: Vec<RectHeader> = Vec::new();
        let mut payloads: Vec<Vec<u8>> = Vec::new();
        let mut area = request;
        if size != announced {
            if !wants_size {
                return Err(Error::Io(std::io::Error::other(
                    "the screen changed size and the client cannot follow",
                )));
            }
            // The client starts again from an empty picture of the new size.
            area = whole(size);
            st.dirty.clear();
            st.dirty.add(area);
            st.copies.clear();
            let (header, payload) = if wants_ext {
                layout_rect(size, REASON_SERVER, STATUS_OK)
            } else {
                (
                    RectHeader {
                        rect: whole(size),
                        encoding: pseudo::DESKTOP_SIZE,
                    },
                    Vec::new(),
                )
            };
            headers.push(header);
            payloads.push(payload);
            st.layout_told = true;
        } else if let Some((reason, status)) = st.layout_reply.take() {
            if wants_ext {
                let (header, payload) = layout_rect(size, reason, status);
                headers.push(header);
                payloads.push(payload);
                st.layout_told = true;
            }
        } else if wants_ext && !st.layout_told {
            let (header, payload) = layout_rect(size, REASON_SERVER, STATUS_OK);
            headers.push(header);
            payloads.push(payload);
            st.layout_told = true;
        }
        if let Some(cursor) = st.cursor.take()
            && st.wants_cursor
        {
            let payload =
                encode_cursor(&cursor, &packer).map_err(|e| Error::Io(std::io::Error::other(e)))?;
            headers.push(RectHeader {
                rect: Rect {
                    x: cursor.hotspot.0,
                    y: cursor.hotspot.1,
                    width: cursor.width,
                    height: cursor.height,
                },
                encoding: pseudo::CURSOR,
            });
            payloads.push(payload);
        }
        if chosen == enc::OPEN_H264 {
            // Every frame redraws the whole picture, so scrolls need no copy.
            st.copies.clear();
        }
        // Scrolls first: they act on the picture as the client has it now.
        for copy in st.copies.drain(..) {
            headers.push(RectHeader {
                rect: copy.dst,
                encoding: enc::COPY_RECT,
            });
            let [sx, sy] = [copy.src_x.to_be_bytes(), copy.src_y.to_be_bytes()];
            payloads.push(vec![sx[0], sx[1], sy[0], sy[1]]);
        }
        st.dirty.clip(size.0, size.1);
        let pieces = st.dirty.take_within(area);
        if chosen == enc::OPEN_H264 {
            // Whatever changed, the frame is the whole picture; copy it out
            // and encode after the lock is let go.
            if !pieces.is_empty() {
                h264_frame = Some((fb.pixels().to_vec(), size));
            }
        }
        for piece in pieces.into_iter().filter(|_| chosen != enc::OPEN_H264) {
            // Tight rectangles are at most 2048 wide.
            let width_limit = if chosen == enc::TIGHT {
                TIGHT_MAX_WIDTH
            } else {
                u16::MAX
            };
            for rect in split_width(piece, width_limit) {
                let payload = match chosen {
                    enc::ZRLE => st.zrle.encode(&fb, rect, &packer),
                    enc::TIGHT => st.tight.encode(&fb, rect, &packer),
                    enc::TRLE => encode_trle(&fb, rect, &packer),
                    enc::HEXTILE => encode_hextile(&fb, rect, &packer),
                    _ => encode_raw(&fb, rect, &packer),
                }
                .map_err(|e| Error::Io(std::io::Error::other(e)))?;
                headers.push(RectHeader {
                    rect,
                    encoding: chosen,
                });
                payloads.push(payload);
            }
        }
        (headers, payloads)
    };
    let (mut headers, mut payloads) = (headers, payloads);
    if let (Some((pixels, size)), Some(codec)) = (h264_frame, video)
        && let Some(payload) = encode_h264(st, &*codec, &pixels, size)?
    {
        headers.push(RectHeader {
            rect: whole(size),
            encoding: enc::OPEN_H264,
        });
        payloads.push(payload);
    }
    if headers.is_empty() {
        return Ok(false);
    }
    let rects: Vec<EncodedRect<'_>> = headers
        .iter()
        .zip(&payloads)
        .map(|(header, payload)| EncodedRect {
            header: *header,
            payload,
        })
        .collect();
    session.send_update(&rects).await?;
    st.handle.updates.fetch_add(1, Ordering::Relaxed);
    if st.announced_fence {
        // Ask the client to confirm it got everything so far.
        let seq = st.next_fence;
        st.next_fence = st.next_fence.wrapping_add(1);
        session
            .send(&ServerMessage::Fence {
                flags: fence::REQUEST | fence::BLOCK_BEFORE,
                payload: seq.to_be_bytes().to_vec(),
            })
            .await?;
        st.in_flight.push_back((seq, std::time::Instant::now()));
    }
    st.handle.encoding.store(chosen, Ordering::Relaxed);
    Ok(true)
}

/// One H.264 frame of the whole picture, as an Open H.264 payload: flags
/// that start the context (first frame, or the picture changed size), then
/// the data. `None` when the encoder had nothing to send.
fn encode_h264(
    st: &mut State,
    codec: &dyn rfb_codec::video::VideoCodec,
    pixels: &[u8],
    size: (u16, u16),
) -> Result<Option<Vec<u8>>, Error> {
    use rfb_codec::video::{RESET_ALL_CONTEXTS, RESET_CONTEXT, payload};
    let io = |e: String| Error::Io(std::io::Error::other(e));
    let mut flags = 0;
    let fresh = match &st.h264 {
        Some(ctx) if ctx.size == size => false,
        Some(_) => {
            flags |= RESET_ALL_CONTEXTS;
            true
        }
        None => true,
    };
    if fresh {
        flags |= RESET_CONTEXT;
        st.h264 = Some(H264Context {
            encoder: codec.encoder(size.0, size.1).map_err(io)?,
            size,
        });
    }
    let ctx = st.h264.as_mut().expect("created above");
    let data = ctx
        .encoder
        .encode(pixels, size.0, size.1, usize::from(size.0) * 4, fresh)
        .map_err(io)?;
    if data.is_empty() && !fresh {
        return Ok(None);
    }
    Ok(Some(payload(flags, &data)))
}

/// Tell the client the server's clipboard changed: through Extended Clipboard
/// (UTF-8) when the client has announced it, else as Latin-1 `ServerCutText`.
async fn send_clipboard<S>(
    session: &mut ServerSession<S>,
    st: &mut State,
    text: &str,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if st.clipboard.is_active() {
        for (flags, payload) in st.clipboard.local_changed(text) {
            session
                .send(&ServerMessage::ExtendedClipboard { flags, payload })
                .await?;
        }
        return Ok(());
    }
    let latin1: Vec<u8> = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    // Longer text cannot be sent the old way: better nothing than half of it.
    if latin1.len() <= rfb_proto::consts::limits::MAX_CUT_TEXT_LEN as usize {
        session.send(&ServerMessage::ServerCutText(latin1)).await?;
    }
    Ok(())
}

/// `rect` cut into pieces at most `limit` wide.
fn split_width(rect: Rect, limit: u16) -> Vec<Rect> {
    let limit = limit.max(1);
    let mut pieces = Vec::new();
    let mut x = 0;
    while x < rect.width {
        let width = (rect.width - x).min(limit);
        pieces.push(Rect {
            x: rect.x + x,
            width,
            ..rect
        });
        x += width;
    }
    pieces
}
