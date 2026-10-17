//! The `PipeWire` side: receives the screen frames the portal hands out and
//! keeps the newest one for [`crate::WaylandSource`].

use std::{
    os::fd::OwnedFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use pipewire as pw;
use pw::spa;
use spa::{
    param::video::{VideoFormat, VideoInfoRaw},
    pod::Pod,
};

/// Pixel layouts the source understands (4 or 3 bytes per pixel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    Bgrx,
    Rgbx,
    Rgb,
    Bgr,
}

impl Layout {
    pub(crate) fn from_spa(format: VideoFormat) -> Option<Self> {
        match format {
            VideoFormat::BGRx | VideoFormat::BGRA => Some(Self::Bgrx),
            VideoFormat::RGBx | VideoFormat::RGBA => Some(Self::Rgbx),
            VideoFormat::RGB => Some(Self::Rgb),
            VideoFormat::BGR => Some(Self::Bgr),
            _ => None,
        }
    }

    pub(crate) fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Bgrx | Self::Rgbx => 4,
            Self::Rgb | Self::Bgr => 3,
        }
    }

    /// Convert tightly packed `src` rows to `B, G, R, X` pixels appended to `out`.
    pub(crate) fn to_bgrx(self, src: &[u8], out: &mut Vec<u8>) {
        match self {
            Self::Bgrx => out.extend_from_slice(src),
            Self::Rgbx => {
                for p in src.as_chunks::<4>().0 {
                    out.extend_from_slice(&[p[2], p[1], p[0], 0xFF]);
                }
            }
            Self::Rgb => {
                for p in src.as_chunks::<3>().0 {
                    out.extend_from_slice(&[p[2], p[1], p[0], 0xFF]);
                }
            }
            Self::Bgr => {
                for p in src.as_chunks::<3>().0 {
                    out.extend_from_slice(&[p[0], p[1], p[2], 0xFF]);
                }
            }
        }
    }
}

/// One screen frame, rows packed without padding.
#[derive(Debug, Default, Clone)]
pub(crate) struct Frame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub layout: Option<Layout>,
    pub data: Vec<u8>,
}

/// Shared between the `PipeWire` thread and the source.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub frame: Mutex<Frame>,
    /// The stream ended or failed; `error` says why.
    pub ended: AtomicBool,
    pub error: Mutex<Option<String>>,
}

impl Shared {
    fn fail(&self, why: impl Into<String>) {
        *self
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(why.into());
        self.ended.store(true, Ordering::Relaxed);
    }
}

struct UserData {
    format: VideoInfoRaw,
    shared: Arc<Shared>,
    seq: u64,
}

/// What we accept: the formats above, any size, any frame rate.
fn format_pod() -> Vec<u8> {
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::BGR,
            VideoFormat::RGB,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 16384,
                height: 16384
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 240, denom: 1 }
        ),
    );
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .unwrap_or_default()
}

/// Run the `PipeWire` loop until `quit` fires or the stream ends. Meant for
/// its own thread.
pub(crate) fn run(
    fd: OwnedFd,
    node_id: u32,
    shared: &Arc<Shared>,
    quit: pw::channel::Receiver<()>,
) {
    if let Err(e) = run_inner(fd, node_id, shared, quit) {
        shared.fail(format!("PipeWire: {e}"));
    }
    shared.ended.store(true, Ordering::Relaxed);
}

#[allow(clippy::too_many_lines)] // one stream set-up, with its callbacks
fn run_inner(
    fd: OwnedFd,
    node_id: u32,
    shared: &Arc<Shared>,
    quit: pw::channel::Receiver<()>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let weak = mainloop.downgrade();
    let _quit = quit.attach(mainloop.loop_(), move |()| {
        if let Some(l) = weak.upgrade() {
            l.quit();
        }
    });
    let stream = pw::stream::StreamBox::new(
        &core,
        "farpane",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let data = UserData {
        format: VideoInfoRaw::default(),
        shared: Arc::clone(shared),
        seq: 0,
    };
    let loop_for_state = mainloop.downgrade();
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, user, _old, new| {
            if let pw::stream::StreamState::Error(why) = new {
                user.shared.fail(format!("PipeWire stream: {why}"));
                if let Some(l) = loop_for_state.upgrade() {
                    l.quit();
                }
            }
        })
        .param_changed(|_, user, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media, subtype)) = spa::param::format_utils::parse_format(param) else {
                return;
            };
            if media != spa::param::format::MediaType::Video
                || subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            if user.format.parse(param).is_err() {
                user.shared
                    .fail("PipeWire sent a video format that cannot be read");
            }
        })
        .process(|stream, user| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let size = user.format.size();
            let (w, h) = (size.width as usize, size.height as usize);
            let Some(layout) = Layout::from_spa(user.format.format()) else {
                user.shared.fail(format!(
                    "the screen is delivered in a pixel format we cannot read ({:?})",
                    user.format.format()
                ));
                return;
            };
            let datas = buffer.datas_mut();
            let Some(first) = datas.first_mut() else {
                return;
            };
            let (stride, offset, used) = {
                let c = first.chunk();
                (c.stride(), c.offset() as usize, c.size() as usize)
            };
            let Some(bytes) = first.data() else {
                // No mapped memory: a buffer type we did not ask for.
                user.shared
                    .fail("PipeWire delivered a buffer without readable memory");
                return;
            };
            let bpp = layout.bytes_per_pixel();
            let stride = usize::try_from(stride).unwrap_or(0).max(w * bpp);
            if w == 0
                || h == 0
                || offset + (h - 1) * stride + w * bpp
                    > bytes.len().min(offset + used.max(stride * h))
            {
                return; // an empty or short buffer: skip this frame
            }
            let mut frame = user
                .shared
                .frame
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            frame.data.clear();
            for row in 0..h {
                let at = offset + row * stride;
                frame.data.extend_from_slice(&bytes[at..at + w * bpp]);
            }
            user.seq += 1;
            frame.seq = user.seq;
            frame.width = size.width;
            frame.height = size.height;
            frame.layout = Some(layout);
        })
        .register()?;

    let values = format_pod();
    let mut params = [Pod::from_bytes(&values).ok_or(pw::Error::CreationFailed)?];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    mainloop.run();
    Ok(())
}
