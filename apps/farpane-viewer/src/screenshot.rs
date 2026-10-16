use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use rfb_codec::{Framebuffer, enc};
use rfb_proto::consts::pseudo;

/// Write the framebuffer to `path`: a binary PPM if the name ends in `.ppm`
/// (exact and easy to compare), otherwise an RGB PNG.
///
/// # Errors
/// I/O or encoding errors.
pub fn write_image(fb: &Framebuffer, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let rgb: Vec<u8> = fb
        .pixels()
        .chunks_exact(4)
        .flat_map(|bgrx| [bgrx[2], bgrx[1], bgrx[0]])
        .collect();
    let mut out = BufWriter::new(File::create(path)?);
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ppm"))
    {
        write!(out, "P6\n{} {}\n255\n", fb.width(), fb.height())?;
        out.write_all(&rgb)?;
        return Ok(());
    }
    let mut encoder = png::Encoder::new(out, u32::from(fb.width()), u32::from(fb.height()));
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

/// Parse a comma-separated encoding list such as `zrle,hextile,raw`.
///
/// # Errors
/// The name of the first unknown encoding.
pub fn parse_encodings(list: &str) -> Result<Vec<i32>, String> {
    list.split(',')
        .map(|name| match name.trim().to_ascii_lowercase().as_str() {
            "raw" => Ok(enc::RAW),
            "copyrect" => Ok(enc::COPY_RECT),
            "rre" => Ok(enc::RRE),
            "corre" => Ok(enc::CO_RRE),
            "hextile" => Ok(enc::HEXTILE),
            "zlib" => Ok(enc::ZLIB),
            "zrle" => Ok(enc::ZRLE),
            "tight" => Ok(enc::TIGHT),
            "trle" => Ok(enc::TRLE),
            "cursor" => Ok(pseudo::CURSOR),
            "desktopsize" => Ok(pseudo::DESKTOP_SIZE),
            "extdesktopsize" => Ok(pseudo::EXT_DESKTOP_SIZE),
            "extclipboard" => Ok(pseudo::EXT_CLIPBOARD),
            "fence" => Ok(pseudo::FENCE),
            other => Err(format!("unknown encoding {other:?}")),
        })
        .collect()
}
