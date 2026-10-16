use proptest::prelude::*;
use rfb_codec::{Damage, DecodeError, Decoder, Framebuffer, PixelConverter, enc};
use rfb_proto::{PixelFormat, consts::pseudo, msg::server::RectHeader, types::Rect};

fn header(encoding: i32, x: u16, y: u16, w: u16, h: u16) -> RectHeader {
    RectHeader {
        rect: Rect {
            x,
            y,
            width: w,
            height: h,
        },
        encoding,
    }
}

fn pf(bpp: u8, big_endian: bool, max: [u16; 3], shift: [u8; 3]) -> PixelFormat {
    PixelFormat {
        bits_per_pixel: bpp,
        depth: bpp.min(24),
        big_endian,
        true_colour: true,
        red_max: max[0],
        green_max: max[1],
        blue_max: max[2],
        red_shift: shift[0],
        green_shift: shift[1],
        blue_shift: shift[2],
    }
}

#[test]
fn converts_common_formats() {
    // 32 bpp little-endian XRGB: the framebuffer layout itself.
    let c = PixelConverter::new(&PixelFormat::RGB888).unwrap();
    assert_eq!(c.pixel(&[0x11, 0x22, 0x33, 0x00]), [0x11, 0x22, 0x33, 0xFF]);

    // 32 bpp big-endian XRGB: bytes X, R, G, B.
    let c = PixelConverter::new(&pf(32, true, [255; 3], [16, 8, 0])).unwrap();
    assert_eq!(c.pixel(&[0, 0x33, 0x22, 0x11]), [0x11, 0x22, 0x33, 0xFF]);

    // 32 bpp little-endian XBGR (red in the low byte).
    let c = PixelConverter::new(&pf(32, false, [255; 3], [0, 8, 16])).unwrap();
    assert_eq!(c.pixel(&[0x33, 0x22, 0x11, 0]), [0x11, 0x22, 0x33, 0xFF]);

    // 16 bpp RGB565 little-endian: white, black, pure red.
    let c = PixelConverter::new(&pf(16, false, [31, 63, 31], [11, 5, 0])).unwrap();
    assert_eq!(c.pixel(&[0xFF, 0xFF]), [255, 255, 255, 0xFF]);
    assert_eq!(c.pixel(&[0, 0]), [0, 0, 0, 0xFF]);
    assert_eq!(c.pixel(&[0x00, 0xF8]), [0, 0, 255, 0xFF]);

    // 8 bpp BGR233: blue 2 bits at 6, green 3 at 3, red 3 at 0.
    let c = PixelConverter::new(&pf(8, false, [7, 7, 3], [0, 3, 6])).unwrap();
    assert_eq!(c.pixel(&[0b11_000_000]), [255, 0, 0, 0xFF]);
    assert_eq!(c.pixel(&[0b00_111_000]), [0, 255, 0, 0xFF]);
    assert_eq!(c.pixel(&[0b00_000_111]), [0, 0, 255, 0xFF]);
}

#[test]
fn rejects_formats_it_cannot_convert() {
    let mut mapped = PixelFormat::RGB888;
    mapped.true_colour = false;
    assert!(matches!(
        PixelConverter::new(&mapped),
        Err(DecodeError::UnsupportedFormat(_))
    ));
    let mut zero = PixelFormat::RGB888;
    zero.green_max = 0;
    assert!(PixelConverter::new(&zero).is_err());
    let mut shifted = pf(16, false, [31, 63, 31], [11, 5, 0]);
    shifted.red_shift = 16;
    assert!(PixelConverter::new(&shifted).is_err());
}

#[test]
fn raw_fills_the_rectangle_and_reports_damage() {
    let mut fb = Framebuffer::new(4, 3).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    // 2 x 2 at (1, 1): blue, green / red, white (wire order B, G, R, X).
    let payload = [
        255, 0, 0, 0, 0, 255, 0, 0, //
        0, 0, 255, 0, 255, 255, 255, 0,
    ];
    let h = header(enc::RAW, 1, 1, 2, 2);
    assert_eq!(dec.decode(&mut fb, &h, &payload), Ok(Damage::Rect(h.rect)));
    assert_eq!(fb.pixel(1, 1), Some([255, 0, 0, 255]));
    assert_eq!(fb.pixel(2, 1), Some([0, 255, 0, 255]));
    assert_eq!(fb.pixel(1, 2), Some([0, 0, 255, 255]));
    assert_eq!(fb.pixel(2, 2), Some([255, 255, 255, 255]));
    assert_eq!(fb.pixel(0, 0), Some([0, 0, 0, 255]), "outside is untouched");
    assert_eq!(fb.pixel(4, 0), None);
}

#[test]
fn raw_in_16_bpp_and_after_a_format_switch() {
    let mut fb = Framebuffer::new(2, 1).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    dec.set_pixel_format(&pf(16, false, [31, 63, 31], [11, 5, 0]))
        .unwrap();
    let h = header(enc::RAW, 0, 0, 2, 1);
    dec.decode(&mut fb, &h, &[0x00, 0xF8, 0xFF, 0xFF]).unwrap();
    assert_eq!(fb.pixel(0, 0), Some([0, 0, 255, 255]));
    assert_eq!(fb.pixel(1, 0), Some([255, 255, 255, 255]));
}

#[test]
fn raw_rejects_bad_sizes_and_bounds() {
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    assert_eq!(
        dec.decode(&mut fb, &header(enc::RAW, 0, 0, 2, 2), &[0; 15]),
        Err(DecodeError::Malformed("Raw payload size"))
    );
    assert_eq!(
        dec.decode(&mut fb, &header(enc::RAW, 3, 0, 2, 1), &[0; 8]),
        Err(DecodeError::OutOfBounds)
    );
    // Empty rectangles are fine while they sit inside the framebuffer.
    assert!(
        dec.decode(&mut fb, &header(enc::RAW, 0, 0, 0, 4), &[])
            .is_ok()
    );
    assert!(
        dec.decode(&mut fb, &header(enc::RAW, 0, 0, 4, 0), &[])
            .is_ok()
    );
    assert_eq!(
        dec.decode(&mut fb, &header(enc::RAW, 0, 0, 0, 5), &[]),
        Err(DecodeError::OutOfBounds)
    );
}

#[test]
fn copy_rect_moves_pixels_even_when_areas_overlap() {
    // 4 x 1 strip 1,2,3,4 (blue channel), copy x0..3 -> x1..4.
    let mut fb = Framebuffer::new(5, 1).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    let raw: Vec<u8> = (1..=4u8).flat_map(|v| [v, 0, 0, 0]).collect();
    dec.decode(&mut fb, &header(enc::RAW, 0, 0, 4, 1), &raw)
        .unwrap();
    dec.decode(&mut fb, &header(enc::COPY_RECT, 1, 0, 4, 1), &[0, 0, 0, 0])
        .unwrap();
    let blues: Vec<u8> = (0..5).map(|x| fb.pixel(x, 0).unwrap()[0]).collect();
    assert_eq!(blues, [1, 1, 2, 3, 4]);
}

#[test]
fn copy_rect_rejects_bad_payloads_and_sources() {
    let mut fb = Framebuffer::new(10, 10).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    let h = header(enc::COPY_RECT, 0, 0, 4, 4);
    assert!(matches!(
        dec.decode(&mut fb, &h, &[0, 0, 0]),
        Err(DecodeError::Malformed(_))
    ));
    assert!(matches!(
        dec.decode(&mut fb, &h, &[0, 0, 0, 0, 0]),
        Err(DecodeError::Malformed(_))
    ));
    // Source 8,8 + 4 x 4 leaves the framebuffer.
    assert_eq!(
        dec.decode(&mut fb, &h, &[0, 8, 0, 8]),
        Err(DecodeError::OutOfBounds)
    );
}

#[test]
fn desktop_size_resizes_and_clears_and_other_pseudo_rects_are_quiet() {
    let mut fb = Framebuffer::new(2, 2).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    dec.decode(&mut fb, &header(enc::RAW, 0, 0, 1, 1), &[9, 9, 9, 0])
        .unwrap();
    assert_eq!(
        dec.decode(&mut fb, &header(pseudo::DESKTOP_SIZE, 0, 0, 8, 6), &[]),
        Ok(Damage::Resized)
    );
    assert_eq!((fb.width(), fb.height()), (8, 6));
    assert_eq!(fb.pixel(0, 0), Some([0, 0, 0, 255]));
    assert_eq!(
        dec.decode(&mut fb, &header(pseudo::LAST_RECT, 0, 0, 0, 0), &[]),
        Ok(Damage::None)
    );
    assert_eq!(
        dec.decode(&mut fb, &header(enc::TIGHT, 0, 0, 1, 1), &[]),
        Err(DecodeError::UnsupportedEncoding(enc::TIGHT))
    );
    assert!(Framebuffer::new(20_000, 1).is_err());
}

proptest! {
    /// CopyRect equals "read the whole source area first, then write it".
    #[test]
    fn copy_rect_matches_a_naive_reference(
        (w, h) in (1u16..12, 1u16..12),
        seed in prop::collection::vec(any::<u8>(), 12 * 12 * 4),
        src in (0u16..12, 0u16..12),
        dst in (0u16..12, 0u16..12),
        size in (1u16..12, 1u16..12),
    ) {
        let (cw, ch) = (size.0.min(w - src.0.min(w - 1)).min(w - dst.0.min(w - 1)),
                        size.1.min(h - src.1.min(h - 1)).min(h - dst.1.min(h - 1)));
        let (sx, sy) = (src.0.min(w - 1), src.1.min(h - 1));
        let (dx, dy) = (dst.0.min(w - 1), dst.1.min(h - 1));
        let mut fb = Framebuffer::new(w, h).unwrap();
        let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
        let raw: Vec<u8> = seed.iter().copied().cycle().take(usize::from(w) * usize::from(h) * 4).collect();
        dec.decode(&mut fb, &header(enc::RAW, 0, 0, w, h), &raw).unwrap();
        let before = fb.clone();

        let mut want = before.clone();
        let mut tmp = Vec::new();
        for y in 0..ch {
            for x in 0..cw {
                tmp.push(((dx + x, dy + y), before.pixel(sx + x, sy + y).unwrap()));
            }
        }
        // `want` is rebuilt from `before` through Raw writes of single pixels.
        let mut wdec = Decoder::new(&PixelFormat::RGB888).unwrap();
        for ((x, y), p) in tmp {
            wdec.decode(&mut want, &header(enc::RAW, x, y, 1, 1), &[p[0], p[1], p[2], 0]).unwrap();
        }

        let [sx_hi, sx_lo] = sx.to_be_bytes();
        let [sy_hi, sy_lo] = sy.to_be_bytes();
        let payload = [sx_hi, sx_lo, sy_hi, sy_lo];
        dec.decode(&mut fb, &header(enc::COPY_RECT, dx, dy, cw, ch), &payload).unwrap();
        prop_assert_eq!(fb, want);
    }
}

#[test]
fn rgba_copies_a_rect_in_texture_order() {
    let mut fb = Framebuffer::new(3, 2).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    // Wire pixels are B, G, R, X: (1,0) is red 10 / green 20 / blue 30.
    dec.decode(&mut fb, &header(enc::RAW, 1, 0, 1, 1), &[30, 20, 10, 0])
        .unwrap();
    let all = fb
        .rgba(Rect {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        })
        .unwrap();
    assert_eq!(all.len(), 3 * 2 * 4);
    assert_eq!(&all[4..8], &[10, 20, 30, 255]);
    assert_eq!(&all[0..4], &[0, 0, 0, 255]);
    // A sub-rectangle is tightly packed.
    let sub = fb
        .rgba(Rect {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        })
        .unwrap();
    assert_eq!(sub, [10, 20, 30, 255, 0, 0, 0, 255]);
    assert_eq!(
        fb.rgba(Rect {
            x: 2,
            y: 1,
            width: 2,
            height: 1
        }),
        Err(DecodeError::OutOfBounds)
    );
}

#[test]
fn cursor_decodes_pixels_and_mask_with_the_hotspot() {
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    // 10 x 2 cursor, hotspot (3, 1). Wire pixels are B, G, R, X: pixel i has
    // red = i + 1, green 0, blue 0 (so wire bytes [0, 0, i + 1, 0]).
    let mut payload = Vec::new();
    for i in 0..20u8 {
        payload.extend([0, 0, i + 1, 0]);
    }
    // Mask rows are 2 bytes (10 bits, padded). Row 0: bits 1,0,0,0,0,0,0,0 | 0,1.
    payload.extend([0b1000_0000, 0b0100_0000]);
    payload.extend([0b0000_0000, 0b0000_0000]);
    let h = header(pseudo::CURSOR, 3, 1, 10, 2);
    let damage = dec.decode(&mut fb, &h, &payload).unwrap();
    let Damage::Cursor(c) = damage else {
        panic!("{damage:?}")
    };
    assert_eq!((c.hotspot, c.width, c.height), ((3, 1), 10, 2));
    assert_eq!(c.rgba.len(), 10 * 2 * 4);
    let px = |x: usize, y: usize| &c.rgba[(y * 10 + x) * 4..(y * 10 + x) * 4 + 4];
    assert_eq!(
        px(0, 0),
        [1, 0, 0, 255],
        "mask bit 7 of byte 0 is the leftmost pixel"
    );
    assert_eq!(px(1, 0), [2, 0, 0, 0], "clear bit: transparent");
    assert_eq!(
        px(9, 0),
        [10, 0, 0, 255],
        "second mask byte starts at pixel 8"
    );
    assert_eq!(px(8, 0), [9, 0, 0, 0]);
    assert_eq!(px(0, 1), [11, 0, 0, 0], "second row is fully transparent");
    assert_eq!(
        fb,
        Framebuffer::new(4, 4).unwrap(),
        "the picture is untouched"
    );
}

#[test]
fn hidden_and_malformed_cursors() {
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    // 0 x 0 hides the cursor: no pixels, no mask.
    let Ok(Damage::Cursor(c)) = dec.decode(&mut fb, &header(pseudo::CURSOR, 0, 0, 0, 0), &[])
    else {
        panic!("expected a cursor");
    };
    assert_eq!(c.rgba.len(), 0);
    // Wrong payload size: pixels but no mask.
    assert_eq!(
        dec.decode(&mut fb, &header(pseudo::CURSOR, 0, 0, 2, 2), &[0; 16]),
        Err(DecodeError::Malformed("Cursor payload size"))
    );
    // A cursor may be larger than the framebuffer and its position is a
    // hotspot, not a location: no bounds check.
    let payload = vec![0u8; 8 * 8 * 4 + 8];
    assert!(
        dec.decode(
            &mut fb,
            &header(pseudo::CURSOR, 60_000, 60_000, 8, 8),
            &payload
        )
        .is_ok()
    );
}

#[test]
fn cursor_uses_the_session_pixel_format() {
    let pf16 = PixelFormat {
        bits_per_pixel: 16,
        depth: 16,
        red_max: 31,
        green_max: 63,
        blue_max: 31,
        red_shift: 11,
        green_shift: 5,
        blue_shift: 0,
        ..PixelFormat::RGB888
    };
    let mut dec = Decoder::new(&pf16).unwrap();
    // One pure-red RGB565 pixel (little-endian 0xF800), opaque.
    let payload = [0x00, 0xF8, 0b1000_0000];
    let Ok(Damage::Cursor(c)) = dec.decode(
        &mut Framebuffer::new(1, 1).unwrap(),
        &header(pseudo::CURSOR, 0, 0, 1, 1),
        &payload,
    ) else {
        panic!("expected a cursor");
    };
    assert_eq!(c.rgba, [255, 0, 0, 255]);
}

fn ext_payload(screens: &[rfb_proto::types::Screen]) -> Vec<u8> {
    let mut p = vec![u8::try_from(screens.len()).unwrap(), 0, 0, 0];
    for s in screens {
        s.write(&mut p);
    }
    p
}

#[test]
fn an_extended_layout_resizes_the_picture_and_reports_the_screens() {
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    dec.decode(&mut fb, &header(enc::RAW, 0, 0, 1, 1), &[9, 9, 9, 0])
        .unwrap();
    let screens = [rfb_proto::types::Screen {
        id: 3,
        x: 0,
        y: 0,
        width: 8,
        height: 6,
        flags: 0,
    }];
    // Reason 1 (we asked), status 0 (granted), new size 8 x 6.
    let h = header(pseudo::EXT_DESKTOP_SIZE, 1, 0, 8, 6);
    let Ok(Damage::Layout(l)) = dec.decode(&mut fb, &h, &ext_payload(&screens)) else {
        panic!("expected a layout");
    };
    assert_eq!((l.reason, l.status, l.width, l.height), (1, 0, 8, 6));
    assert!(l.resized);
    assert_eq!(l.screens, screens);
    assert_eq!((fb.width(), fb.height()), (8, 6));
    assert_eq!(
        fb.pixel(0, 0),
        Some([0, 0, 0, 255]),
        "a resize clears the picture"
    );
}

#[test]
fn repeating_the_same_size_does_not_clear_the_picture() {
    // Servers send the layout on every full refresh.
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    dec.decode(&mut fb, &header(enc::RAW, 0, 0, 1, 1), &[9, 9, 9, 0])
        .unwrap();
    let h = header(pseudo::EXT_DESKTOP_SIZE, 0, 0, 4, 4);
    let Ok(Damage::Layout(l)) = dec.decode(&mut fb, &h, &ext_payload(&[])) else {
        panic!("expected a layout");
    };
    assert!(!l.resized);
    assert_eq!(fb.pixel(0, 0), Some([9, 9, 9, 255]), "the picture survived");
}

#[test]
fn a_refusal_never_resizes_whatever_its_size_fields_say() {
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    for status in [1u16, 2, 3, 4, 99] {
        let h = header(pseudo::EXT_DESKTOP_SIZE, 1, status, 9999, 9999);
        let Ok(Damage::Layout(l)) = dec.decode(&mut fb, &h, &ext_payload(&[])) else {
            panic!("expected a layout");
        };
        assert_eq!((l.status, l.resized), (status, false));
        assert_eq!((fb.width(), fb.height()), (4, 4));
    }
}

#[test]
fn a_layout_payload_must_match_its_screen_count() {
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    let h = header(pseudo::EXT_DESKTOP_SIZE, 0, 0, 4, 4);
    let mut short = ext_payload(&[rfb_proto::types::Screen::default()]);
    short.pop();
    assert!(matches!(
        dec.decode(&mut fb, &h, &short),
        Err(DecodeError::Malformed(_))
    ));
    let mut long = ext_payload(&[]);
    long.push(0);
    assert!(matches!(
        dec.decode(&mut fb, &h, &long),
        Err(DecodeError::Malformed(_))
    ));
    // A too large size is refused by the framebuffer limit, not allocated.
    let h = header(pseudo::EXT_DESKTOP_SIZE, 0, 0, 30_000, 30_000);
    assert!(dec.decode(&mut fb, &h, &ext_payload(&[])).is_err());
}
