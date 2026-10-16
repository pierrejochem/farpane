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

#[test]
fn cursor_round_trips_through_the_decoder_in_every_format() {
    use rfb_codec::{Cursor, Damage, encode_cursor};
    // 11 pixels wide so the mask rows need padding; a mix of opaque and clear.
    let (w, h) = (11u16, 5u16);
    let mut rgba = Vec::new();
    for i in 0..usize::from(w) * usize::from(h) {
        let opaque = i % 3 != 0;
        rgba.extend_from_slice(&[
            (i * 20) as u8,
            (i * 7) as u8,
            200,
            if opaque { 255 } else { 0 },
        ]);
    }
    let cursor = Cursor {
        hotspot: (3, 2),
        width: w,
        height: h,
        rgba,
    };
    for (name, pf) in formats() {
        let packer = PixelPacker::new(&pf).unwrap();
        let conv = PixelConverter::new(&pf).unwrap();
        let payload = encode_cursor(&cursor, &packer).unwrap();
        let rect = Rect {
            x: 3,
            y: 2,
            width: w,
            height: h,
        };
        assert_eq!(
            frame(
                &header(rect, rfb_proto::consts::pseudo::CURSOR),
                &pf,
                &payload
            ),
            Ok(payload.len()),
            "{name}"
        );
        let mut fb = Framebuffer::new(16, 16).unwrap();
        let Damage::Cursor(got) = Decoder::new(&pf)
            .unwrap()
            .decode(
                &mut fb,
                &header(rect, rfb_proto::consts::pseudo::CURSOR),
                &payload,
            )
            .unwrap()
        else {
            panic!("not a cursor");
        };
        assert_eq!(
            (got.hotspot, got.width, got.height),
            ((3, 2), w, h),
            "{name}"
        );
        for (i, (want, have)) in cursor
            .rgba
            .chunks_exact(4)
            .zip(got.rgba.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(have[3], want[3], "{name}: opacity of pixel {i}");
            let mut wire = Vec::new();
            packer.pack([want[2], want[1], want[0], 0xFF], &mut wire);
            let [b, g, r, _] = conv.pixel(&wire);
            assert_eq!(&have[..3], &[r, g, b], "{name}: colour of pixel {i}");
        }
    }
    // A hidden cursor has no payload at all.
    let none = Cursor {
        hotspot: (0, 0),
        width: 0,
        height: 0,
        rgba: Vec::new(),
    };
    let packer = PixelPacker::new(&PixelFormat::RGB888).unwrap();
    assert_eq!(encode_cursor(&none, &packer).unwrap(), Vec::<u8>::new());
    let bad = Cursor {
        rgba: vec![0; 3],
        ..cursor
    };
    assert!(encode_cursor(&bad, &packer).is_err());
}

#[test]
fn trle_round_trips_in_every_format_with_reuse_across_tiles() {
    use rfb_codec::encode_trle;
    for (name, pf) in formats() {
        let packer = PixelPacker::new(&pf).unwrap();
        let mut decoder = Decoder::new(&pf).unwrap();
        let mut shown = Framebuffer::new(150, 90).unwrap();
        for (pattern, f) in patterns() {
            let fb = picture(150, 90, &f);
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
                    x: 16,
                    y: 16,
                    width: 16,
                    height: 16,
                },
                Rect {
                    x: 64,
                    y: 64,
                    width: 64,
                    height: 26,
                },
            ] {
                let payload = encode_trle(&fb, rect, &packer).unwrap();
                assert_eq!(
                    frame(&header(rect, enc::TRLE), &pf, &payload),
                    Ok(payload.len()),
                    "{name} {pattern}: the framer agrees with the encoder"
                );
                decoder
                    .decode(&mut shown, &header(rect, enc::TRLE), &payload)
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

proptest! {
    #[test]
    fn trle_round_trips_random_pictures(
        w in 1u16..100,
        h in 1u16..100,
        colours in 1usize..40,
        run in 1u16..30,
        seed in any::<u32>(),
        fmt in 0usize..7,
    ) {
        use rfb_codec::encode_trle;
        let (_, pf) = formats().swap_remove(fmt);
        let colour = move |i: u32| {
            let v = i.wrapping_mul(2_654_435_761).wrapping_add(seed);
            [(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
        };
        let fb = picture(w, h, |x, y| colour((u32::from(x / run) + u32::from(y / 3)) % colours as u32));
        let rect = Rect { x: 0, y: 0, width: w, height: h };
        let packer = PixelPacker::new(&pf).unwrap();
        let payload = encode_trle(&fb, rect, &packer).unwrap();
        prop_assert_eq!(frame(&header(rect, enc::TRLE), &pf, &payload), Ok(payload.len()));
        let mut shown = Framebuffer::new(w, h).unwrap();
        Decoder::new(&pf).unwrap().decode(&mut shown, &header(rect, enc::TRLE), &payload).unwrap();
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

#[test]
fn hextile_round_trips_in_every_format() {
    use rfb_codec::encode_hextile;
    for (name, pf) in formats() {
        let packer = PixelPacker::new(&pf).unwrap();
        let mut decoder = Decoder::new(&pf).unwrap();
        let mut shown = Framebuffer::new(150, 90).unwrap();
        for (pattern, f) in patterns() {
            let fb = picture(150, 90, &f);
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
                    x: 16,
                    y: 16,
                    width: 16,
                    height: 16,
                },
                Rect {
                    x: 64,
                    y: 64,
                    width: 64,
                    height: 26,
                },
            ] {
                let payload = encode_hextile(&fb, rect, &packer).unwrap();
                assert_eq!(
                    frame(&header(rect, enc::HEXTILE), &pf, &payload),
                    Ok(payload.len()),
                    "{name} {pattern}: the framer agrees with the encoder"
                );
                decoder
                    .decode(&mut shown, &header(rect, enc::HEXTILE), &payload)
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
fn hextile_sends_a_repeated_background_only_once() {
    use rfb_codec::encode_hextile;
    let pf = PixelFormat::RGB888;
    let packer = PixelPacker::new(&pf).unwrap();
    let fb = picture(32, 16, |_, _| [9, 8, 7]);
    let payload = encode_hextile(
        &fb,
        Rect {
            x: 0,
            y: 0,
            width: 32,
            height: 16,
        },
        &packer,
    )
    .unwrap();
    // Tile one: background specified, then the colour (B G R X). Tile two: nothing.
    assert_eq!(payload, [2, 7, 8, 9, 0, 0]);
}

#[test]
fn hextile_two_colours_use_uncoloured_subrectangles() {
    use rfb_codec::encode_hextile;
    let pf = PixelFormat::RGB888;
    let packer = PixelPacker::new(&pf).unwrap();
    // 16 x 16 white with one 3 x 2 black block at (4, 5).
    let fb = picture(16, 16, |x, y| {
        if (4..7).contains(&x) && (5..7).contains(&y) {
            [0, 0, 0]
        } else {
            [255; 3]
        }
    });
    let payload = encode_hextile(
        &fb,
        Rect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        },
        &packer,
    )
    .unwrap();
    // Background and foreground specified, any subrects (2 + 4 + 8 = 14), one
    // subrect at x=4 y=5, width 3 height 2.
    assert_eq!(payload, [14, 255, 255, 255, 0, 0, 0, 0, 0, 1, 0x45, 0x21]);
}

proptest! {
    #[test]
    fn hextile_round_trips_random_pictures(
        w in 1u16..70,
        h in 1u16..70,
        colours in 1usize..12,
        run in 1u16..20,
        seed in any::<u32>(),
        fmt in 0usize..7,
    ) {
        use rfb_codec::encode_hextile;
        let (_, pf) = formats().swap_remove(fmt);
        let colour = move |i: u32| {
            let v = i.wrapping_mul(2_654_435_761).wrapping_add(seed);
            [(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
        };
        let fb = picture(w, h, |x, y| colour((u32::from(x / run) + u32::from(y / 2) * 3) % colours as u32));
        let rect = Rect { x: 0, y: 0, width: w, height: h };
        let packer = PixelPacker::new(&pf).unwrap();
        let payload = encode_hextile(&fb, rect, &packer).unwrap();
        prop_assert_eq!(frame(&header(rect, enc::HEXTILE), &pf, &payload), Ok(payload.len()));
        let mut shown = Framebuffer::new(w, h).unwrap();
        Decoder::new(&pf).unwrap().decode(&mut shown, &header(rect, enc::HEXTILE), &payload).unwrap();
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

#[test]
fn tight_round_trips_in_every_format_across_rectangles_of_one_connection() {
    use rfb_codec::TightEncoder;
    for (name, pf) in formats() {
        let packer = PixelPacker::new(&pf).unwrap();
        let mut encoder = TightEncoder::new();
        let mut decoder = Decoder::new(&pf).unwrap();
        let mut shown = Framebuffer::new(150, 90).unwrap();
        for (pattern, f) in patterns() {
            let fb = picture(150, 90, &f);
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
                    x: 16,
                    y: 16,
                    width: 16,
                    height: 16,
                },
                Rect {
                    x: 64,
                    y: 64,
                    width: 64,
                    height: 26,
                },
                Rect {
                    x: 0,
                    y: 0,
                    width: 150,
                    height: 3,
                },
            ] {
                let payload = encoder.encode(&fb, rect, &packer).unwrap();
                assert_eq!(
                    frame(&header(rect, enc::TIGHT), &pf, &payload),
                    Ok(payload.len()),
                    "{name} {pattern}: the framer agrees with the encoder"
                );
                decoder
                    .decode(&mut shown, &header(rect, enc::TIGHT), &payload)
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
fn tight_picks_fill_palette_or_copy_by_what_the_picture_holds() {
    use rfb_codec::TightEncoder;
    let pf = PixelFormat::RGB888;
    let packer = PixelPacker::new(&pf).unwrap();
    let mut encoder = TightEncoder::new();
    let all = Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 1,
    };
    // One colour: fill, a TPIXEL of R, G, B.
    let fb = picture(8, 1, |_, _| [1, 2, 3]);
    assert_eq!(encoder.encode(&fb, all, &packer).unwrap(), [0x80, 1, 2, 3]);
    // Two colours, 8 x 1: palette filter on stream 1, 1 bit per pixel, under
    // 12 bytes so not deflated.
    let fb = picture(8, 1, |x, _| if x < 3 { [255, 0, 0] } else { [0, 0, 255] });
    assert_eq!(
        encoder.encode(&fb, all, &packer).unwrap(),
        [0x50, 1, 1, 255, 0, 0, 0, 0, 255, 0b0001_1111]
    );
    // Noise: the copy filter, stream 0.
    let fb = picture(8, 1, |x, _| {
        [(x * 37) as u8, (x * 91) as u8, (x * 13 + 5) as u8]
    });
    let out = encoder.encode(&fb, all, &packer).unwrap();
    assert_eq!(out[0], 0x00);
    // 8 x 1 x 3 = 24 bytes: deflated, with a compact length.
    assert_eq!(usize::from(out[1]), out.len() - 2);
}

#[test]
fn tight_refuses_rectangles_wider_than_2048_and_outside_the_picture() {
    use rfb_codec::{TIGHT_MAX_WIDTH, TightEncoder};
    let packer = PixelPacker::new(&PixelFormat::RGB888).unwrap();
    let fb = Framebuffer::new(2100, 2).unwrap();
    let mut encoder = TightEncoder::new();
    let wide = Rect {
        x: 0,
        y: 0,
        width: TIGHT_MAX_WIDTH + 1,
        height: 1,
    };
    assert!(encoder.encode(&fb, wide, &packer).is_err());
    let ok = Rect {
        x: 0,
        y: 0,
        width: TIGHT_MAX_WIDTH,
        height: 2,
    };
    assert!(encoder.encode(&fb, ok, &packer).is_ok());
    let outside = Rect {
        x: 2000,
        y: 0,
        width: 200,
        height: 1,
    };
    assert!(encoder.encode(&fb, outside, &packer).is_err());
}

proptest! {
    #[test]
    fn tight_round_trips_random_pictures(
        w in 1u16..90,
        h in 1u16..90,
        colours in 1usize..300,
        run in 1u16..20,
        seed in any::<u32>(),
        fmt in 0usize..7,
    ) {
        use rfb_codec::TightEncoder;
        let (_, pf) = formats().swap_remove(fmt);
        let colour = move |i: u32| {
            let v = i.wrapping_mul(2_654_435_761).wrapping_add(seed);
            [(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
        };
        let fb = picture(w, h, |x, y| colour((u32::from(x / run) + u32::from(y) * 7) % colours as u32));
        let rect = Rect { x: 0, y: 0, width: w, height: h };
        let packer = PixelPacker::new(&pf).unwrap();
        let payload = TightEncoder::new().encode(&fb, rect, &packer).unwrap();
        prop_assert_eq!(frame(&header(rect, enc::TIGHT), &pf, &payload), Ok(payload.len()));
        let mut shown = Framebuffer::new(w, h).unwrap();
        Decoder::new(&pf).unwrap().decode(&mut shown, &header(rect, enc::TIGHT), &payload).unwrap();
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
