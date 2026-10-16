//! The rectangle framers over arbitrary headers, pixel formats and bytes.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rfb_proto::{PixelFormat, ProtoError, framing::frame, msg::server::RectHeader, types::Rect};

#[derive(Debug, Arbitrary)]
struct Input {
    encoding: i32,
    /// Fold the encoding into -20..20 so the real encodings are hit often.
    small: bool,
    rect: [u16; 4],
    /// 0, 1 or 2 select 8, 16 or 32 bits per pixel.
    bpp: u8,
    body: Vec<u8>,
}

fuzz_target!(|i: Input| {
    let bits = [8u8, 16, 32][usize::from(i.bpp % 3)];
    let pf = PixelFormat {
        bits_per_pixel: bits,
        depth: bits.min(24),
        ..PixelFormat::RGB888
    };
    let header = RectHeader {
        rect: Rect {
            x: i.rect[0],
            y: i.rect[1],
            width: i.rect[2],
            height: i.rect[3],
        },
        encoding: if i.small {
            i32::from(i.encoding as i8) % 20
        } else {
            i.encoding
        },
    };
    match frame(&header, &pf, &i.body) {
        Ok(n) => {
            assert!(n <= i.body.len());
            // Framing is a pure function of the first `n` bytes.
            assert_eq!(frame(&header, &pf, &i.body[..n]), Ok(n));
        }
        Err(ProtoError::NeedMore(n)) => assert!(n > 0),
        Err(_) => {}
    }
});
