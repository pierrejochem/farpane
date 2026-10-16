//! RRE, `CoRRE` and Hextile.

// Test geometry reads best with short names; generated coordinates are < 16.
#![allow(
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    clippy::similar_names
)]

use proptest::prelude::*;
use rfb_codec::{Damage, DecodeError, Decoder, Framebuffer, enc};
use rfb_proto::{PixelFormat, framing::frame, msg::server::RectHeader, types::Rect};

const RED: [u8; 4] = [0, 0, 255, 0xFF];
const GREEN: [u8; 4] = [0, 255, 0, 0xFF];
const BLUE: [u8; 4] = [255, 0, 0, 0xFF];
const BLACK: [u8; 4] = [0, 0, 0, 0xFF];

/// A wire pixel in the default 32 bpp format: B, G, R, X.
fn wire(c: [u8; 4]) -> [u8; 4] {
    [c[0], c[1], c[2], 0]
}

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

fn run(fb: &mut Framebuffer, h: &RectHeader, payload: &[u8]) -> Result<Damage, DecodeError> {
    Decoder::new(&PixelFormat::RGB888)
        .unwrap()
        .decode(fb, h, payload)
}

#[test]
fn rre_draws_background_then_subrects() {
    let mut fb = Framebuffer::new(10, 10).unwrap();
    let mut p = 2u32.to_be_bytes().to_vec();
    p.extend(wire(BLUE));
    // Subrect 1: red at (1,1) 2x1. Subrect 2: green at (3,2) 1x2.
    p.extend(wire(RED));
    for v in [1u16, 1, 2, 1] {
        p.extend(v.to_be_bytes());
    }
    p.extend(wire(GREEN));
    for v in [3u16, 2, 1, 2] {
        p.extend(v.to_be_bytes());
    }
    let h = header(enc::RRE, 2, 3, 5, 4);
    assert_eq!(frame(&h, &PixelFormat::RGB888, &p), Ok(p.len()));
    assert_eq!(run(&mut fb, &h, &p), Ok(Damage::Rect(h.rect)));
    // Rect origin is (2, 3); subrect coordinates are relative to it.
    assert_eq!(fb.pixel(2, 3), Some(BLUE));
    assert_eq!(fb.pixel(3, 4), Some(RED));
    assert_eq!(fb.pixel(4, 4), Some(RED));
    assert_eq!(fb.pixel(5, 4), Some(BLUE));
    assert_eq!(fb.pixel(5, 5), Some(GREEN));
    assert_eq!(fb.pixel(5, 6), Some(GREEN));
    assert_eq!(fb.pixel(5, 7), Some(BLACK), "row 7 is below the rectangle");
    assert_eq!(fb.pixel(1, 3), Some(BLACK), "outside the rectangle");
    assert_eq!(fb.pixel(7, 3), Some(BLACK));
}

#[test]
fn co_rre_uses_byte_coordinates() {
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let mut p = 1u32.to_be_bytes().to_vec();
    p.extend(wire(BLUE));
    p.extend(wire(RED));
    p.extend([1, 2, 2, 1]);
    let h = header(enc::CO_RRE, 0, 0, 4, 4);
    assert_eq!(frame(&h, &PixelFormat::RGB888, &p), Ok(p.len()));
    run(&mut fb, &h, &p).unwrap();
    assert_eq!(fb.pixel(1, 2), Some(RED));
    assert_eq!(fb.pixel(2, 2), Some(RED));
    assert_eq!(fb.pixel(3, 2), Some(BLUE));
}

#[test]
fn rre_rejects_bad_payloads() {
    let mut fb = Framebuffer::new(8, 8).unwrap();
    let h = header(enc::RRE, 0, 0, 4, 4);
    let mut p = 1u32.to_be_bytes().to_vec();
    p.extend(wire(BLUE));
    p.extend(wire(RED));
    // Subrect 3,3 2x2 leaves the 4x4 rectangle.
    for v in [3u16, 3, 2, 2] {
        p.extend(v.to_be_bytes());
    }
    assert!(matches!(
        run(&mut fb, &h, &p),
        Err(DecodeError::Malformed(_))
    ));
    // Truncated, and trailing bytes.
    assert!(matches!(
        run(&mut fb, &h, &p[..p.len() - 1]),
        Err(DecodeError::Malformed(_))
    ));
    let mut ok = 0u32.to_be_bytes().to_vec();
    ok.extend(wire(BLUE));
    assert!(run(&mut fb, &h, &ok).is_ok());
    ok.push(0);
    assert!(matches!(
        run(&mut fb, &h, &ok),
        Err(DecodeError::Malformed(_))
    ));
    // Outside the framebuffer.
    assert_eq!(
        run(&mut fb, &header(enc::RRE, 6, 6, 4, 4), &ok[..8]),
        Err(DecodeError::OutOfBounds)
    );
}

/// Build a Hextile payload for a 20 x 17 rectangle (2 x 2 tiles of 16x16,
/// 4x16, 16x1 and 4x1) that exercises every flag.
fn hextile_20x17() -> Vec<u8> {
    let mut p = Vec::new();
    // Tile 0 (16x16): background + foreground + two plain subrects.
    p.push(2 | 4 | 8);
    p.extend(wire(BLUE));
    p.extend(wire(RED));
    p.push(2);
    p.extend([0x00, 0x11]); // x0 y0, 2x2
    p.extend([0xFE, 0x00]); // x15 y14, 1x1
    // Tile 1 (4x16): nothing specified, no subrects: reuse the blue background.
    p.push(0);
    // Tile 2 (16x1): raw.
    p.push(1);
    for i in 0..16u8 {
        p.extend([i, 0, 0, 0]);
    }
    // Tile 3 (4x1): new background and coloured subrects.
    p.push(2 | 8 | 16);
    p.extend(wire(GREEN));
    p.push(1);
    p.extend(wire(RED));
    p.extend([0x20, 0x10]); // x2 y0, 2x1
    p
}

#[test]
fn hextile_decodes_every_flag_and_edge_tile() {
    let mut fb = Framebuffer::new(30, 20).unwrap();
    let p = hextile_20x17();
    let h = header(enc::HEXTILE, 5, 2, 20, 17);
    assert_eq!(frame(&h, &PixelFormat::RGB888, &p), Ok(p.len()));
    assert_eq!(run(&mut fb, &h, &p), Ok(Damage::Rect(h.rect)));
    let at = |x: u16, y: u16| fb.pixel(5 + x, 2 + y).unwrap();
    // Tile 0.
    assert_eq!(at(0, 0), RED);
    assert_eq!(at(1, 1), RED);
    assert_eq!(at(2, 2), BLUE);
    assert_eq!(at(15, 14), RED);
    assert_eq!(at(15, 15), BLUE);
    // Tile 1 carried the background over.
    assert_eq!(at(16, 0), BLUE);
    assert_eq!(at(19, 15), BLUE);
    // Tile 2 is raw: blue channel counts 0..15.
    for i in 0..16u8 {
        assert_eq!(at(u16::from(i), 16)[0], i);
    }
    // Tile 3: green background with a red 2x1 subrect at x2.
    assert_eq!(at(16, 16), GREEN);
    assert_eq!(at(18, 16), RED);
    assert_eq!(at(19, 16), RED);
}

#[test]
fn hextile_rejects_invalid_payloads() {
    let mut fb = Framebuffer::new(16, 16).unwrap();
    let h = header(enc::HEXTILE, 0, 0, 16, 16);
    let bad = |p: &[u8]| {
        matches!(
            run(&mut Framebuffer::new(16, 16).unwrap(), &h, p),
            Err(DecodeError::Malformed(_))
        )
    };
    // Background never specified.
    assert!(bad(&[0]));
    // Foreground never specified but subrects use it.
    let mut p = vec![2 | 8];
    p.extend(wire(BLUE));
    p.extend([1, 0x00, 0x00]);
    assert!(bad(&p));
    // Foreground together with coloured subrects.
    let mut p = vec![2 | 4 | 8 | 16];
    p.extend(wire(BLUE));
    p.extend(wire(RED));
    assert!(bad(&p));
    // Subrect sticks out of an 4x4 edge tile.
    let h4 = header(enc::HEXTILE, 0, 0, 4, 4);
    let mut p = vec![2 | 4 | 8];
    p.extend(wire(BLUE));
    p.extend(wire(RED));
    p.extend([1, 0x30, 0x10]); // x3, w2 -> 5 > 4
    assert!(matches!(
        run(&mut fb, &h4, &p),
        Err(DecodeError::Malformed(_))
    ));
    // Truncated and trailing bytes.
    let good = {
        let mut p = vec![2];
        p.extend(wire(BLUE));
        p
    };
    assert!(run(&mut fb, &h, &good).is_ok());
    assert!(bad(&good[..good.len() - 1]));
    let mut long = good.clone();
    long.push(0);
    assert!(bad(&long));
}

#[test]
fn hextile_in_16_bpp_uses_two_byte_pixels() {
    let pf = PixelFormat {
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
    let mut fb = Framebuffer::new(2, 2).unwrap();
    let p = [2, 0x00, 0xF8]; // background = pure red
    let h = header(enc::HEXTILE, 0, 0, 2, 2);
    assert_eq!(frame(&h, &pf, &p), Ok(3));
    Decoder::new(&pf).unwrap().decode(&mut fb, &h, &p).unwrap();
    assert_eq!(fb.pixel(1, 1), Some(RED));
}

/// A tiny deterministic byte source for building random valid payloads.
struct Entropy<'a>(&'a [u8], usize);

impl Entropy<'_> {
    fn next(&mut self) -> u8 {
        let b = self.0[self.1 % self.0.len()];
        self.1 += 1;
        b.wrapping_add((self.1 / self.0.len()) as u8)
    }
}

/// A random but valid Hextile payload for a `w` x `h` rectangle.
fn random_hextile(w: u16, h: u16, seed: &[u8]) -> Vec<u8> {
    let mut e = Entropy(seed, 0);
    let mut p = Vec::new();
    let (mut have_bg, mut have_fg) = (false, false);
    for ty in (0..h).step_by(16) {
        let th = (h - ty).min(16);
        for tx in (0..w).step_by(16) {
            let tw = (w - tx).min(16);
            if e.next() % 4 == 0 {
                p.push(1);
                for _ in 0..usize::from(tw) * usize::from(th) {
                    p.extend([e.next(), e.next(), e.next(), 0]);
                }
                continue;
            }
            let mut mask = 0u8;
            if !have_bg || e.next() % 2 == 0 {
                mask |= 2;
                have_bg = true;
            }
            let coloured = e.next() % 3 == 0;
            let subrects = e.next() % 5 != 0;
            if subrects {
                mask |= 8;
                if coloured {
                    mask |= 16;
                } else if !have_fg || e.next() % 2 == 0 {
                    mask |= 4;
                    have_fg = true;
                }
            }
            p.push(mask);
            if mask & 2 != 0 {
                p.extend([e.next(), e.next(), e.next(), 0]);
            }
            if mask & 4 != 0 {
                p.extend([e.next(), e.next(), e.next(), 0]);
            }
            if mask & 8 != 0 {
                let n = e.next() % 6;
                p.push(n);
                for _ in 0..n {
                    if mask & 16 != 0 {
                        p.extend([e.next(), e.next(), e.next(), 0]);
                    }
                    let x = u16::from(e.next()) % tw;
                    let y = u16::from(e.next()) % th;
                    let sw = u16::from(e.next()) % (tw - x) + 1;
                    let sh = u16::from(e.next()) % (th - y) + 1;
                    p.push((x as u8) << 4 | y as u8);
                    p.push(((sw - 1) as u8) << 4 | (sh - 1) as u8);
                }
            }
            if mask & 16 != 0 {
                have_fg = false; // coloured tiles leave no usable foreground
            }
        }
    }
    p
}

proptest! {
    /// Random valid payloads: the framer and the decoder agree on the length,
    /// and decoding succeeds.
    #[test]
    fn hextile_framer_and_decoder_agree(
        w in 1u16..70,
        h in 1u16..50,
        seed in prop::collection::vec(any::<u8>(), 1..64),
    ) {
        let p = random_hextile(w, h, &seed);
        let hd = header(enc::HEXTILE, 0, 0, w, h);
        prop_assert_eq!(frame(&hd, &PixelFormat::RGB888, &p), Ok(p.len()));
        let mut fb = Framebuffer::new(w, h).unwrap();
        prop_assert_eq!(run(&mut fb, &hd, &p), Ok(Damage::Rect(hd.rect)));
    }

    /// An all-raw Hextile rectangle equals the same pixels sent as Raw.
    #[test]
    fn raw_only_hextile_equals_raw(
        w in 1u16..40,
        h in 1u16..40,
        seed in prop::collection::vec(any::<u8>(), 1..32),
    ) {
        let mut e = Entropy(&seed, 0);
        let pixels: Vec<u8> = (0..usize::from(w) * usize::from(h))
            .flat_map(|_| [e.next(), e.next(), e.next(), 0])
            .collect();
        let at = |x: u16, y: u16| {
            let i = (usize::from(y) * usize::from(w) + usize::from(x)) * 4;
            &pixels[i..i + 4]
        };
        let mut hextile = Vec::new();
        for ty in (0..h).step_by(16) {
            for tx in (0..w).step_by(16) {
                hextile.push(1);
                for y in ty..(ty + 16).min(h) {
                    for x in tx..(tx + 16).min(w) {
                        hextile.extend_from_slice(at(x, y));
                    }
                }
            }
        }
        let (mut a, mut b) = (Framebuffer::new(w, h).unwrap(), Framebuffer::new(w, h).unwrap());
        run(&mut a, &header(enc::HEXTILE, 0, 0, w, h), &hextile).unwrap();
        run(&mut b, &header(enc::RAW, 0, 0, w, h), &pixels).unwrap();
        prop_assert_eq!(a, b);
    }
}
