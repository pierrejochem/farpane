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
