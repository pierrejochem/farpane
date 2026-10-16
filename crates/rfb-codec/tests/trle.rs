//! TRLE decoding of byte strings written by hand from RFC 6143 section 7.7.5.

#![allow(clippy::many_single_char_names)]

use rfb_codec::{DecodeError, Decoder, Framebuffer, enc};
use rfb_proto::{PixelFormat, framing::frame, msg::server::RectHeader, types::Rect};

const PF: PixelFormat = PixelFormat::RGB888;

fn rect(w: u16, h: u16) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    }
}

fn header(w: u16, h: u16) -> RectHeader {
    RectHeader {
        rect: rect(w, h),
        encoding: enc::TRLE,
    }
}

/// CPIXELs of RGB888 are three bytes, B G R (the low three bytes, little endian).
const RED: [u8; 3] = [0, 0, 255];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [255, 0, 0];

fn decode(w: u16, h: u16, payload: &[u8]) -> Result<Framebuffer, DecodeError> {
    let mut fb = Framebuffer::new(w, h).unwrap();
    // The framer and the decoder must agree on where the payload ends.
    if let Ok(n) = frame(&header(w, h), &PF, payload) {
        assert!(n <= payload.len(), "the framer found more than there is");
    }
    Decoder::new(&PF)
        .unwrap()
        .decode(&mut fb, &header(w, h), payload)?;
    Ok(fb)
}

fn at(fb: &Framebuffer, x: u16, y: u16) -> [u8; 3] {
    let [b, g, r, _] = fb.pixel(x, y).unwrap();
    [b, g, r]
}

#[test]
fn raw_solid_and_packed_palette_tiles() {
    // Raw 2 x 1.
    let fb = decode(2, 1, &[&[0][..], &RED, &GREEN].concat()).unwrap();
    assert_eq!((at(&fb, 0, 0), at(&fb, 1, 0)), (RED, GREEN));
    // Solid.
    let fb = decode(3, 3, &[&[1][..], &BLUE].concat()).unwrap();
    assert!((0..3).all(|y| (0..3).all(|x| at(&fb, x, y) == BLUE)));
    // Packed palette of two, 3 x 2: rows 101 and 010, left pixel in the top bit.
    let fb = decode(
        3,
        2,
        &[&[2][..], &RED, &GREEN, &[0b1010_0000, 0b0100_0000]].concat(),
    )
    .unwrap();
    assert_eq!(
        [at(&fb, 0, 0), at(&fb, 1, 0), at(&fb, 2, 0)],
        [GREEN, RED, GREEN]
    );
    assert_eq!(
        [at(&fb, 0, 1), at(&fb, 1, 1), at(&fb, 2, 1)],
        [RED, GREEN, RED]
    );
}

#[test]
fn runs_continue_across_rows_and_use_the_255_rule() {
    // 3 x 2: a run of 4 red then 2 green: [pixel, 3], [pixel, 1].
    let fb = decode(3, 2, &[&[128][..], &RED, &[3], &GREEN, &[1]].concat()).unwrap();
    assert_eq!(at(&fb, 0, 0), RED);
    assert_eq!(at(&fb, 2, 1), GREEN);
    assert_eq!(at(&fb, 0, 1), RED, "the run wrapped into the second row");
    assert_eq!(at(&fb, 1, 1), GREEN, "two green pixels follow the four red");
    // 16 x 16: one run of 256 is [255, 0].
    let fb = decode(16, 16, &[&[128][..], &BLUE, &[255, 0]].concat()).unwrap();
    assert_eq!(at(&fb, 15, 15), BLUE);
    // Palette RLE: palette of 2, then (0, run of 3), (1), (1).
    let fb = decode(
        3,
        2,
        &[&[130][..], &RED, &GREEN, &[0x80, 2, 1, 1, 0]].concat(),
    )
    .unwrap();
    assert_eq!(
        [at(&fb, 0, 0), at(&fb, 2, 0), at(&fb, 0, 1)],
        [RED, RED, GREEN]
    );
}

#[test]
fn the_previous_palette_is_reused_by_127_and_129() {
    // Two 16 x 1 tiles in a 32 x 1 rectangle. The first has a packed
    // palette (red, green) with the pattern 1010...; the second reuses it
    // with the opposite pattern.
    let first = [&[2][..], &RED, &GREEN, &[0b1010_1010, 0b1010_1010]].concat();
    let second = [&[127][..], &[0b0101_0101, 0b0101_0101]].concat();
    let fb = decode(32, 1, &[first, second].concat()).unwrap();
    assert_eq!((at(&fb, 0, 0), at(&fb, 1, 0)), (GREEN, RED));
    assert_eq!((at(&fb, 16, 0), at(&fb, 17, 0)), (RED, GREEN));
    // Palette RLE, then 129: 16 green (index 1), run length 16 is [15].
    let first = [&[130][..], &RED, &GREEN, &[0x80, 15]].concat();
    let second = [&[129][..], &[0x81, 15]].concat();
    let fb = decode(32, 1, &[first, second].concat()).unwrap();
    assert_eq!(
        (at(&fb, 0, 0), at(&fb, 16, 0), at(&fb, 31, 0)),
        (RED, GREEN, GREEN)
    );
    // Solid and raw tiles in between do not replace the palette.
    let tiles = [
        &[130][..],
        &RED,
        &GREEN,
        &[0x80, 15],
        &[1],
        &BLUE,
        &[0],
        &[0; 48],
        &[129],
        &[0x81, 15],
    ]
    .concat();
    let fb = decode(64, 1, &tiles).unwrap();
    assert_eq!(
        (at(&fb, 16, 0), at(&fb, 32, 0), at(&fb, 48, 0)),
        (BLUE, [0; 3], GREEN)
    );
    // A packed tile can reuse the palette of an earlier palette RLE tile of 2.
    let tiles = [&[130][..], &RED, &GREEN, &[0x80, 15], &[127], &[0xFF, 0x00]].concat();
    let fb = decode(32, 1, &tiles).unwrap();
    assert_eq!((at(&fb, 16, 0), at(&fb, 24, 0)), (GREEN, RED));
}

#[test]
fn invalid_tiles_are_errors() {
    let bad =
        |w: u16, payload: &[u8]| matches!(decode(w, 1, payload), Err(DecodeError::Malformed(_)));
    assert!(bad(16, &[127, 0, 0]), "reuse before any palette");
    assert!(bad(16, &[129, 0]), "reuse before any palette");
    assert!(bad(16, &[17]), "unused subencoding");
    assert!(
        bad(16, &[&[128][..], &RED, &[16]].concat()),
        "run past the tile"
    );
    assert!(
        bad(16, &[&[130][..], &RED, &GREEN, &[2]].concat()),
        "index outside the palette"
    );
    // A packed tile cannot reuse a palette of 20.
    let big = [&[128 + 20][..], &[0; 60], &[0x80, 15], &[127], &[0; 2]].concat();
    assert!(bad(32, &big));
    // Extra bytes after the tiles.
    assert!(bad(2, &[&[1][..], &RED, &[0]].concat()));
    // Truncated.
    assert!(bad(2, &[&[0][..], &RED].concat()));
}

#[test]
fn trle_works_inside_a_rectangle_that_is_not_at_the_origin() {
    let mut fb = Framebuffer::new(40, 40).unwrap();
    let h = RectHeader {
        rect: Rect {
            x: 10,
            y: 20,
            width: 3,
            height: 2,
        },
        encoding: enc::TRLE,
    };
    Decoder::new(&PF)
        .unwrap()
        .decode(&mut fb, &h, &[&[1][..], &GREEN].concat())
        .unwrap();
    assert_eq!(at(&fb, 10, 20), GREEN);
    assert_eq!(at(&fb, 12, 21), GREEN);
    assert_eq!(at(&fb, 13, 21), [0, 0, 0]);
    assert_eq!(at(&fb, 9, 20), [0, 0, 0]);
    // And one that does not fit is refused.
    let out = RectHeader {
        rect: Rect {
            x: 39,
            y: 39,
            width: 3,
            height: 3,
        },
        encoding: enc::TRLE,
    };
    assert_eq!(
        Decoder::new(&PF)
            .unwrap()
            .decode(&mut fb, &out, &[1, 0, 0, 0]),
        Err(DecodeError::OutOfBounds)
    );
}
