//! Screen capture through DXGI Desktop Duplication.
//!
//! Faster than GDI and it says which parts changed, so the server does not
//! have to compare the whole picture. One duplication per monitor; their
//! pictures are copied into the one buffer that spans the virtual desktop.
//! It can refuse (a desktop switch, a mode change, the secure desktop for a
//! process without SYSTEM rights, a driver without support such as some
//! virtual display adapters, a rotated or HDR monitor); the caller then goes
//! back to GDI and tries again later.

use std::io;

use rfb_proto::types::Rect;
use windows::{
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
                ID3D11Texture2D,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY},
                CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
                DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTPUT_DESC, IDXGIAdapter, IDXGIAdapter1,
                IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
            },
        },
    },
    core::Interface,
};

/// More rectangles than this in one step are merged into their bounding box.
const MAX_RECTS: usize = 64;

/// One monitor's duplication.
struct Output {
    dup: IDXGIOutputDuplication,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    staging: Option<ID3D11Texture2D>,
    /// Where the monitor sits in the picture, and its size.
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    /// Nothing has been copied yet, so the first frame counts as all changed.
    fresh: bool,
}

/// Desktop Duplication over all monitors.
pub(crate) struct Duplicator {
    outputs: Vec<Output>,
    /// Width of the whole picture in pixels.
    width: usize,
}

impl std::fmt::Debug for Duplicator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Duplicator")
            .field("outputs", &self.outputs.len())
            .finish_non_exhaustive()
    }
}

// SAFETY: the COM objects here are created and used by one thread at a time;
// the server moves the source to its capture thread once and uses it only
// there. The D3D11 device is created without the single-threaded flag and its
// immediate context is only used from that thread.
unsafe impl Send for Duplicator {}

fn other(what: &str, e: &windows::core::Error) -> io::Error {
    io::Error::other(format!("{what}: {e}"))
}

impl Duplicator {
    /// Duplicate every monitor attached to the desktop whose picture is
    /// `width` x `height` pixels with its top left at `origin`.
    ///
    /// # Errors
    /// No monitor can be duplicated, or one is rotated, not 8-bit colour, or
    /// outside the picture.
    pub(crate) fn new(origin: (i32, i32), width: u16, height: u16) -> io::Result<Self> {
        // SAFETY: plain factory creation; the interfaces own their references.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|e| other("DXGI factory", &e))?;
        let mut outputs = Vec::new();
        let mut adapter_index = 0;
        // SAFETY: EnumAdapters1 has no preconditions; it fails past the last adapter.
        while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
            adapter_index += 1;
            outputs.extend(adapter_outputs(&adapter, origin, width, height)?);
        }
        if outputs.is_empty() {
            return Err(io::Error::other("no monitor can be duplicated"));
        }
        Ok(Self {
            outputs,
            width: usize::from(width),
        })
    }

    /// Copy what changed into `frame` (`B, G, R, X`, the whole picture, no
    /// padding) and return the changed rectangles.
    ///
    /// # Errors
    /// The duplication is gone (mode change, desktop switch, lost access);
    /// build a new [`Duplicator`].
    pub(crate) fn grab(&mut self, frame: &mut [u8]) -> io::Result<Vec<Rect>> {
        let mut changed = Vec::new();
        for output in &mut self.outputs {
            output.grab(frame, self.width, &mut changed)?;
        }
        if changed.len() > MAX_RECTS {
            changed = vec![bounding_box(&changed)];
        }
        Ok(changed)
    }
}

fn bounding_box(rects: &[Rect]) -> Rect {
    let x0 = rects.iter().map(|r| r.x).min().unwrap_or(0);
    let y0 = rects.iter().map(|r| r.y).min().unwrap_or(0);
    let x1 = rects.iter().map(|r| r.x + r.width).max().unwrap_or(0);
    let y1 = rects.iter().map(|r| r.y + r.height).max().unwrap_or(0);
    Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

fn adapter_outputs(
    adapter: &IDXGIAdapter1,
    origin: (i32, i32),
    width: u16,
    height: u16,
) -> io::Result<Vec<Output>> {
    let mut found = Vec::new();
    let base: IDXGIAdapter = adapter.cast().map_err(|e| other("adapter", &e))?;
    let mut device = None;
    let mut context = None;
    // SAFETY: the out pointers are valid; the adapter is a live interface.
    unsafe {
        D3D11CreateDevice(
            &base,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&raw mut device),
            None,
            Some(&raw mut context),
        )
    }
    .map_err(|e| other("Direct3D device", &e))?;
    let (Some(device), Some(context)) = (device, context) else {
        return Err(io::Error::other("Direct3D gave no device"));
    };
    let mut index = 0;
    // SAFETY: EnumOutputs has no preconditions; it fails past the last output.
    while let Ok(output) = unsafe { adapter.EnumOutputs(index) } {
        index += 1;
        // SAFETY: a live output interface.
        let desc: DXGI_OUTPUT_DESC =
            unsafe { output.GetDesc() }.map_err(|e| other("output description", &e))?;
        if !desc.AttachedToDesktop.as_bool() {
            continue;
        }
        if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY {
            return Err(io::Error::other("a rotated monitor is not duplicated"));
        }
        let RECT {
            left,
            top,
            right,
            bottom,
        } = desc.DesktopCoordinates;
        let (x, y) = (left - origin.0, top - origin.1);
        if x < 0
            || y < 0
            || right - origin.0 > i32::from(width)
            || bottom - origin.1 > i32::from(height)
        {
            return Err(io::Error::other("a monitor lies outside the desktop"));
        }
        let output1: IDXGIOutput1 = output.cast().map_err(|e| other("output", &e))?;
        // SAFETY: `device` is a live Direct3D device on this adapter.
        let dup = unsafe { output1.DuplicateOutput(&device) }
            .map_err(|e| other("DuplicateOutput", &e))?;
        found.push(Output {
            dup,
            context: context.clone(),
            device: device.clone(),
            staging: None,
            x: usize::try_from(x).unwrap_or(0),
            y: usize::try_from(y).unwrap_or(0),
            width: usize::try_from(right - left).unwrap_or(0),
            height: usize::try_from(bottom - top).unwrap_or(0),
            fresh: true,
        });
    }
    Ok(found)
}

impl Output {
    fn grab(
        &mut self,
        frame: &mut [u8],
        full_width: usize,
        changed: &mut Vec<Rect>,
    ) -> io::Result<()> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: valid out pointers; a zero timeout never blocks.
        match unsafe {
            self.dup
                .AcquireNextFrame(0, &raw mut info, &raw mut resource)
        } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(()),
            Err(e) => return Err(other("AcquireNextFrame", &e)),
        }
        // The frame must be released whatever happens below.
        let result = if info.LastPresentTime == 0 {
            Ok(()) // only the pointer moved
        } else {
            resource.map_or_else(
                || Err(io::Error::other("no desktop image")),
                |r| self.copy(&r, &info, frame, full_width, changed),
            )
        };
        // SAFETY: a frame was acquired above.
        let _ = unsafe { self.dup.ReleaseFrame() };
        result
    }

    fn copy(
        &mut self,
        resource: &IDXGIResource,
        info: &DXGI_OUTDUPL_FRAME_INFO,
        frame: &mut [u8],
        full_width: usize,
        changed: &mut Vec<Rect>,
    ) -> io::Result<()> {
        let texture: ID3D11Texture2D = resource.cast().map_err(|e| other("desktop image", &e))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: valid out pointer.
        unsafe { texture.GetDesc(&raw mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(io::Error::other("the monitor is not 8-bit BGRA (HDR?)"));
        }
        let rects = if self.fresh {
            vec![RECT {
                left: 0,
                top: 0,
                right: i32::try_from(self.width).unwrap_or(0),
                bottom: i32::try_from(self.height).unwrap_or(0),
            }]
        } else {
            self.changed_rects(info)?
        };
        if rects.is_empty() {
            return Ok(());
        }
        if self.staging.is_none() {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging = None;
            // SAFETY: `staging_desc` is valid and `staging` is a valid out pointer.
            unsafe {
                self.device
                    .CreateTexture2D(&raw const staging_desc, None, Some(&raw mut staging))
            }
            .map_err(|e| other("staging texture", &e))?;
            self.staging = staging;
        }
        let Some(staging) = &self.staging else {
            return Err(io::Error::other("no staging texture"));
        };
        // SAFETY: both textures are live and the same size and format.
        unsafe { self.context.CopyResource(staging, &texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture allows CPU reads; `mapped` is a valid out pointer.
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
        }
        .map_err(|e| other("Map", &e))?;
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData.cast::<u8>().cast_const();
        for r in &rects {
            let (x0, y0) = (clamp(r.left, self.width), clamp(r.top, self.height));
            let (x1, y1) = (clamp(r.right, self.width), clamp(r.bottom, self.height));
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let row = (x1 - x0) * 4;
            for y in y0..y1 {
                let to = ((self.y + y) * full_width + self.x + x0) * 4;
                // SAFETY: the mapping holds `height` rows of `pitch` bytes and
                // `y < height`, `x1 <= width`, so the row lies inside it.
                let from = unsafe { std::slice::from_raw_parts(base.add(y * pitch + x0 * 4), row) };
                frame[to..to + row].copy_from_slice(from);
            }
            changed.push(Rect {
                x: u16::try_from(self.x + x0).unwrap_or(0),
                y: u16::try_from(self.y + y0).unwrap_or(0),
                width: u16::try_from(x1 - x0).unwrap_or(0),
                height: u16::try_from(y1 - y0).unwrap_or(0),
            });
        }
        // SAFETY: the subresource was mapped above.
        unsafe { self.context.Unmap(staging, 0) };
        self.fresh = false;
        Ok(())
    }

    /// The dirty and moved-to rectangles of the frame just acquired.
    fn changed_rects(&self, info: &DXGI_OUTDUPL_FRAME_INFO) -> io::Result<Vec<RECT>> {
        let total = info.TotalMetadataBufferSize as usize;
        if total == 0 {
            return Ok(Vec::new());
        }
        let mut moves = vec![
            DXGI_OUTDUPL_MOVE_RECT::default();
            total / size_of::<DXGI_OUTDUPL_MOVE_RECT>() + 1
        ];
        let mut needed = 0u32;
        // SAFETY: the buffer holds `size` bytes as told; `needed` is a valid out pointer.
        unsafe {
            self.dup.GetFrameMoveRects(
                u32::try_from(moves.len() * size_of::<DXGI_OUTDUPL_MOVE_RECT>()).unwrap_or(0),
                moves.as_mut_ptr(),
                &raw mut needed,
            )
        }
        .map_err(|e| other("GetFrameMoveRects", &e))?;
        moves.truncate(needed as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>());
        let mut dirty = vec![RECT::default(); total / size_of::<RECT>() + 1];
        let mut needed = 0u32;
        // SAFETY: as above.
        unsafe {
            self.dup.GetFrameDirtyRects(
                u32::try_from(dirty.len() * size_of::<RECT>()).unwrap_or(0),
                dirty.as_mut_ptr(),
                &raw mut needed,
            )
        }
        .map_err(|e| other("GetFrameDirtyRects", &e))?;
        dirty.truncate(needed as usize / size_of::<RECT>());
        // The texture already holds the moved pixels, so only where they
        // landed has to be sent again.
        dirty.extend(moves.iter().map(|m| m.DestinationRect));
        Ok(dirty)
    }
}

fn clamp(v: i32, max: usize) -> usize {
    usize::try_from(v).unwrap_or(0).min(max)
}
