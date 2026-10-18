use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use rfb_codec::{Cursor, Framebuffer};
use rfb_proto::types::Rect;
use rfb_server::{Capture, Source};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW,
        HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
    },
    System::{
        DataExchange::{
            CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
        },
        Memory::{GlobalLock, GlobalUnlock},
        Ole::CF_UNICODETEXT,
    },
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        WindowsAndMessaging::{
            CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, GetSystemMetrics, HCURSOR,
            ICONINFO, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
        },
    },
};

const TILE: usize = 64;
/// RFB sizes are 16 bits.
const MAX_SIDE: i32 = 0xFFFF;

/// Captures the virtual desktop with GDI.
pub struct WindowsSource {
    origin: (i32, i32),
    width: u16,
    height: u16,
    screen: HDC,
    mem: HDC,
    bitmap: HBITMAP,
    /// The latest frame, `B, G, R, X`, `width` x `height`, no padding.
    frame: Vec<u8>,
    first: bool,
    cursor: Option<CursorKey>,
    /// Clipboard sequence number already seen, and the one our own input
    /// set (not reported back).
    clip_seen: u32,
    clip_own: Arc<AtomicU32>,
}

/// What identifies the cursor currently shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorKey {
    handle: usize,
    showing: bool,
}

impl std::fmt::Debug for WindowsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsSource")
            .field("size", &(self.width, self.height))
            .finish_non_exhaustive()
    }
}

// SAFETY: the device contexts and the bitmap are plain GDI handles owned by
// this struct. GDI objects may be used from any thread as long as only one
// thread uses them at a time; the server moves the source to its capture
// thread and uses it only there.
unsafe impl Send for WindowsSource {}

/// The virtual desktop: origin and size in pixels.
fn virtual_screen() -> (i32, i32, i32, i32) {
    // SAFETY: GetSystemMetrics has no preconditions.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

impl WindowsSource {
    pub(crate) fn new(clip_own: Arc<AtomicU32>) -> io::Result<Self> {
        // Real pixels, not scaled ones. Fails harmlessly when the process
        // already chose (the dashboard's window library does).
        // SAFETY: no preconditions.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let (x, y, w, h) = virtual_screen();
        if w <= 0 || h <= 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(io::Error::other(format!(
                "the desktop has no usable size ({w} x {h}); is a user logged in on this session?"
            )));
        }
        // SAFETY: a null window means the whole screen; the DC is released in Drop.
        let screen = unsafe { GetDC(None) };
        if screen.is_invalid() {
            return Err(io::Error::other("cannot get the screen device context"));
        }
        // SAFETY: `screen` is a valid DC.
        let mem = unsafe { CreateCompatibleDC(Some(screen)) };
        let mut source = Self {
            origin: (x, y),
            width: 0,
            height: 0,
            screen,
            mem,
            bitmap: HBITMAP::default(),
            frame: Vec::new(),
            first: true,
            cursor: None,
            // SAFETY: no preconditions.
            clip_seen: unsafe { GetClipboardSequenceNumber() },
            clip_own,
        };
        source.set_size(w, h)?;
        Ok(source)
    }

    /// (Re)create the bitmap and the frame buffer for a `w` x `h` desktop.
    fn set_size(&mut self, w: i32, h: i32) -> io::Result<()> {
        // SAFETY: the old bitmap (if any) is not selected into a DC while it is deleted:
        // a new one is selected over it first.
        unsafe {
            let bitmap = CreateCompatibleBitmap(self.screen, w, h);
            if bitmap.is_invalid() {
                return Err(io::Error::other("cannot create the capture bitmap"));
            }
            // The first call replaces the DC's stock 1 x 1 bitmap, which is not ours to delete.
            SelectObject(self.mem, HGDIOBJ(bitmap.0));
            if self.bitmap.is_invalid() {
                self.bitmap = bitmap;
            } else {
                let _ = DeleteObject(HGDIOBJ(std::mem::replace(&mut self.bitmap, bitmap).0));
            }
        }
        self.width = u16::try_from(w).map_err(io::Error::other)?;
        self.height = u16::try_from(h).map_err(io::Error::other)?;
        self.frame = vec![0; usize::from(self.width) * usize::from(self.height) * 4];
        Ok(())
    }

    /// Copy the screen into `self.frame`.
    fn grab(&mut self) -> bool {
        let (w, h) = (i32::from(self.width), i32::from(self.height));
        // SAFETY: both DCs are valid; the bitmap in `mem` is `w` x `h`.
        if unsafe {
            BitBlt(
                self.mem,
                0,
                0,
                w,
                h,
                Some(self.screen),
                self.origin.0,
                self.origin.1,
                SRCCOPY | CAPTUREBLT,
            )
        }
        .is_err()
        {
            return false;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        // SAFETY: `frame` holds w * h * 4 bytes, exactly what 32-bit rows of
        // `h` lines need; the bitmap is not selected into another DC.
        let lines = unsafe {
            GetDIBits(
                self.mem,
                self.bitmap,
                0,
                u32::from(self.height),
                Some(self.frame.as_mut_ptr().cast()),
                &raw mut info,
                DIB_RGB_COLORS,
            )
        };
        lines == h
    }

    fn cursor(&mut self) -> Option<Cursor> {
        let mut info = CURSORINFO {
            cbSize: u32::try_from(size_of::<CURSORINFO>()).unwrap_or(0),
            ..CURSORINFO::default()
        };
        // SAFETY: `info` is initialised with its size.
        if unsafe { GetCursorInfo(&raw mut info) }.is_err() {
            return None;
        }
        let showing = info.flags.0 & CURSOR_SHOWING.0 != 0;
        let key = CursorKey {
            handle: info.hCursor.0 as usize,
            showing,
        };
        if self.cursor == Some(key) {
            return None;
        }
        self.cursor = Some(key);
        if !showing {
            return Some(hidden_cursor());
        }
        Some(read_cursor(self.screen, info.hCursor).unwrap_or_else(hidden_cursor))
    }

    fn clipboard(&mut self) -> Option<String> {
        // SAFETY: no preconditions.
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq == self.clip_seen {
            return None;
        }
        if seq == self.clip_own.load(Ordering::Relaxed) {
            self.clip_seen = seq;
            return None;
        }
        match read_clipboard_text() {
            Ok(text) => {
                self.clip_seen = seq;
                text
            }
            // Another program has the clipboard open: look again next time.
            Err(()) => None,
        }
    }
}

impl Drop for WindowsSource {
    fn drop(&mut self) {
        // SAFETY: each handle was created by `new` / `set_size` and is released once.
        unsafe {
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            }
            let _ = DeleteDC(self.mem);
            ReleaseDC(Some(HWND::default()), self.screen);
        }
    }
}

impl Source for WindowsSource {
    fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let mut capture = Capture::default();
        let (x, y, w, h) = virtual_screen();
        let mut whole = std::mem::take(&mut self.first);
        if (w, h) != (i32::from(self.width), i32::from(self.height)) || (x, y) != self.origin {
            if w <= 0 || h <= 0 || w > MAX_SIDE || h > MAX_SIDE {
                return Ok(capture); // mid-change; try again next time
            }
            self.origin = (x, y);
            self.set_size(w, h)?;
            whole = true;
        }
        if (fb.width(), fb.height()) != (self.width, self.height) {
            fb.resize(self.width, self.height)
                .map_err(io::Error::other)?;
            whole = true;
        }
        // The secure desktop and a locked session refuse BitBlt: keep the
        // last picture rather than ending the server.
        if self.grab() {
            let (w, h) = (usize::from(self.width), usize::from(self.height));
            let tiles = if whole {
                vec![Rect {
                    x: 0,
                    y: 0,
                    width: self.width,
                    height: self.height,
                }]
            } else {
                changed_tiles(fb, &self.frame, w, h)
            };
            let mut scratch = Vec::new();
            for rect in &tiles {
                scratch.clear();
                let row = usize::from(rect.width) * 4;
                for line in 0..usize::from(rect.height) {
                    let start = ((usize::from(rect.y) + line) * w + usize::from(rect.x)) * 4;
                    scratch.extend_from_slice(&self.frame[start..start + row]);
                }
                fb.write_bgrx(*rect, &scratch).map_err(io::Error::other)?;
            }
            capture.damage = tiles;
        }
        capture.cursor = self.cursor();
        capture.clipboard = self.clipboard();
        Ok(capture)
    }
}

fn hidden_cursor() -> Cursor {
    Cursor {
        hotspot: (0, 0),
        width: 0,
        height: 0,
        rgba: Vec::new(),
    }
}

/// The 64 x 64 tiles of `new` (`B, G, R, X`, `w` x `h`) that differ from the
/// picture in `fb`.
fn changed_tiles(fb: &Framebuffer, new: &[u8], w: usize, h: usize) -> Vec<Rect> {
    let old = fb.pixels();
    let stride = fb.stride();
    let mut out = Vec::new();
    for ty in (0..h).step_by(TILE) {
        let th = (h - ty).min(TILE);
        for tx in (0..w).step_by(TILE) {
            let tw = (w - tx).min(TILE);
            let differs = (0..th).any(|row| {
                let a = (ty + row) * stride + tx * 4;
                let b = ((ty + row) * w + tx) * 4;
                // The framebuffer's alpha byte is always opaque: compare colour only.
                old[a..a + tw * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(new[b..b + tw * 4].as_chunks::<4>().0)
                    .any(|(o, n)| o[..3] != n[..3])
            });
            if differs {
                // Tile sizes are at most 64.
                #[allow(clippy::cast_possible_truncation)]
                out.push(Rect {
                    x: tx as u16,
                    y: ty as u16,
                    width: tw as u16,
                    height: th as u16,
                });
            }
        }
    }
    out
}

/// Read a cursor's pixels: colour cursors carry their own alpha (or fall
/// back to the AND mask), monochrome ones are AND / XOR masks.
#[allow(clippy::many_single_char_names)] // w, h, x, y, r, g, b, a are the usual names
fn read_cursor(screen: HDC, handle: HCURSOR) -> Option<Cursor> {
    let mut icon = ICONINFO::default();
    // SAFETY: `icon` is a valid out-parameter; the two bitmaps it receives
    // are ours to delete, which `Bitmaps` does.
    unsafe { GetIconInfo(handle.into(), &raw mut icon) }.ok()?;
    let bitmaps = Bitmaps {
        mask: icon.hbmMask,
        color: icon.hbmColor,
    };
    let monochrome = bitmaps.color.is_invalid();
    let (w, mask_h) = bitmap_size(bitmaps.mask)?;
    let h = if monochrome { mask_h / 2 } else { mask_h };
    if w == 0 || h == 0 || w > 256 || h > 256 {
        return None;
    }
    let mask = dib_bits(screen, bitmaps.mask, w, mask_h)?;
    let mut rgba = vec![0u8; w * h * 4];
    // One mask bit per pixel, rows padded to 32 bits in a 1-bpp DIB, but we
    // asked GetDIBits for 32 bpp, so every pixel is a full BGRA value.
    let mask_px = |x: usize, y: usize| mask[(y * w + x) * 4] != 0;
    if monochrome {
        for y in 0..h {
            for x in 0..w {
                let and = mask_px(x, y);
                let xor = mask_px(x, y + h);
                let (value, alpha) = match (and, xor) {
                    (false, true) => (255, 255),
                    (true, false) => (0, 0),
                    // opaque black, and "invert" (which cannot be drawn here) dark too
                    (false, false) | (true, true) => (0, 255),
                };
                let at = (y * w + x) * 4;
                rgba[at..at + 4].copy_from_slice(&[value, value, value, alpha]);
            }
        }
    } else {
        let colour = dib_bits(screen, bitmaps.color, w, h)?;
        let has_alpha = colour.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
        for y in 0..h {
            for x in 0..w {
                let at = (y * w + x) * 4;
                let (b, g, r, a) = (colour[at], colour[at + 1], colour[at + 2], colour[at + 3]);
                let alpha = if has_alpha {
                    a
                } else if mask_px(x, y) {
                    0
                } else {
                    255
                };
                rgba[at..at + 4].copy_from_slice(&[r, g, b, alpha]);
            }
        }
    }
    Some(Cursor {
        hotspot: (
            u16::try_from(icon.xHotspot).unwrap_or(0),
            u16::try_from(icon.yHotspot).unwrap_or(0),
        ),
        width: u16::try_from(w).ok()?,
        height: u16::try_from(h).ok()?,
        rgba,
    })
}

/// The two bitmaps `GetIconInfo` hands over, deleted on drop.
struct Bitmaps {
    mask: HBITMAP,
    color: HBITMAP,
}

impl Drop for Bitmaps {
    fn drop(&mut self) {
        // SAFETY: both were created by GetIconInfo for us; a null one is skipped.
        unsafe {
            if !self.mask.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.mask.0));
            }
            if !self.color.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.color.0));
            }
        }
    }
}

fn bitmap_size(bitmap: HBITMAP) -> Option<(usize, usize)> {
    let mut bm = BITMAP::default();
    // SAFETY: `bm` is a BITMAP of the size passed.
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            i32::try_from(size_of::<BITMAP>()).ok()?,
            Some((&raw mut bm).cast()),
        )
    };
    (got != 0).then_some((
        usize::try_from(bm.bmWidth).ok()?,
        usize::try_from(bm.bmHeight).ok()?,
    ))
}

/// The pixels of `bitmap` as 32-bit top-down BGRA, `w` x `h`.
fn dib_bits(dc: HDC, bitmap: HBITMAP, w: usize, h: usize) -> Option<Vec<u8>> {
    let (wi, hi) = (i32::try_from(w).ok()?, i32::try_from(h).ok()?);
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).ok()?,
            biWidth: wi,
            biHeight: -hi,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let mut bits = vec![0u8; w * h * 4];
    // SAFETY: `bits` holds w * h * 4 bytes, what `h` lines of 32-bit pixels need.
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            u32::try_from(h).ok()?,
            Some(bits.as_mut_ptr().cast()),
            &raw mut info,
            DIB_RGB_COLORS,
        )
    };
    (lines == hi).then_some(bits)
}

/// The clipboard's text, `Ok(None)` if it holds none, `Err` if it could not
/// be opened (another program has it).
fn read_clipboard_text() -> Result<Option<String>, ()> {
    // SAFETY: the clipboard is closed on every path; the global memory block
    // is locked only while its UTF-16 text is copied out.
    unsafe {
        OpenClipboard(None).map_err(|_| ())?;
        let text = (|| {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0)).ok()?;
            let global = windows::Win32::Foundation::HGLOBAL(handle.0);
            let ptr = GlobalLock(global).cast::<u16>();
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 && len < 4 * 1024 * 1024 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(global);
            Some(text.replace("\r\n", "\n"))
        })();
        let _ = CloseClipboard();
        Ok(text)
    }
}
