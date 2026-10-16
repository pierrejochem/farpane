use std::io;

use rfb_codec::{Cursor, Framebuffer};
use rfb_proto::types::Rect;
use rfb_server::{Capture, Source};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        damage::{self, ConnectionExt as _},
        xfixes::{self, ConnectionExt as _},
        xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Window},
    },
};

use crate::{Conn, io_err};

/// More damaged rectangles than this, or this share of the screen, and the
/// whole screen is read in one go instead.
const MAX_RECTS: usize = 24;

/// Rows per `GetImage` request, to keep replies a reasonable size.
const BAND: u16 = 256;

/// Captures the root window.
pub struct X11Source {
    conn: Conn,
    root: Window,
    size: (u16, u16),
    first: bool,
    /// `XFixes` said the cursor changed since it was last read.
    cursor_changed: bool,
    xfixes: bool,
}

impl std::fmt::Debug for X11Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Source")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl X11Source {
    pub(crate) fn new(conn: Conn, screen_num: usize) -> io::Result<Self> {
        let setup = conn.setup();
        let screen = &setup.roots[screen_num];
        let root = screen.root;
        // 32 bits per pixel, B in the lowest byte: the framebuffer's layout.
        let format = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == screen.root_depth)
            .ok_or_else(|| io::Error::other("no pixmap format for the root depth"))?;
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == screen.root_visual)
            .ok_or_else(|| io::Error::other("root visual not found"))?;
        if format.bits_per_pixel != 32
            || setup.image_byte_order != ImageOrder::LSB_FIRST
            || (visual.red_mask, visual.green_mask, visual.blue_mask) != (0xFF_0000, 0xFF00, 0xFF)
        {
            return Err(io::Error::other(format!(
                "unsupported root visual: {} bpp, masks {:x}/{:x}/{:x}",
                format.bits_per_pixel, visual.red_mask, visual.green_mask, visual.blue_mask
            )));
        }
        conn.damage_query_version(1, 1)
            .map_err(io_err)?
            .reply()
            .map_err(|e| io::Error::other(format!("the Damage extension is missing: {e}")))?;
        let id = conn.generate_id().map_err(io_err)?;
        conn.damage_create(id, root, damage::ReportLevel::RAW_RECTANGLES)
            .map_err(io_err)?;
        // The cursor shape comes from XFixes; without it clients draw their own.
        let xfixes = conn
            .xfixes_query_version(4, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some()
            && conn
                .xfixes_select_cursor_input(root, xfixes::CursorNotifyMask::DISPLAY_CURSOR)
                .is_ok();
        conn.flush().map_err(io_err)?;
        let size = (screen.width_in_pixels, screen.height_in_pixels);
        Ok(Self {
            conn,
            root,
            size,
            first: true,
            cursor_changed: false,
            xfixes,
        })
    }

    /// The root window id (for diagnostics and tests).
    #[must_use]
    pub fn root(&self) -> u32 {
        self.root
    }

    /// The pointer's shape if it changed since the last call (or `force`).
    fn cursor_if_changed(&mut self, force: bool) -> io::Result<Option<Cursor>> {
        if !self.xfixes || !(self.cursor_changed || force) {
            return Ok(None);
        }
        self.cursor_changed = false;
        let image = self
            .conn
            .xfixes_get_cursor_image()
            .map_err(io_err)?
            .reply()
            .map_err(io_err)?;
        let mut rgba = Vec::with_capacity(image.cursor_image.len() * 4);
        for argb in &image.cursor_image {
            // XFixes gives premultiplied ARGB; clients want straight colour.
            let [b, g, r, a] = argb.to_le_bytes();
            let un = |c: u8| {
                if a == 0 {
                    0
                } else {
                    u8::try_from((u32::from(c) * 255 / u32::from(a)).min(255)).unwrap_or(255)
                }
            };
            rgba.extend_from_slice(&[un(r), un(g), un(b), a]);
        }
        Ok(Some(Cursor {
            hotspot: (image.xhot, image.yhot),
            width: image.width,
            height: image.height,
            rgba,
        }))
    }

    fn geometry(&self) -> io::Result<(u16, u16)> {
        let g = self
            .conn
            .get_geometry(self.root)
            .map_err(io_err)?
            .reply()
            .map_err(io_err)?;
        Ok((g.width, g.height))
    }

    /// Read `rect` of the root into `fb`.
    fn read(&self, fb: &mut Framebuffer, rect: Rect) -> io::Result<()> {
        let mut y = rect.y;
        let end = rect.y + rect.height;
        while y < end {
            let band = Rect {
                y,
                height: (end - y).min(BAND),
                ..rect
            };
            let image = self
                .conn
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    self.root,
                    i16::try_from(band.x).map_err(io_err)?,
                    i16::try_from(band.y).map_err(io_err)?,
                    band.width,
                    band.height,
                    !0,
                )
                .map_err(io_err)?
                .reply()
                .map_err(io_err)?;
            fb.write_bgrx(band, &image.data).map_err(io_err)?;
            y += band.height;
        }
        Ok(())
    }
}

impl Source for X11Source {
    fn size(&self) -> (u16, u16) {
        self.size
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let (w, h) = self.geometry()?;
        let whole = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut damaged: Vec<Rect> = Vec::new();
        // Drain the damage events (raw rectangles need no re-arming).
        let mut everything = self.first;
        while let Some(event) = self.conn.poll_for_event().map_err(io_err)? {
            if let Event::XfixesCursorNotify(_) = event {
                self.cursor_changed = true;
            }
            if let Event::DamageNotify(d) = event {
                if damaged.len() < MAX_RECTS {
                    damaged.push(Rect {
                        x: u16::try_from(d.area.x.max(0)).unwrap_or(0),
                        y: u16::try_from(d.area.y.max(0)).unwrap_or(0),
                        width: d.area.width,
                        height: d.area.height,
                    });
                } else {
                    everything = true;
                }
            }
        }
        if (w, h) != self.size {
            self.size = (w, h);
            fb.resize(w, h).map_err(io_err)?;
            everything = true;
        }
        self.first = false;
        if everything {
            self.read(fb, whole)?;
            return Ok(Capture {
                damage: vec![whole],
                cursor: self.cursor_if_changed(true)?,
            });
        }
        // Keep what lies inside the screen.
        let mut damage = Vec::with_capacity(damaged.len());
        for mut r in damaged {
            r.width = r.width.min(w.saturating_sub(r.x));
            r.height = r.height.min(h.saturating_sub(r.y));
            if r.width > 0 && r.height > 0 {
                self.read(fb, r)?;
                damage.push(r);
            }
        }
        Ok(Capture {
            damage,
            cursor: self.cursor_if_changed(false)?,
        })
    }
}
