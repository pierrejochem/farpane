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
