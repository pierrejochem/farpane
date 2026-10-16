//! zlib and ZRLE.

#![allow(
    clippy::cast_possible_truncation,
    clippy::needless_for_each,
    clippy::many_single_char_names,
    clippy::similar_names
)]

use flate2::{Compress, Compression, FlushCompress};
use proptest::prelude::*;
use rfb_codec::{Damage, DecodeError, Decoder, Framebuffer, enc};
use rfb_proto::{PixelFormat, framing::frame, msg::server::RectHeader, types::Rect};

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

/// A persistent zlib compressor that sync-flushes after each chunk, like an
/// RFB server.
struct Server(Compress);

impl Server {
    fn new() -> Self {
        Self(Compress::new(Compression::default(), true))
    }

    /// Compress `data` as the next chunk and prefix the `U32` length.
    fn chunk(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 64);
        let start = self.0.total_in();
        loop {
            let done = (self.0.total_in() - start) as usize;
            out.reserve(1024);
            self.0
                .compress_vec(&data[done..], &mut out, FlushCompress::Sync)
                .unwrap();
            if (self.0.total_in() - start) as usize == data.len() && out.len() < out.capacity() {
                break;
            }
        }
        let mut payload = (out.len() as u32).to_be_bytes().to_vec();
        payload.extend(out);
        payload
    }
}

/// An RGB888 pixel as the wire carries it: B, G, R, X.
fn wire(i: u32) -> [u8; 4] {
    [(i * 7) as u8, (i * 13) as u8, (i * 29) as u8, 0]
}

#[test]
fn zlib_decodes_consecutive_rects_on_one_stream() {
    let mut server = Server::new();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    let mut fb = Framebuffer::new(8, 8).unwrap();
    for round in 0..3u32 {
        let pixels: Vec<u8> = (0..16).flat_map(|i| wire(i + round)).collect();
        let payload = server.chunk(&pixels);
        let h = header(enc::ZLIB, 2, 2, 4, 4);
        assert_eq!(frame(&h, &PixelFormat::RGB888, &payload), Ok(payload.len()));
        assert_eq!(dec.decode(&mut fb, &h, &payload), Ok(Damage::Rect(h.rect)));
        let w = wire(round);
        assert_eq!(
            fb.pixel(2, 2),
            Some([w[0], w[1], w[2], 255]),
            "round {round}"
        );
    }
}

#[test]
fn zlib_needs_the_same_stream_for_every_rect() {
    let mut server = Server::new();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let pixels: Vec<u8> = (0..16).flat_map(wire).collect();
    let first = server.chunk(&pixels);
    let second = server.chunk(&pixels);
    // A decoder that never saw the first chunk cannot read the second: it is
    // a continuation, with no zlib header.
    let mut fresh = Decoder::new(&PixelFormat::RGB888).unwrap();
    let h = header(enc::ZLIB, 0, 0, 4, 4);
    assert!(fresh.decode(&mut fb, &h, &second).is_err());
    dec.decode(&mut fb, &h, &first).unwrap();
    dec.decode(&mut fb, &h, &second).unwrap();
}

#[test]
fn zlib_rejects_wrong_sizes_and_prefixes() {
    let mut server = Server::new();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    let mut fb = Framebuffer::new(4, 4).unwrap();
    let h = header(enc::ZLIB, 0, 0, 4, 4);
    // Too few pixels for the rectangle.
    let short = server.chunk(&[0; 12]);
    assert!(matches!(
        dec.decode(&mut fb, &h, &short),
        Err(DecodeError::Malformed(_))
    ));
    // Length prefix disagrees with the data.
    let mut bad = Server::new().chunk(&[0; 64]);
    bad[3] ^= 1;
    assert!(matches!(
        Decoder::new(&PixelFormat::RGB888)
            .unwrap()
            .decode(&mut fb, &h, &bad),
        Err(DecodeError::Malformed(_))
    ));
    // Too many pixels: refused at the limit, not after inflating everything.
    let long = Server::new().chunk(&vec![0; 1_000_000]);
    assert!(matches!(
        Decoder::new(&PixelFormat::RGB888)
            .unwrap()
            .decode(&mut fb, &h, &long),
        Err(DecodeError::Malformed(_))
    ));
}

// --- ZRLE test encoder -----------------------------------------------------

/// How to encode a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Mode {
    Raw,
    Solid,
    Packed,
    PlainRle,
    PaletteRle,
}

/// Encode one tile of wire pixels (`cpix` bytes each), `tw` wide.
fn encode_tile(px: &[Vec<u8>], tw: usize, mode: Mode, out: &mut Vec<u8>) {
    let mut palette: Vec<&Vec<u8>> = Vec::new();
    for p in px {
        if !palette.contains(&p) {
            palette.push(p);
        }
    }
    let index = |p: &Vec<u8>| palette.iter().position(|q| *q == p).unwrap();
    let runs = || {
        let mut runs: Vec<(&Vec<u8>, usize)> = Vec::new();
        for p in px {
            match runs.last_mut() {
                Some((q, n)) if *q == p => *n += 1,
                _ => runs.push((p, 1)),
            }
        }
        runs
    };
    let put_len = |out: &mut Vec<u8>, n: usize| {
        let mut rest = n - 1;
        while rest >= 255 {
            out.push(255);
            rest -= 255;
        }
        out.push(rest as u8);
    };
    match mode {
        Mode::Raw => {
            out.push(0);
            px.iter().for_each(|p| out.extend(p));
        }
        Mode::Solid => {
            assert_eq!(palette.len(), 1);
            out.push(1);
            out.extend(palette[0]);
        }
        Mode::Packed => {
            assert!((2..=16).contains(&palette.len()));
            out.push(palette.len() as u8);
            palette.iter().for_each(|p| out.extend(*p));
            let bits = match palette.len() {
                2 => 1,
                3..=4 => 2,
                _ => 4,
            };
            for row in px.chunks(tw) {
                let mut bytes = vec![0u8; (row.len() * bits).div_ceil(8)];
                for (x, p) in row.iter().enumerate() {
                    bytes[x * bits / 8] |= (index(p) as u8) << (8 - bits - (x * bits) % 8);
                }
                out.extend(bytes);
            }
        }
        Mode::PlainRle => {
            out.push(128);
            for (p, n) in runs() {
                out.extend(p);
                put_len(out, n);
            }
        }
        Mode::PaletteRle => {
            assert!((2..=127).contains(&palette.len()));
            out.push(128 + palette.len() as u8);
            palette.iter().for_each(|p| out.extend(*p));
            for (p, n) in runs() {
                if n == 1 {
                    out.push(index(p) as u8);
                } else {
                    out.push(index(p) as u8 | 0x80);
                    put_len(out, n);
                }
            }
        }
    }
}

/// Convert a CPIXEL-sized wire pixel to the framebuffer's `B, G, R, 0xFF`
/// for the default 32 bpp RGB888 format (CPIXEL = B, G, R).
fn expected(p: &[u8]) -> [u8; 4] {
    [p[0], p[1], p[2], 255]
}

/// A `w` x `h` image whose tiles are cut from `colours` with some structure,
/// each tile encoded with a mode chosen from `modes` where applicable.
fn zrle_case(w: u16, h: u16, seed: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>, Vec<Mode>) {
    let at = |i: usize| seed[i % seed.len()];
    let (mut data, mut image) = (
        Vec::new(),
        vec![vec![0u8; 3]; usize::from(w) * usize::from(h)],
    );
    let mut n = 0usize;
    let mut used = Vec::new();
    for ty in (0..h).step_by(64) {
        for tx in (0..w).step_by(64) {
            let (tw, th) = ((w - tx).min(64) as usize, (h - ty).min(64) as usize);
            n += 1;
            let colours = 1 + usize::from(at(n)) % 20; // 1..=20 distinct colours
            let long_runs = at(n + 1) % 2 == 0;
            let tile: Vec<Vec<u8>> = (0..tw * th)
                .map(|i| {
                    let k = if long_runs { i / 7 } else { i * 31 / 5 };
                    let c = (k + usize::from(at(n + 2))) % colours;
                    vec![(c * 11) as u8, (c * 23) as u8, (c * 37) as u8]
                })
                .collect();
            let distinct = {
                let mut d: Vec<&Vec<u8>> = Vec::new();
                tile.iter().for_each(|p| {
                    if !d.contains(&p) {
                        d.push(p);
                    }
                });
                d.len()
            };
            let mut options = vec![Mode::Raw, Mode::PlainRle];
            if distinct == 1 {
                options.push(Mode::Solid);
            }
            if (2..=16).contains(&distinct) {
                options.push(Mode::Packed);
            }
            if (2..=127).contains(&distinct) {
                options.push(Mode::PaletteRle);
            }
            let mode = options[usize::from(at(n + 3)) % options.len()];
            used.push(mode);
            encode_tile(&tile, tw, mode, &mut data);
            for (i, p) in tile.into_iter().enumerate() {
                let (x, y) = (usize::from(tx) + i % tw, usize::from(ty) + i / tw);
                image[y * usize::from(w) + x] = p;
            }
        }
    }
    (data, image, used)
}

#[test]
fn zrle_decodes_every_subencoding_and_edge_tile() {
    // 130 x 70: tiles of 64x64, 64x64, 2x64, 64x6, 64x6, 2x6.
    let mut modes_used = std::collections::HashSet::new();
    for seed in 0..40u8 {
        let seed_bytes = [seed, seed.wrapping_mul(7), seed.wrapping_add(90), 3, 17];
        let (data, image, used) = zrle_case(130, 70, &seed_bytes);
        modes_used.extend(used);
        let mut server = Server::new();
        let payload = server.chunk(&data);
        let h = header(enc::ZRLE, 1, 2, 130, 70);
        assert_eq!(frame(&h, &PixelFormat::RGB888, &payload), Ok(payload.len()));
        let mut fb = Framebuffer::new(140, 80).unwrap();
        let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
        assert_eq!(dec.decode(&mut fb, &h, &payload), Ok(Damage::Rect(h.rect)));
        for y in 0..70u16 {
            for x in 0..130u16 {
                assert_eq!(
                    fb.pixel(1 + x, 2 + y),
                    Some(expected(&image[usize::from(y) * 130 + usize::from(x)])),
                    "seed {seed} at ({x}, {y})"
                );
            }
        }
    }
    assert_eq!(
        modes_used.len(),
        5,
        "every subencoding was exercised: {modes_used:?}"
    );
}

#[test]
fn zrle_keeps_one_stream_across_rects() {
    let mut server = Server::new();
    let mut dec = Decoder::new(&PixelFormat::RGB888).unwrap();
    let mut fb = Framebuffer::new(16, 16).unwrap();
    for round in 0..3u8 {
        let (data, image, _) = zrle_case(16, 16, &[round, 5, 9]);
        let payload = server.chunk(&data);
        dec.decode(&mut fb, &header(enc::ZRLE, 0, 0, 16, 16), &payload)
            .unwrap();
        assert_eq!(fb.pixel(0, 0), Some(expected(&image[0])), "round {round}");
    }
}

#[test]
fn zrle_cpixel_sizes_follow_the_pixel_format() {
    let base = PixelFormat::RGB888;
    let cases = [
        // (format, bytes per CPIXEL)
        (base, 3),                              // depth 24, low 3 bytes
        (PixelFormat { depth: 32, ..base }, 4), // depth 32: full pixel
        (
            PixelFormat {
                red_shift: 24,
                green_shift: 16,
                blue_shift: 8,
                ..base
            },
            3,
        ), // high 3 bytes
        (
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
            2,
        ),
        (
            PixelFormat {
                big_endian: true,
                ..base
            },
            3,
        ), // big-endian low 3 bytes
    ];
    for (pf, cpix) in cases {
        let conv = rfb_codec::PixelConverter::new(&pf).unwrap();
        assert_eq!(conv.cpixel_len(), cpix, "{pf:?}");
        // One solid 2 x 2 tile in that format.
        let mut data = vec![1u8];
        let colour: Vec<u8> = (0..cpix as u8).map(|i| 0x40 + i).collect();
        data.extend(&colour);
        let payload = Server::new().chunk(&data);
        let mut fb = Framebuffer::new(2, 2).unwrap();
        let h = header(enc::ZRLE, 0, 0, 2, 2);
        assert_eq!(frame(&h, &pf, &payload), Ok(payload.len()));
        Decoder::new(&pf)
            .unwrap()
            .decode(&mut fb, &h, &payload)
            .unwrap();
        assert_eq!(fb.pixel(1, 1), Some(conv.cpixel(&colour)), "{pf:?}");
        assert_eq!(fb.pixel(0, 0), fb.pixel(1, 1));
    }
}

#[test]
fn zrle_cpixel_byte_positions_are_right() {
    // Little-endian low-3: CPIXEL bytes B, G, R.
    let conv = rfb_codec::PixelConverter::new(&PixelFormat::RGB888).unwrap();
    assert_eq!(conv.cpixel(&[1, 2, 3]), [1, 2, 3, 255]);
    // Big-endian low-3: the wire pixel is X, R, G, B and CPIXEL drops X.
    let be = PixelFormat {
        big_endian: true,
        ..PixelFormat::RGB888
    };
    let conv = rfb_codec::PixelConverter::new(&be).unwrap();
    assert_eq!(conv.cpixel(&[3, 2, 1]), [1, 2, 3, 255]);
    // Little-endian high-3 (shifts 24/16/8): the wire pixel is X, B, G, R in
    // memory order [b0..b3] = [0, B, G, R]; CPIXEL carries [B, G, R].
    let high = PixelFormat {
        red_shift: 24,
        green_shift: 16,
        blue_shift: 8,
        ..PixelFormat::RGB888
    };
    let conv = rfb_codec::PixelConverter::new(&high).unwrap();
    assert_eq!(conv.cpixel(&[1, 2, 3]), [1, 2, 3, 255]);
}

#[test]
fn zrle_rejects_invalid_tile_data() {
    let rect = header(enc::ZRLE, 0, 0, 4, 4);
    let try_decode = |tiles: &[u8]| {
        let payload = Server::new().chunk(tiles);
        Decoder::new(&PixelFormat::RGB888).unwrap().decode(
            &mut Framebuffer::new(4, 4).unwrap(),
            &rect,
            &payload,
        )
    };
    let malformed = |tiles: &[u8]| matches!(try_decode(tiles), Err(DecodeError::Malformed(_)));
    assert!(malformed(&[17])); // unused subencoding
    assert!(malformed(&[129, 1, 2, 3])); // unused: RLE with palette size 1
    assert!(malformed(&[1, 1, 2])); // truncated solid colour
    assert!(malformed(&[])); // no tile at all
    // Palette index outside the palette (palette of 2, index 1 -> fine, 3 -> bad).
    let mut p = vec![2, 0, 0, 0, 9, 9, 9];
    p.extend([0b1100_0000; 4]); // indices 1,1,... first row has 11 -> index 1 then 1; use 3 below
    assert!(try_decode(&p).is_ok());
    let mut p = vec![3, 0, 0, 0, 1, 1, 1, 2, 2, 2];
    p.extend([0b1100_0000; 4]); // 2-bit index 3 with a palette of 3
    assert!(malformed(&p));
    // Plain RLE: a run longer than the tile.
    let mut p = vec![128, 5, 5, 5, 16]; // 17 pixels in a 16-pixel tile
    assert!(malformed(&p));
    p[4] = 15; // exactly 16 is fine
    assert!(try_decode(&p).is_ok());
    // Trailing bytes after the last tile.
    let mut p = vec![1, 1, 2, 3];
    assert!(try_decode(&p).is_ok());
    p.push(0);
    assert!(malformed(&p));
    // Length prefix that disagrees.
    let mut payload = Server::new().chunk(&[1, 1, 2, 3]);
    payload[3] = payload[3].wrapping_add(1);
    assert!(matches!(
        Decoder::new(&PixelFormat::RGB888).unwrap().decode(
            &mut Framebuffer::new(4, 4).unwrap(),
            &rect,
            &payload
        ),
        Err(DecodeError::Malformed(_))
    ));
}

#[test]
fn zrle_refuses_a_zip_bomb() {
    // 20 MB of zeros compress to a few KB; a 4x4 rectangle may inflate to
    // well under 1 KB.
    let bomb = Server::new().chunk(&vec![0u8; 20_000_000]);
    assert!(
        bomb.len() < 100_000,
        "test data should be small: {}",
        bomb.len()
    );
    let r = Decoder::new(&PixelFormat::RGB888).unwrap().decode(
        &mut Framebuffer::new(4, 4).unwrap(),
        &header(enc::ZRLE, 0, 0, 4, 4),
        &bomb,
    );
    assert!(
        matches!(r, Err(DecodeError::Malformed(m)) if m.contains("larger")),
        "{r:?}"
    );
}

proptest! {
    /// Random structured images round-trip through the test encoder, the
    /// framer and the decoder.
    #[test]
    fn zrle_round_trips_random_images(
        w in 1u16..140,
        h in 1u16..140,
        seed in prop::collection::vec(any::<u8>(), 1..16),
    ) {
        let (data, image, _) = zrle_case(w, h, &seed);
        let payload = Server::new().chunk(&data);
        let hd = header(enc::ZRLE, 0, 0, w, h);
        prop_assert_eq!(frame(&hd, &PixelFormat::RGB888, &payload), Ok(payload.len()));
        let mut fb = Framebuffer::new(w, h).unwrap();
        Decoder::new(&PixelFormat::RGB888).unwrap().decode(&mut fb, &hd, &payload).unwrap();
        for y in 0..h {
            for x in 0..w {
                prop_assert_eq!(
                    fb.pixel(x, y),
                    Some(expected(&image[usize::from(y) * usize::from(w) + usize::from(x)]))
                );
            }
        }
    }

    /// Garbage never panics; it decodes or fails with an error.
    #[test]
    fn garbage_payloads_do_not_panic(body in prop::collection::vec(any::<u8>(), 0..200)) {
        let mut payload = (body.len() as u32).to_be_bytes().to_vec();
        payload.extend(&body);
        for e in [enc::ZLIB, enc::ZRLE] {
            let mut fb = Framebuffer::new(20, 20).unwrap();
            let _ = Decoder::new(&PixelFormat::RGB888)
                .unwrap()
                .decode(&mut fb, &header(e, 0, 0, 20, 20), &payload);
        }
    }
}
