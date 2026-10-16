//! One client connection: its pending changes, encoder state and requests.

use std::sync::Arc;

use rfb_codec::{
    PixelPacker, ZrleEncoder,
    clipboard::{ExtClipboard, Role},
    encode_raw,
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
    server::Shared,
};

/// `ExtendedDesktopSize` reasons and results (rfbproto.rst).
const REASON_SERVER: u16 = 0;
const REASON_CLIENT: u16 = 1;
const STATUS_OK: u16 = 0;
const STATUS_PROHIBITED: u16 = 1;

struct State {
    dirty: Region,
    /// The area of the outstanding update request, if any.
    request: Option<Rect>,
    zrle: ZrleEncoder,
    clipboard: ExtClipboard,
    /// Whether the client has been told the layout (`ExtendedDesktopSize`).
    layout_told: bool,
    /// A reply to a `SetDesktopSize`, sent with the next update.
    layout_reply: Option<(u16, u16)>,
    announced_clipboard: bool,
    announced_fence: bool,
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

pub(crate) async fn run<S>(mut session: ServerSession<S>, shared: Arc<Shared>) -> Result<(), Error>
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
        dirty: Region::new(),
        request: None,
        zrle: ZrleEncoder::new(),
        clipboard: ExtClipboard::new(Role::Server),
        layout_told: false,
        layout_reply: None,
        announced_clipboard: false,
        announced_fence: false,
    };
    st.dirty.add(whole(size));
    loop {
        if let Some(request) = st.request {
            let ready = st.dirty.touches(request) || st.layout_reply.is_some();
            if ready && send_update(&mut session, &shared, &mut st, request).await? {
                st.request = None;
            }
        }
        tokio::select! {
            message = session.next_message() => {
                handle_message(&mut session, &shared, &mut st, message?).await?;
            }
            update = updates.recv() => match update {
                Ok(u) if u.ended => {
                    return Err(Error::Io(std::io::Error::other("screen capture stopped")));
                }
                Ok(u) => {
                    for r in &u.rects {
                        st.dirty.add(*r);
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
}

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
            if list.contains(&pseudo::EXT_CLIPBOARD) && !st.announced_clipboard {
                st.announced_clipboard = true;
                let (flags, payload) = st.clipboard.announce();
                session
                    .send(&ServerMessage::ExtendedClipboard { flags, payload })
                    .await?;
            }
            if list.contains(&pseudo::FENCE) && !st.announced_fence {
                st.announced_fence = true;
                session
                    .send(&ServerMessage::Fence {
                        flags: 0,
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
            if !shared.view_only {
                shared.input.key(keysym, down);
            }
        }
        ClientMessage::PointerEvent { buttons, x, y } => {
            if !shared.view_only {
                shared.input.pointer(x, y, buttons);
            }
        }
        ClientMessage::ClientCutText(latin1) => {
            if !shared.view_only {
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
            if let Some(text) = reaction.received {
                if !shared.view_only {
                    // The wire has CR LF; backends get plain LF.
                    shared.input.clipboard(&text.replace("\r\n", "\n"));
                }
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
    let use_zrle = encodings.contains(&enc::ZRLE);

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
        st.dirty.clip(size.0, size.1);
        for rect in st.dirty.take_within(area) {
            let (encoding, payload) = if use_zrle {
                (enc::ZRLE, st.zrle.encode(&fb, rect, &packer))
            } else {
                (enc::RAW, encode_raw(&fb, rect, &packer))
            };
            let payload = payload.map_err(|e| Error::Io(std::io::Error::other(e)))?;
            headers.push(RectHeader { rect, encoding });
            payloads.push(payload);
        }
        (headers, payloads)
    };
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
    Ok(true)
}
