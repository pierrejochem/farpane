//! Tight: the encoder here follows the spec text, independent of the decoder.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::needless_range_loop
)]

use flate2::{Compress, Compression, FlushCompress};
use rfb_codec::{Damage, DecodeError, Decoder, Framebuffer, enc};
use rfb_proto::{PixelFormat, framing::frame, msg::server::RectHeader, types::Rect};

fn header(x: u16, y: u16, w: u16, h: u16) -> RectHeader {
    RectHeader {
        rect: Rect {
            x,
            y,
            width: w,
            height: h,
        },
        encoding: enc::TIGHT,
    }
}

fn rgb565() -> PixelFormat {
    PixelFormat {
        bits_per_pixel: 16,
        depth: 16,
        red_max: 31,
        green_max: 63,
        blue_max: 31,
        red_shift: 11,
        green_shift: 5,
        blue_shift: 0,
        ..PixelFormat::RGB888
    }
}

/// One of the server's four zlib streams, sync-flushed after each chunk.
struct Stream(Compress);

impl Stream {
    fn new() -> Self {
        Self(Compress::new(Compression::default(), true))
    }

    fn compress(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 64);
        let start = self.0.total_in();
        loop {
            let done = (self.0.total_in() - start) as usize;
            out.reserve(1024);
            self.0
                .compress_vec(&data[done..], &mut out, FlushCompress::Sync)
                .unwrap();
            if (self.0.total_in() - start) as usize == data.len() && out.len() < out.capacity() {
                return out;
            }
        }
    }
}

/// The spec's compact length: 7 bits per byte, least significant first.
fn compact(len: usize) -> Vec<u8> {
    let mut out = vec![(len & 0x7F) as u8];
    if len > 0x7F {
        out[0] |= 0x80;
        out.push(((len >> 7) & 0x7F) as u8);
        if len > 0x3FFF {
            out[1] |= 0x80;
            out.push((len >> 14) as u8);
        }
    }
    out
}

/// Basic compression: `control` byte (with stream bits), optional filter id
/// and palette `prefix`, then the data, deflated when it is 12 bytes or more.
fn basic(control: u8, prefix: &[u8], data: &[u8], stream: &mut Stream) -> Vec<u8> {
    let mut p = vec![control];
    p.extend_from_slice(prefix);
    if data.len() < 12 {
        p.extend_from_slice(data);
    } else {
        let z = stream.compress(data);
        p.extend(compact(z.len()));
        p.extend(z);
    }
    p
}

fn decode(
    dec: &mut Decoder,
    fb: &mut Framebuffer,
    pf: &PixelFormat,
    h: &RectHeader,
    payload: &[u8],
) -> Result<Damage, DecodeError> {
    assert_eq!(frame(h, pf, payload), Ok(payload.len()), "framing agrees");
    dec.decode(fb, h, payload)
}

/// An RGB test image.
fn image(w: usize, h: usize) -> Vec<[u8; 3]> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            [
                (x * 9 + y * 3) as u8,
                (255 - x * 5 + y * 11) as u8,
                (x * y + 40) as u8,
            ]
        })
        .collect()
}

fn expect(fb: &Framebuffer, x0: u16, y0: u16, w: usize, img: &[[u8; 3]]) {
    for (i, [r, g, b]) in img.iter().enumerate() {
        let (x, y) = (i % w, i / w);
        assert_eq!(
            fb.pixel(x0 + x as u16, y0 + y as u16),
            Some([*b, *g, *r, 255]),
            "pixel {x},{y}"
        );
    }
}

#[test]
fn fill_paints_the_rectangle_with_one_tpixel() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(10, 10).unwrap();
    let h = header(2, 3, 4, 5);
    decode(&mut dec, &mut fb, &pf, &h, &[0x80, 10, 20, 30]).unwrap();
    assert_eq!(fb.pixel(2, 3), Some([30, 20, 10, 255]));
    assert_eq!(fb.pixel(5, 7), Some([30, 20, 10, 255]));
    assert_eq!(fb.pixel(6, 7), Some([0, 0, 0, 255]), "outside is untouched");
}

#[test]
fn copy_filter_small_and_compressed_and_across_chunks_of_one_stream() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(40, 40).unwrap();
    // 2 x 1: 6 bytes, sent as is.
    let img = image(2, 1);
    let data: Vec<u8> = img.iter().flatten().copied().collect();
    decode(
        &mut dec,
        &mut fb,
        &pf,
        &header(0, 0, 2, 1),
        &basic(0, &[], &data, &mut Stream::new()),
    )
    .unwrap();
    expect(&fb, 0, 0, 2, &img);
    // Larger rectangles on stream 1, one stream across three rectangles.
    let mut stream = Stream::new();
    for round in 0..3u16 {
        let img = image(16, 8 + usize::from(round));
        let data: Vec<u8> = img.iter().flatten().copied().collect();
        let h = header(4, round * 10, 16, 8 + round);
        let payload = basic(0x10, &[], &data, &mut stream);
        decode(&mut dec, &mut fb, &pf, &h, &payload).unwrap();
        expect(&fb, 4, round * 10, 16, &img);
    }
}

#[test]
fn the_four_streams_are_independent_and_reset_bits_start_them_over() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(32, 32).unwrap();
    let img = image(12, 12);
    let data: Vec<u8> = img.iter().flatten().copied().collect();
    let h = header(0, 0, 12, 12);
    let mut streams = [Stream::new(), Stream::new(), Stream::new(), Stream::new()];
    // Prime stream 0 and stream 3 with different histories.
    for id in [0usize, 3] {
        let payload = basic((id as u8) << 4, &[], &data, &mut streams[id]);
        decode(&mut dec, &mut fb, &pf, &h, &payload).unwrap();
    }
    // Stream 0 again, then the server resets it (bit 0) and starts a new one.
    let payload = basic(0, &[], &data, &mut streams[0]);
    decode(&mut dec, &mut fb, &pf, &h, &payload).unwrap();
    streams[0] = Stream::new();
    let payload = basic(0x01, &[], &data, &mut streams[0]);
    decode(&mut dec, &mut fb, &pf, &h, &payload).unwrap();
    expect(&fb, 0, 0, 12, &img);
    // Stream 3 was not reset and still continues.
    let payload = basic(0x30, &[], &data, &mut streams[3]);
    decode(&mut dec, &mut fb, &pf, &h, &payload).unwrap();
    // A reset also happens for a fill rectangle.
    decode(
        &mut dec,
        &mut fb,
        &pf,
        &header(0, 0, 1, 1),
        &[0x88, 1, 2, 3],
    )
    .unwrap();
    streams[3] = Stream::new();
    let payload = basic(0x30, &[], &data, &mut streams[3]);
    decode(&mut dec, &mut fb, &pf, &h, &payload).unwrap();
    expect(&fb, 0, 0, 12, &img);
}

#[test]
fn palette_with_two_colours_is_one_bit_per_pixel_rows_padded() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(32, 32).unwrap();
    let (w, h) = (11usize, 3usize);
    let colours = [[255u8, 0, 0], [0, 0, 255]];
    let index = |x: usize, y: usize| (x + y) % 2;
    let mut data = Vec::new();
    for y in 0..h {
        let mut row = vec![0u8; w.div_ceil(8)];
        for x in 0..w {
            if index(x, y) == 1 {
                row[x / 8] |= 0x80 >> (x % 8); // leftmost pixel in the top bit
            }
        }
        data.extend(row);
    }
    let mut prefix = vec![1, 1]; // filter id 1, colours - 1
    prefix.extend(colours.iter().flatten());
    let payload = basic(0x40, &prefix, &data, &mut Stream::new());
    decode(
        &mut dec,
        &mut fb,
        &pf,
        &header(0, 0, w as u16, h as u16),
        &payload,
    )
    .unwrap();
    let img: Vec<[u8; 3]> = (0..w * h).map(|i| colours[index(i % w, i / w)]).collect();
    expect(&fb, 0, 0, w, &img);
}

#[test]
fn palette_with_many_colours_is_one_byte_per_pixel() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(32, 32).unwrap();
    let (w, h, n) = (20usize, 9usize, 200usize);
    let palette: Vec<[u8; 3]> = (0..n)
        .map(|i| [i as u8, (i * 3) as u8, 255 - i as u8])
        .collect();
    let indices: Vec<u8> = (0..w * h).map(|i| ((i * 7) % n) as u8).collect();
    let mut prefix = vec![1, (n - 1) as u8];
    prefix.extend(palette.iter().flatten());
    let payload = basic(0x60, &prefix, &indices, &mut Stream::new()); // stream 2
    decode(
        &mut dec,
        &mut fb,
        &pf,
        &header(0, 0, w as u16, h as u16),
        &payload,
    )
    .unwrap();
    let img: Vec<[u8; 3]> = indices.iter().map(|&i| palette[usize::from(i)]).collect();
    expect(&fb, 0, 0, w, &img);
}

#[test]
fn a_palette_index_past_the_palette_is_an_error() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(8, 8).unwrap();
    let mut prefix = vec![1, 2]; // three colours
    prefix.extend([0u8; 9]);
    let payload = basic(0x40, &prefix, &[0, 1, 2, 3], &mut Stream::new());
    assert_eq!(
        dec.decode(&mut fb, &header(0, 0, 4, 1), &payload),
        Err(DecodeError::Malformed("Tight palette index"))
    );
}

/// The forward gradient filter from the spec, for one component plane.
fn gradient_encode(values: &[Vec<u32>], max: u32) -> Vec<Vec<u32>> {
    let (h, w) = (values.len(), values[0].len());
    let v = |i: isize, j: isize| -> i64 {
        if i < 0 || j < 0 {
            0
        } else {
            i64::from(values[i as usize][j as usize])
        }
    };
    let mut out = vec![vec![0u32; w]; h];
    for i in 0..h {
        for j in 0..w {
            let (i, j) = (i as isize, j as isize);
            let p = (v(i - 1, j) + v(i, j - 1) - v(i - 1, j - 1)).clamp(0, i64::from(max));
            out[i as usize][j as usize] = ((v(i, j) - p).rem_euclid(i64::from(max) + 1)) as u32;
        }
    }
    out
}

#[test]
fn gradient_filter_on_24_bit_tpixels() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(40, 40).unwrap();
    let (w, h) = (23usize, 17usize);
    let img = image(w, h);
    let planes: Vec<Vec<Vec<u32>>> = (0..3)
        .map(|k| {
            let v: Vec<Vec<u32>> = (0..h)
                .map(|y| (0..w).map(|x| u32::from(img[y * w + x][k])).collect())
                .collect();
            gradient_encode(&v, 255)
        })
        .collect();
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            for plane in &planes {
                data.push(plane[y][x] as u8);
            }
        }
    }
    let payload = basic(0x40, &[2], &data, &mut Stream::new());
    decode(
        &mut dec,
        &mut fb,
        &pf,
        &header(3, 4, w as u16, h as u16),
        &payload,
    )
    .unwrap();
    expect(&fb, 3, 4, w, &img);
}

#[test]
fn gradient_filter_on_16_bit_pixels() {
    let pf = rgb565();
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(40, 40).unwrap();
    let (w, h) = (9usize, 7usize);
    let maxes = [31u32, 63, 31];
    let comp = |x: usize, y: usize, k: usize| ((x * 3 + y * 5 + k * 11) as u32) % (maxes[k] + 1);
    let planes: Vec<Vec<Vec<u32>>> = (0..3)
        .map(|k| {
            let v: Vec<Vec<u32>> = (0..h)
                .map(|y| (0..w).map(|x| comp(x, y, k)).collect())
                .collect();
            gradient_encode(&v, maxes[k])
        })
        .collect();
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let raw = planes[0][y][x] << 11 | planes[1][y][x] << 5 | planes[2][y][x];
            data.extend((raw as u16).to_le_bytes());
        }
    }
    let payload = basic(0x40, &[2], &data, &mut Stream::new());
    decode(
        &mut dec,
        &mut fb,
        &pf,
        &header(0, 0, w as u16, h as u16),
        &payload,
    )
    .unwrap();
    for y in 0..h {
        for x in 0..w {
            let s = |k: usize| ((comp(x, y, k) * 255 + maxes[k] / 2) / maxes[k]) as u8;
            assert_eq!(
                fb.pixel(x as u16, y as u16),
                Some([s(2), s(1), s(0), 255]),
                "{x},{y}"
            );
        }
    }
}

#[test]
fn copy_filter_with_a_16_bit_format_uses_whole_pixels() {
    let pf = rgb565();
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(8, 8).unwrap();
    let data: Vec<u8> = [0xF800u16, 0x07E0, 0x001F, 0xFFFF]
        .iter()
        .flat_map(|p| p.to_le_bytes())
        .collect();
    decode(
        &mut dec,
        &mut fb,
        &pf,
        &header(0, 0, 4, 1),
        &basic(0, &[], &data, &mut Stream::new()),
    )
    .unwrap();
    assert_eq!(fb.pixel(0, 0), Some([0, 0, 255, 255]));
    assert_eq!(fb.pixel(1, 0), Some([0, 255, 0, 255]));
    assert_eq!(fb.pixel(2, 0), Some([255, 0, 0, 255]));
    assert_eq!(fb.pixel(3, 0), Some([255, 255, 255, 255]));
}

#[test]
fn basic_compression_without_zlib_carries_the_data_as_is() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(16, 16).unwrap();
    let img = image(6, 5);
    let data: Vec<u8> = img.iter().flatten().copied().collect();
    let mut p = vec![0xA0];
    p.extend(&data);
    decode(&mut dec, &mut fb, &pf, &header(1, 1, 6, 5), &p).unwrap();
    expect(&fb, 1, 1, 6, &img);
    // With an explicit copy filter.
    let mut p = vec![0xE0, 0];
    p.extend(&data);
    decode(&mut dec, &mut fb, &pf, &header(1, 1, 6, 5), &p).unwrap();
    expect(&fb, 1, 1, 6, &img);
}

#[test]
fn jpeg_and_damage_are_errors_not_panics() {
    let pf = PixelFormat::RGB888;
    let mut dec = Decoder::new(&pf).unwrap();
    let mut fb = Framebuffer::new(16, 16).unwrap();
    let h = header(0, 0, 4, 4);
    assert!(matches!(
        dec.decode(&mut fb, &h, &[0x90, 2, 0xFF, 0xD8]),
        Err(DecodeError::Malformed(_))
    ));
    assert!(matches!(
        dec.decode(&mut fb, &h, &[0xB0]),
        Err(DecodeError::Malformed(_))
    ));
    assert!(matches!(
        dec.decode(&mut fb, &h, &[0x40, 7]),
        Err(DecodeError::Malformed(_))
    ));
    // Trailing bytes after a fill.
    assert!(matches!(
        dec.decode(&mut fb, &h, &[0x80, 1, 2, 3, 4]),
        Err(DecodeError::Malformed(_))
    ));
    // Data that inflates to the wrong size.
    let mut stream = Stream::new();
    let z = stream.compress(&[0u8; 20]);
    let mut p = vec![0x00];
    p.extend(compact(z.len()));
    p.extend(z);
    assert!(dec.decode(&mut fb, &h, &p).is_err());
    // Outside the framebuffer.
    assert_eq!(
        dec.decode(&mut fb, &header(10, 10, 20, 20), &[0x80, 1, 2, 3]),
        Err(DecodeError::OutOfBounds)
    );
    // Gradient on an 8 bpp format.
    let pf8 = PixelFormat {
        bits_per_pixel: 8,
        depth: 8,
        red_max: 7,
        green_max: 7,
        blue_max: 3,
        red_shift: 5,
        green_shift: 2,
        blue_shift: 0,
        ..PixelFormat::RGB888
    };
    let mut dec8 = Decoder::new(&pf8).unwrap();
    assert!(matches!(
        dec8.decode(&mut fb, &h, &[0x40, 2]),
        Err(DecodeError::Malformed(_))
    ));
}

mod robustness {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Whatever the bytes, framing and decoding return errors, never panic,
        /// and when framing succeeds the decoder sees a complete payload.
        #[test]
        fn random_payloads_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..300),
            w in 0u16..40,
            h in 0u16..40,
            wide in any::<bool>(),
        ) {
            let pf = if wide { PixelFormat::RGB888 } else { rgb565() };
            let hdr = header(0, 0, w, h);
            if let Ok(n) = frame(&hdr, &pf, &bytes) {
                let mut dec = Decoder::new(&pf).unwrap();
                let mut fb = Framebuffer::new(40, 40).unwrap();
                let _ = dec.decode(&mut fb, &hdr, &bytes[..n]);
            }
        }
    }
}
