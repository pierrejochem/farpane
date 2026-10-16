use core::fmt;

use rfb_codec::{Damage, DecodeError, Decoder, Framebuffer, enc};
use rfb_proto::{
    PixelFormat, conn::ClientConnEvent, consts::pseudo, msg::client::ClientMessage, types::Rect,
};
use rfb_tokio::{ClientOptions, ClientSession, Error, ServerInfo};
use tokio::net::TcpStream;

/// Encodings we ask for, most preferred first. ZRLE usually compresses best;
/// Raw is always understood by servers.
pub const DEFAULT_ENCODINGS: [i32; 7] = [
    enc::ZRLE,
    enc::HEXTILE,
    enc::ZLIB,
    enc::CO_RRE,
    enc::COPY_RECT,
    enc::RAW,
    pseudo::DESKTOP_SIZE,
];

/// Why connecting or running failed.
#[derive(Debug)]
pub enum ConnectError {
    Session(Error),
    Decode(DecodeError),
    Io(std::io::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(e) => write!(f, "{e}"),
            Self::Decode(e) => write!(f, "decode error: {e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<Error> for ConnectError {
    fn from(e: Error) -> Self {
        Self::Session(e)
    }
}

impl From<DecodeError> for ConnectError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

/// A connected remote screen: the session, its decoder and the picture.
#[derive(Debug)]
pub struct RemoteScreen {
    session: ClientSession<TcpStream>,
    decoder: Decoder,
    fb: Framebuffer,
    name: String,
}

impl RemoteScreen {
    /// Connect to `addr` (`host:port`), authenticate, switch the server to our
    /// native pixel format and ask for a full first update.
    ///
    /// # Errors
    /// Connection, handshake (including a wrong or missing password), or
    /// setup errors.
    pub async fn connect(addr: &str, password: Option<Vec<u8>>) -> Result<Self, ConnectError> {
        Self::connect_with(addr, password, &DEFAULT_ENCODINGS).await
    }

    /// Like [`RemoteScreen::connect`], asking for `encodings` (most preferred
    /// first) instead of the default list. Handy to test one encoding at a time.
    ///
    /// # Errors
    /// As [`RemoteScreen::connect`].
    pub async fn connect_with(
        addr: &str,
        password: Option<Vec<u8>>,
        encodings: &[i32],
    ) -> Result<Self, ConnectError> {
        let stream = TcpStream::connect(addr).await.map_err(ConnectError::Io)?;
        stream.set_nodelay(true).map_err(ConnectError::Io)?;
        let (mut session, info) = ClientSession::connect(
            stream,
            ClientOptions {
                password,
                ..ClientOptions::default()
            },
        )
        .await?;
        let ServerInfo {
            width,
            height,
            name,
            ..
        } = info;
        let fb = Framebuffer::new(width, height)?;
        let decoder = Decoder::new(&PixelFormat::RGB888)?;
        session
            .send(&ClientMessage::SetPixelFormat(PixelFormat::RGB888))
            .await?;
        session
            .send(&ClientMessage::SetEncodings(encodings.to_vec()))
            .await?;
        let mut screen = Self {
            session,
            decoder,
            fb,
            name: String::from_utf8_lossy(&name).into_owned(),
        };
        screen.request_update(false).await?;
        Ok(screen)
    }

    /// The remote desktop's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current picture.
    #[must_use]
    pub fn framebuffer(&self) -> &Framebuffer {
        &self.fb
    }

    /// Ask for the whole screen (`incremental == false`) or only changes.
    ///
    /// # Errors
    /// I/O errors.
    pub async fn request_update(&mut self, incremental: bool) -> Result<(), ConnectError> {
        let rect = Rect {
            x: 0,
            y: 0,
            width: self.fb.width(),
            height: self.fb.height(),
        };
        self.session
            .send(&ClientMessage::FramebufferUpdateRequest { incremental, rect })
            .await?;
        Ok(())
    }

    /// Wait for the next thing worth reacting to. Decoding happens before
    /// this returns, so the picture is already up to date for a
    /// [`ScreenEvent::Damage`]. Cancel safe: dropping the future (for example
    /// in a `select!`) loses nothing.
    ///
    /// # Errors
    /// Session errors (including the server hanging up) and decode errors;
    /// after an error the connection is unusable.
    pub async fn next_event(&mut self) -> Result<ScreenEvent, ConnectError> {
        loop {
            match self.session.next_event().await? {
                ClientConnEvent::Rect { header, payload } => {
                    let damage = self.decoder.decode(&mut self.fb, &header, &payload)?;
                    if damage != Damage::None {
                        return Ok(ScreenEvent::Damage(damage));
                    }
                }
                ClientConnEvent::UpdateEnd => return Ok(ScreenEvent::UpdateEnd),
                ClientConnEvent::Bell => return Ok(ScreenEvent::Bell),
                ClientConnEvent::CutText(text) => return Ok(ScreenEvent::CutText(text)),
                ClientConnEvent::Handshake(_)
                | ClientConnEvent::UpdateBegin { .. }
                | ClientConnEvent::ColourMap { .. } => {}
            }
        }
    }

    /// Wait for one complete `FramebufferUpdate`, decode it into the picture
    /// and return what changed. Not cancel safe: use [`RemoteScreen::next_event`]
    /// inside a `select!`.
    ///
    /// # Errors
    /// As [`RemoteScreen::next_event`].
    pub async fn next_update(&mut self) -> Result<Vec<Damage>, ConnectError> {
        let mut damage = Vec::new();
        loop {
            match self.next_event().await? {
                ScreenEvent::Damage(d) => damage.push(d),
                ScreenEvent::UpdateEnd => return Ok(damage),
                ScreenEvent::Bell | ScreenEvent::CutText(_) => {}
            }
        }
    }

    /// Send a key press or release (an X11 keysym).
    ///
    /// # Errors
    /// I/O errors.
    pub async fn key(&mut self, keysym: u32, down: bool) -> Result<(), ConnectError> {
        Ok(self
            .session
            .send(&ClientMessage::KeyEvent { down, keysym })
            .await?)
    }

    /// Send the pointer position (remote framebuffer pixels) and button mask
    /// (bit 0 left, 1 middle, 2 right, 3 wheel up, 4 wheel down, ...).
    ///
    /// # Errors
    /// I/O errors.
    pub async fn pointer(&mut self, buttons: u8, x: u16, y: u16) -> Result<(), ConnectError> {
        Ok(self
            .session
            .send(&ClientMessage::PointerEvent { buttons, x, y })
            .await?)
    }

    /// Send clipboard text (Latin-1 as the legacy message requires).
    ///
    /// # Errors
    /// I/O errors.
    pub async fn cut_text(&mut self, latin1: Vec<u8>) -> Result<(), ConnectError> {
        Ok(self
            .session
            .send(&ClientMessage::ClientCutText(latin1))
            .await?)
    }
}

/// Something that happened on the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenEvent {
    /// The picture changed (already applied).
    Damage(Damage),
    /// The server finished one `FramebufferUpdate`; request the next one.
    UpdateEnd,
    Bell,
    /// Clipboard text from the server (Latin-1 bytes).
    CutText(Vec<u8>),
}
