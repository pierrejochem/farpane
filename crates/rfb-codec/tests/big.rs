//! Large rectangles through the zlib-based encoders.
//!
//! Regression: with flate2's `miniz_oxide` backend a sync-flushed chunk of a
//! few hundred kilobytes lost its last bytes, so a 3000 x 40 ZRLE rectangle
//! failed to decode. The crate now uses the `zlib-rs` backend.

#![allow(clippy::cast_possible_truncation)]

use rfb_codec::{Decoder, Framebuffer, PixelPacker, TightEncoder, ZrleEncoder, enc};
use rfb_proto::{PixelFormat, msg::server::RectHeader, types::Rect};

fn gradient(w: u16, h: u16) -> Framebuffer {
    let mut fb = Framebuffer::new(w, h).unwrap();
    let mut wire = Vec::new();
    for y in 0..h {
        for x in 0..w {
            wire.extend_from_slice(&[(x / 2) as u8, (y / 2) as u8, ((x + y) / 4) as u8, 0]);
        }
    }
    let rect = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let raw = RectHeader {
        rect,
        encoding: enc::RAW,
    };
    Decoder::new(&PixelFormat::RGB888)
        .unwrap()
        .decode(&mut fb, &raw, &wire)
        .unwrap();
    fb
}

#[test]
fn large_zrle_and_tight_rectangles_survive_the_sync_flush() {
    let pf = PixelFormat::RGB888;
    let packer = PixelPacker::new(&pf).unwrap();
    for (w, h) in [(64u16, 40u16), (1000, 40), (2048, 64), (2048, 200)] {
        let fb = gradient(w, h);
        let rect = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        for encoding in [enc::ZRLE, enc::TIGHT] {
            let payload = if encoding == enc::ZRLE {
                ZrleEncoder::new().encode(&fb, rect, &packer).unwrap()
            } else {
                TightEncoder::new().encode(&fb, rect, &packer).unwrap()
            };
            let mut shown = Framebuffer::new(w, h).unwrap();
            Decoder::new(&pf)
                .unwrap()
                .decode(&mut shown, &RectHeader { rect, encoding }, &payload)
                .unwrap_or_else(|e| panic!("{w}x{h} encoding {encoding}: {e}"));
            assert_eq!(shown, fb, "{w}x{h} encoding {encoding}");
        }
    }
    // A wider one than Tight allows still works with ZRLE.
    let fb = gradient(3000, 40);
    let rect = Rect {
        x: 0,
        y: 0,
        width: 3000,
        height: 40,
    };
    let payload = ZrleEncoder::new().encode(&fb, rect, &packer).unwrap();
    let mut shown = Framebuffer::new(3000, 40).unwrap();
    Decoder::new(&pf)
        .unwrap()
        .decode(
            &mut shown,
            &RectHeader {
                rect,
                encoding: enc::ZRLE,
            },
            &payload,
        )
        .unwrap();
    assert_eq!(shown, fb);
}

/// A picture with smooth gradients crossed by noisy stripes, so that tiles
/// compress badly in places and well in others.
fn noisy(w: u16, h: u16, seed: u32) -> Framebuffer {
    let mut fb = Framebuffer::new(w, h).unwrap();
    let mut wire = Vec::new();
    let mut state = seed;
    for y in 0..h {
        for x in 0..w {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (state >> 24) as u8;
            let striped = (x / 40 + y / 40) % 3 == 0;
            wire.extend_from_slice(&[
                if striped { noise } else { (x / 3) as u8 },
                if striped { noise ^ 0x55 } else { (y / 3) as u8 },
                if striped {
                    noise.rotate_left(3)
                } else {
                    ((x + y) / 5) as u8
                },
                0,
            ]);
        }
    }
    let rect = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let raw = RectHeader {
        rect,
        encoding: enc::RAW,
    };
    Decoder::new(&PixelFormat::RGB888)
        .unwrap()
        .decode(&mut fb, &raw, &wire)
        .unwrap();
    fb
}

/// Regression: `flate2`'s write encoder could return from a sync flush before
/// a large chunk was fully written (seen with 192 x 192 rectangles of
/// gradient content), which cut the stream. Rectangles of many sizes, one
/// persistent stream per encoding, must all come back exactly.
#[test]
fn many_rectangles_of_many_sizes_on_one_stream_decode_exactly() {
    let pf = PixelFormat::RGB888;
    let packer = PixelPacker::new(&pf).unwrap();
    let source = noisy(600, 500, 7);
    for encoding in [enc::ZRLE, enc::TIGHT] {
        let mut zrle = ZrleEncoder::new();
        let mut tight = TightEncoder::new();
        let mut client = Framebuffer::new(600, 500).unwrap();
        let mut decoder = Decoder::new(&pf).unwrap();
        for (i, (x, y, w, h)) in [
            (0u16, 0u16, 192u16, 192u16),
            (30, 40, 192, 192),
            (100, 10, 256, 300),
            (0, 0, 600, 500),
            (77, 61, 64, 64),
            (200, 200, 400, 300),
            (1, 1, 300, 120),
            (0, 0, 600, 500),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = Rect {
                x,
                y,
                width: w,
                height: h,
            };
            let payload = if encoding == enc::ZRLE {
                zrle.encode(&source, rect, &packer)
            } else {
                tight.encode(&source, rect, &packer)
            }
            .unwrap();
            let header = RectHeader { rect, encoding };
            decoder
                .decode(&mut client, &header, &payload)
                .unwrap_or_else(|e| panic!("encoding {encoding}, rectangle {i} {rect:?}: {e}"));
        }
        assert_eq!(client.pixels(), source.pixels(), "encoding {encoding}");
    }
}
