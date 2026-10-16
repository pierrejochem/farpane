use alloc::{vec, vec::Vec};

use super::*;
use crate::types::Rect;

fn header(encoding: i32, width: u16, height: u16) -> RectHeader {
    RectHeader {
        rect: Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        encoding,
    }
}

const PF32: PixelFormat = PixelFormat::RGB888;

fn pf16() -> PixelFormat {
    PixelFormat {
        bits_per_pixel: 16,
        depth: 16,
        red_max: 31,
        green_max: 63,
        blue_max: 31,
        red_shift: 11,
        green_shift: 5,
        blue_shift: 0,
        ..PF32
    }
}

/// `payload` frames to its own length, trailing bytes are left alone, and
/// every proper prefix asks for more input.
fn check(h: &RectHeader, pf: &PixelFormat, payload: &[u8]) {
    assert_eq!(frame(h, pf, payload), Ok(payload.len()), "whole payload");
    let mut with_next = payload.to_vec();
    with_next.extend_from_slice(&[0xAA; 7]);
    assert_eq!(
        frame(h, pf, &with_next),
        Ok(payload.len()),
        "next rect follows"
    );
    for cut in 0..payload.len() {
        assert!(
            matches!(frame(h, pf, &payload[..cut]), Err(ProtoError::NeedMore(n)) if n > 0),
            "prefix {cut} of {}",
            payload.len()
        );
    }
}

#[test]
fn raw_is_width_height_bpp() {
    check(&header(enc::RAW, 3, 2), &PF32, &[0; 24]);
    check(&header(enc::RAW, 3, 2), &pf16(), &[0; 12]);
    check(&header(enc::RAW, 0, 5), &PF32, &[]);
}

#[test]
fn need_more_reports_exact_shortfall_for_fixed_sizes() {
    assert_eq!(
        frame(&header(enc::RAW, 3, 2), &PF32, &[0; 20]),
        Err(ProtoError::NeedMore(4))
    );
    assert_eq!(
        frame(&header(enc::COPY_RECT, 1, 1), &PF32, &[0; 1]),
        Err(ProtoError::NeedMore(3))
    );
}

#[test]
fn copy_rect_is_four_bytes() {
    check(&header(enc::COPY_RECT, 10, 10), &PF32, &[0, 5, 0, 6]);
}

#[test]
fn rre_counts_subrects() {
    // count 2, background, then 2 x (pixel + x y w h as U16).
    let mut p = vec![0, 0, 0, 2];
    p.extend_from_slice(&[1, 2, 3, 4]);
    p.extend_from_slice(&[0; 2 * (4 + 8)]);
    check(&header(enc::RRE, 50, 50), &PF32, &p);
    // 16 bpp: pixel is 2 bytes.
    let mut p = vec![0, 0, 0, 1, 9, 9];
    p.extend_from_slice(&[0; 2 + 8]);
    check(&header(enc::RRE, 50, 50), &pf16(), &p);
}

#[test]
fn co_rre_uses_byte_coordinates() {
    let mut p = vec![0, 0, 0, 3, 1, 2, 3, 4];
    p.extend_from_slice(&[0; 3 * (4 + 4)]);
    check(&header(enc::CO_RRE, 50, 50), &PF32, &p);
    // Zero subrects: header and background only.
    check(&header(enc::CO_RRE, 5, 5), &PF32, &[0, 0, 0, 0, 1, 2, 3, 4]);
}

#[test]
fn zlib_and_zrle_are_length_prefixed() {
    for e in [enc::ZLIB, enc::ZRLE] {
        check(&header(e, 64, 64), &PF32, &[0, 0, 0, 3, 0x78, 0x9C, 0x00]);
        check(&header(e, 64, 64), &PF32, &[0, 0, 0, 0]);
    }
}

#[test]
fn cursor_is_pixels_plus_bitmask() {
    // 10 x 3 at 32 bpp: 30 pixels (120 bytes) + 3 rows of 2 mask bytes.
    check(&header(pseudo::CURSOR, 10, 3), &PF32, &[0; 120 + 6]);
    // 8 wide needs 1 mask byte per row, 9 wide needs 2.
    check(&header(pseudo::CURSOR, 8, 2), &pf16(), &[0; 32 + 2]);
    check(&header(pseudo::CURSOR, 9, 2), &pf16(), &[0; 36 + 4]);
}

#[test]
fn size_pseudo_rects_have_no_payload() {
    for e in [pseudo::DESKTOP_SIZE, pseudo::LAST_RECT] {
        assert_eq!(frame(&header(e, 800, 600), &PF32, &[]), Ok(0));
    }
}

#[test]
fn hextile_single_raw_tile() {
    // 4 x 2 tile, raw bit: mask + 8 pixels.
    let mut p = vec![hextile::RAW];
    p.extend_from_slice(&[0; 32]);
    check(&header(enc::HEXTILE, 4, 2), &PF32, &p);
}

#[test]
fn hextile_flag_combinations() {
    let h = header(enc::HEXTILE, 16, 16);
    // Solid background only.
    let mut p = vec![hextile::BACKGROUND_SPECIFIED];
    p.extend_from_slice(&[1, 2, 3, 4]);
    check(&h, &PF32, &p);
    // Nothing specified: carried over from the previous tile.
    check(&h, &PF32, &[0]);
    // Background + foreground + 2 plain subrects (2 bytes each).
    let mut p =
        vec![hextile::BACKGROUND_SPECIFIED | hextile::FOREGROUND_SPECIFIED | hextile::ANY_SUBRECTS];
    p.extend_from_slice(&[0; 8]);
    p.push(2);
    p.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]);
    check(&h, &PF32, &p);
    // Coloured subrects: pixel + 2 bytes each.
    let mut p = vec![hextile::ANY_SUBRECTS | hextile::SUBRECTS_COLOURED, 2];
    p.extend_from_slice(&[0; 2 * (4 + 2)]);
    check(&h, &PF32, &p);
    // 255 subrects is the maximum count byte.
    let mut p = vec![hextile::ANY_SUBRECTS, 255];
    p.extend_from_slice(&[0; 255 * 2]);
    check(&h, &PF32, &p);
}

#[test]
fn hextile_walks_all_tiles_with_edge_sizes() {
    // 20 x 17 -> 2 x 2 tiles: 16x16, 4x16, 16x1, 4x1.
    let mut p = vec![hextile::RAW];
    p.extend_from_slice(&[0; 16 * 16 * 4]);
    p.push(hextile::BACKGROUND_SPECIFIED);
    p.extend_from_slice(&[0; 4]);
    p.push(hextile::RAW);
    p.extend_from_slice(&[0; 16 * 4]);
    p.push(0);
    check(&header(enc::HEXTILE, 20, 17), &PF32, &p);
    // Raw edge tile sizes at 16 bpp: 4 x 1.
    let mut p = vec![0, hextile::RAW];
    p.extend_from_slice(&[0; 8]);
    check(&header(enc::HEXTILE, 20, 1), &pf16(), &p[..]);
}

#[test]
fn hextile_rejects_foreground_with_coloured_subrects() {
    let p = [
        hextile::FOREGROUND_SPECIFIED | hextile::SUBRECTS_COLOURED,
        0,
        0,
        0,
        0,
    ];
    assert!(matches!(
        frame(&header(enc::HEXTILE, 16, 16), &PF32, &p),
        Err(ProtoError::Malformed { .. })
    ));
}

#[test]
fn zero_sized_hextile_has_no_tiles() {
    assert_eq!(frame(&header(enc::HEXTILE, 0, 10), &PF32, &[]), Ok(0));
}

#[test]
fn byte_by_byte_stream_of_mixed_rects() {
    // Frame consecutive rects out of one growing buffer, as a connection will.
    let rects: Vec<(RectHeader, Vec<u8>)> = vec![
        (header(enc::RAW, 2, 1), vec![1; 8]),
        (header(pseudo::LAST_RECT, 0, 0), vec![]),
        (header(enc::COPY_RECT, 4, 4), vec![0, 1, 0, 2]),
        (header(enc::ZRLE, 8, 8), vec![0, 0, 0, 2, 9, 9]),
    ];
    let stream: Vec<u8> = rects.iter().flat_map(|(_, p)| p.clone()).collect();
    let (mut buf, mut done) = (Vec::new(), 0);
    for b in stream {
        buf.push(b);
        // Drain every rect that is now complete, including empty ones.
        while let Some((h, _)) = rects.get(done) {
            match frame(h, &PF32, &buf) {
                Ok(n) => {
                    buf.drain(..n);
                    done += 1;
                }
                Err(ProtoError::NeedMore(_)) => break,
                Err(e) => panic!("{e}"),
            }
        }
    }
    assert_eq!(done, rects.len());
    assert_eq!(buf.len(), 0);
}

#[test]
fn oversized_payloads_are_refused_before_buffering() {
    let limit = u64::from(limits::MAX_RECT_PAYLOAD);
    // Raw 16384 x 16384 x 4 bytes = 1 GiB.
    assert!(matches!(
        frame(&header(enc::RAW, 16_384, 16_384), &PF32, &[]),
        Err(ProtoError::LimitExceeded { limit: l, .. }) if l == limit
    ));
    // Length-prefixed with a hostile length.
    assert!(matches!(
        frame(&header(enc::ZRLE, 8, 8), &PF32, &u32::MAX.to_be_bytes()),
        Err(ProtoError::LimitExceeded { .. })
    ));
    // RRE with a hostile subrect count.
    assert!(matches!(
        frame(&header(enc::RRE, 8, 8), &PF32, &u32::MAX.to_be_bytes()),
        Err(ProtoError::LimitExceeded { .. })
    ));
}

#[test]
fn unknown_encodings_are_fatal() {
    assert_eq!(
        frame(&header(enc::TRLE, 8, 8), &PF32, &[0; 16]),
        Err(ProtoError::UnsupportedEncoding(enc::TRLE))
    );
    assert_eq!(
        frame(&header(12345, 8, 8), &PF32, &[]),
        Err(ProtoError::UnsupportedEncoding(12345))
    );
}

#[test]
fn extended_desktop_size_is_four_bytes_plus_sixteen_per_screen() {
    let h = header(pseudo::EXT_DESKTOP_SIZE, 1920, 1080);
    // No screens: the header only.
    check(&h, &PF32, &[0, 0, 0, 0]);
    // Two screens.
    let mut p = vec![2, 0, 0, 0];
    p.extend_from_slice(&[0; 32]);
    check(&h, &PF32, &p);
    // The count is read before anything else can be waited for.
    assert_eq!(
        frame(&h, &PF32, &[255]),
        Err(ProtoError::NeedMore(3 + 255 * 16))
    );
}

// --- Tight (rfbproto.rst, "Tight Encoding") ---------------------------------

fn tight_header(w: u16, h: u16) -> RectHeader {
    header(enc::TIGHT, w, h)
}

#[test]
fn tight_fill_is_one_tpixel() {
    // 32 bpp, depth 24, 8-bit channels: a TPIXEL is 3 bytes.
    check(&tight_header(100, 100), &PF32, &[0x80, 1, 2, 3]);
    // Stream reset bits do not change the length.
    check(&tight_header(100, 100), &PF32, &[0x8F, 1, 2, 3]);
    // Any other format uses its own pixel size.
    check(&tight_header(100, 100), &pf16(), &[0x80, 1, 2]);
}

#[test]
fn tight_tpixel_is_three_bytes_only_for_8_bit_channels() {
    assert_eq!(PF32.tight_pixel_len(), 3);
    assert_eq!(pf16().tight_pixel_len(), 2);
    let deep = PixelFormat {
        red_max: 1023,
        green_max: 1023,
        blue_max: 1023,
        depth: 30,
        ..PF32
    };
    assert_eq!(deep.tight_pixel_len(), 4);
}

#[test]
fn tight_basic_data_under_twelve_bytes_is_sent_as_is() {
    // 2 x 1 pixels = 6 bytes of CopyFilter data, no length, no zlib.
    check(&tight_header(2, 1), &PF32, &[0x00, 1, 2, 3, 4, 5, 6]);
}

#[test]
fn tight_basic_data_of_twelve_bytes_or_more_has_a_compact_length() {
    // 4 x 1 pixels = 12 bytes: length 5, then 5 compressed bytes.
    check(&tight_header(4, 1), &PF32, &[0x00, 5, 9, 9, 9, 9, 9]);
    // Stream 3 selected (bits 4-5), reset bit 3 set: same framing.
    check(&tight_header(4, 1), &PF32, &[0x38, 5, 9, 9, 9, 9, 9]);
}

#[test]
fn tight_palette_filter_sizes() {
    // Two colours: 1 bit per pixel, rows padded to bytes. 8 x 1 -> 1 byte,
    // raw. Layout: control, filter id, colours - 1, palette, data.
    let mut p = vec![0x40, 1, 1, 1, 2, 3, 4, 5, 6, 0xAA];
    check(&tight_header(8, 1), &PF32, &p);
    // 9 x 2 -> 2 bytes per row = 4 bytes, still raw.
    p = vec![0x40, 1, 1, 1, 2, 3, 4, 5, 6, 1, 2, 3, 4];
    check(&tight_header(9, 2), &PF32, &p);
    // Three colours: 8 bits per pixel. 4 x 3 = 12 bytes -> compact length.
    p = vec![0x40, 1, 2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 3, 0, 0, 0];
    check(&tight_header(4, 3), &PF32, &p);
}

#[test]
fn tight_without_zlib_is_always_raw() {
    let mut p = vec![0xA0];
    p.extend_from_slice(&[0; 48]); // 4 x 4 x 3
    check(&tight_header(4, 4), &PF32, &p);
    // With a filter id (0xE0), copy filter.
    let mut p = vec![0xE0, 0];
    p.extend_from_slice(&[0; 48]);
    check(&tight_header(4, 4), &PF32, &p);
    // Palette without zlib: data size known, no length.
    let p = vec![0xE0, 1, 1, 1, 2, 3, 4, 5, 6, 0xFF, 0xFF];
    check(&tight_header(8, 2), &PF32, &p);
}

#[test]
fn tight_gradient_filter_has_the_size_of_copy() {
    check(&tight_header(2, 1), &PF32, &[0x40, 2, 1, 2, 3, 4, 5, 6]);
}

#[test]
fn tight_jpeg_is_a_compact_length_and_data() {
    // The spec's own example: 10000 is 0x90 0x4E.
    let mut p = vec![0x90, 0x90, 0x4E];
    p.extend_from_slice(&[0; 10_000]);
    check(&tight_header(64, 64), &PF32, &p);
    check(&tight_header(1, 1), &PF32, &[0x90, 0]);
}

#[test]
fn tight_compact_length_uses_three_bytes_from_16384() {
    // 16384 = 1 << 14: 0x80 0x80 0x01.
    let mut p = vec![0x90, 0x80, 0x80, 0x01];
    p.extend_from_slice(&[0; 16_384]);
    check(&tight_header(64, 64), &PF32, &p);
    // 127 and 128 bracket the one/two byte boundary: 0x7F and 0x80 0x01.
    let mut p = vec![0x90, 0x7F];
    p.extend_from_slice(&[0; 127]);
    check(&tight_header(64, 64), &PF32, &p);
    let mut p = vec![0x90, 0x80, 0x01];
    p.extend_from_slice(&[0; 128]);
    check(&tight_header(64, 64), &PF32, &p);
}

#[test]
fn tight_invalid_controls_are_fatal() {
    for bad in [&[0xB0u8][..], &[0xC0], &[0xD0], &[0xF0]] {
        assert!(
            matches!(
                frame(&tight_header(4, 4), &PF32, bad),
                Err(ProtoError::Malformed { .. })
            ),
            "{bad:x?}"
        );
    }
    // Unknown filter id, and a palette of one colour.
    assert!(matches!(
        frame(&tight_header(4, 4), &PF32, &[0x40, 3]),
        Err(ProtoError::Malformed { .. })
    ));
    assert!(matches!(
        frame(&tight_header(4, 4), &PF32, &[0x40, 1, 0, 1, 2, 3]),
        Err(ProtoError::Malformed { .. })
    ));
}

#[test]
fn tight_sizes_are_bounded() {
    // A raw 16384 x 16384 rectangle is far beyond the payload limit.
    let mut h = tight_header(u16::MAX, u16::MAX);
    h.rect.width = u16::MAX;
    assert!(matches!(
        frame(&h, &PF32, &[0xA0]),
        Err(ProtoError::LimitExceeded { .. })
    ));
}
