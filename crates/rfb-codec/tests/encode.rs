//! The server's encoders against the viewer's decoders and framers.

#![allow(
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    clippy::cast_possible_wrap
)]

use proptest::prelude::*;
use rfb_codec::{Decoder, Framebuffer, PixelConverter, PixelPacker, ZrleEncoder, enc, encode_raw};
use rfb_proto::{PixelFormat, framing::frame, msg::server::RectHeader, types::Rect};

fn formats() -> Vec<(&'static str, PixelFormat)> {
    let base = PixelFormat::RGB888;
    vec![
        ("rgb888", base),
        (
            "bgr888 big-endian",
            PixelFormat {
                big_endian: true,
                red_shift: 8,
                green_shift: 16,
                blue_shift: 24,
                ..base
            },
        ),
        (
            "rgb565",
            PixelFormat {
                bits_per_pixel: 16,
                depth: 16,
                red_max: 31,
                green_max: 63,
                blue_max: 31,
                red_shift: 11,
                green_shift: 5,
                blue_shift: 0,
                ..base
            },
        ),
        (
            "rgb565 big-endian",
            PixelFormat {
                bits_per_pixel: 16,
                depth: 16,
                big_endian: true,
                red_max: 31,
                green_max: 63,
                blue_max: 31,
                red_shift: 11,
                green_shift: 5,
                blue_shift: 0,
                ..base
            },
        ),
        (
            "rgb332",
            PixelFormat {
                bits_per_pixel: 8,
                depth: 8,
                red_max: 7,
                green_max: 7,
                blue_max: 3,
                red_shift: 5,
                green_shift: 2,
                blue_shift: 0,
                ..base
            },
        ),
        (
            "30-bit in 32 (full pixels, no 3-byte CPIXEL)",
            PixelFormat {
                depth: 30,
                red_max: 1023,
                green_max: 1023,
                blue_max: 1023,
                red_shift: 20,
                green_shift: 10,
                blue_shift: 0,
                ..base
            },
        ),
        (
            "24-bit colour in the high bytes",
            PixelFormat {
                red_shift: 24,
                green_shift: 16,
                blue_shift: 8,
                ..base
            },
        ),
    ]
}

fn header(rect: Rect, encoding: i32) -> RectHeader {
    RectHeader { rect, encoding }
}

/// A framebuffer filled by `f`, built through the decoder's Raw path.
fn picture(w: u16, h: u16, f: impl Fn(u16, u16) -> [u8; 3]) -> Framebuffer {
    let mut wire = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            wire.extend_from_slice(&[b, g, r, 0]);
        }
    }
    let mut fb = Framebuffer::new(w, h).unwrap();
    let rect = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    Decoder::new(&PixelFormat::RGB888)
        .unwrap()
        .decode(&mut fb, &header(rect, enc::RAW), &wire)
        .unwrap();
    fb
}

/// What the viewer should end up with: each pixel squeezed into `pf` and back.
fn expected(fb: &Framebuffer, rect: Rect, pf: &PixelFormat) -> Vec<[u8; 4]> {
    let packer = PixelPacker::new(pf).unwrap();
    let conv = PixelConverter::new(pf).unwrap();
    let mut out = Vec::new();
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let mut wire = Vec::new();
            packer.pack(fb.pixel(x, y).unwrap(), &mut wire);
            out.push(conv.pixel(&wire));
        }
    }
    out
}

fn assert_matches(
    shown: &Framebuffer,
    source: &Framebuffer,
    rect: Rect,
    pf: &PixelFormat,
    what: &str,
) {
    let want = expected(source, rect, pf);
    let mut i = 0;
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            assert_eq!(shown.pixel(x, y), Some(want[i]), "{what}: pixel {x},{y}");
            i += 1;
        }
    }
}

type Pattern = Box<dyn Fn(u16, u16) -> [u8; 3]>;

fn patterns() -> Vec<(&'static str, Pattern)> {
    vec![
        ("solid", Box::new(|_, _| [10, 20, 30])),
        (
            "stripes",
            Box::new(|x, _| if x % 3 == 0 { [255, 0, 0] } else { [0, 0, 255] }),
        ),
        (
            "few colours",
            Box::new(|x, y| {
                [
                    [0, 0, 0],
                    [255, 255, 255],
                    [200, 30, 30],
                    [30, 200, 30],
                    [30, 30, 200],
                ][usize::from((x / 7 + y / 5) % 5)]
            }),
        ),
        (
            "bands",
            Box::new(|_, y| [(y / 9).wrapping_mul(30) as u8, 40, 200]),
        ),
        (
            "noise",
            Box::new(|x, y| {
                [
                    x.wrapping_mul(37).wrapping_add(y.wrapping_mul(11)) as u8,
                    x.wrapping_mul(5).wrapping_add(y.wrapping_mul(29)) as u8,
                    x.wrapping_mul(y) as u8,
                ]
            }),
        ),
        (
            "gradient",
            Box::new(|x, y| [(x * 2) as u8, (y * 3) as u8, ((x + y) % 256) as u8]),
        ),
    ]
}

#[test]
fn raw_round_trips_in_every_format() {
    for (name, pf) in formats() {
        let packer = PixelPacker::new(&pf).unwrap();
        for (pattern, f) in patterns() {
            let fb = picture(37, 21, &f);
            let rect = Rect {
                x: 3,
                y: 2,
                width: 30,
                height: 17,
            };
            let payload = encode_raw(&fb, rect, &packer).unwrap();
            assert_eq!(
                frame(&header(rect, enc::RAW), &pf, &payload),
                Ok(payload.len())
            );
            let mut shown = Framebuffer::new(37, 21).unwrap();
            Decoder::new(&pf)
                .unwrap()
                .decode(&mut shown, &header(rect, enc::RAW), &payload)
                .unwrap();
            assert_matches(&shown, &fb, rect, &pf, &format!("{name} {pattern}"));
        }
    }
}

#[test]
fn zrle_round_trips_in_every_format_across_several_rectangles_of_one_stream() {
    for (name, pf) in formats() {
        let packer = PixelPacker::new(&pf).unwrap();
        let mut encoder = ZrleEncoder::new();
        let mut decoder = Decoder::new(&pf).unwrap();
        let mut shown = Framebuffer::new(150, 90).unwrap();
        for (pattern, f) in patterns() {
            let fb = picture(150, 90, &f);
            // A rectangle with partial tiles on every edge and one that is tiny.
            for rect in [
                Rect {
                    x: 5,
                    y: 3,
                    width: 133,
                    height: 71,
                },
                Rect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                Rect {
                    x: 64,
                    y: 64,
                    width: 64,
                    height: 26,
                },
            ] {
                let payload = encoder.encode(&fb, rect, &packer).unwrap();
                assert_eq!(
                    frame(&header(rect, enc::ZRLE), &pf, &payload),
                    Ok(payload.len()),
                    "{name} {pattern}"
                );
                decoder
                    .decode(&mut shown, &header(rect, enc::ZRLE), &payload)
                    .unwrap();
                assert_matches(
                    &shown,
                    &fb,
                    rect,
                    &pf,
                    &format!("{name} {pattern} {rect:?}"),
                );
            }
        }
    }
}

#[test]
fn zrle_compresses_flat_pictures_well() {
    let pf = PixelFormat::RGB888;
    let packer = PixelPacker::new(&pf).unwrap();
    let fb = picture(640, 480, |x, y| {
        if (x / 80 + y / 60) % 2 == 0 {
            [250, 250, 250]
        } else {
            [20, 20, 90]
        }
    });
    let rect = Rect {
        x: 0,
        y: 0,
        width: 640,
        height: 480,
    };
    let payload = ZrleEncoder::new().encode(&fb, rect, &packer).unwrap();
    assert!(payload.len() < 2_000, "{} bytes", payload.len());
}

proptest! {
    /// Random small-palette pictures: every sub-encoding gets used somewhere.
    #[test]
    fn zrle_round_trips_random_pictures(
        w in 1u16..140,
        h in 1u16..140,
        colours in 1usize..200,
        run in 1u16..80,
        seed in any::<u32>(),
        fmt in 0usize..7,
    ) {
        let (_, pf) = formats().swap_remove(fmt);
        let colour = move |i: u32| {
            let v = i.wrapping_mul(2_654_435_761).wrapping_add(seed);
            [(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
        };
        let fb = picture(w, h, |x, y| colour((u32::from(x / run) + u32::from(y) * 7) % colours as u32));
        let rect = Rect { x: 0, y: 0, width: w, height: h };
        let packer = PixelPacker::new(&pf).unwrap();
        let payload = ZrleEncoder::new().encode(&fb, rect, &packer).unwrap();
        prop_assert_eq!(frame(&header(rect, enc::ZRLE), &pf, &payload), Ok(payload.len()));
        let mut shown = Framebuffer::new(w, h).unwrap();
        Decoder::new(&pf).unwrap().decode(&mut shown, &header(rect, enc::ZRLE), &payload).unwrap();
        let want = expected(&fb, rect, &pf);
        let mut i = 0;
        for y in 0..h {
            for x in 0..w {
                prop_assert_eq!(shown.pixel(x, y), Some(want[i]));
                i += 1;
            }
        }
    }
}
