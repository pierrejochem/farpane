//! Property tests: `parse(write(m)) == m`, prefixes never parse, and
//! arbitrary bytes never panic.

use alloc::{vec, vec::Vec};

use proptest::prelude::*;

use super::{client::ClientMessage, server::ServerMessage};
use crate::{
    ProtoError,
    types::{PixelFormat, Rect, Screen},
};

fn pixel_format() -> impl Strategy<Value = PixelFormat> {
    (
        prop_oneof![Just(8u8), Just(16), Just(32)],
        any::<bool>(),
        any::<bool>(),
        any::<[u16; 3]>(),
        any::<[u8; 3]>(),
    )
        .prop_flat_map(|(bpp, big_endian, true_colour, max, shift)| {
            (0..=bpp).prop_map(move |depth| PixelFormat {
                bits_per_pixel: bpp,
                depth,
                big_endian,
                true_colour,
                red_max: max[0],
                green_max: max[1],
                blue_max: max[2],
                red_shift: shift[0],
                green_shift: shift[1],
                blue_shift: shift[2],
            })
        })
}

fn client_message() -> impl Strategy<Value = ClientMessage> {
    prop_oneof![
        pixel_format().prop_map(ClientMessage::SetPixelFormat),
        prop::collection::vec(any::<i32>(), 0..64).prop_map(ClientMessage::SetEncodings),
        (any::<bool>(), any::<[u16; 4]>()).prop_map(|(incremental, r)| {
            ClientMessage::FramebufferUpdateRequest {
                incremental,
                rect: Rect {
                    x: r[0],
                    y: r[1],
                    width: r[2],
                    height: r[3],
                },
            }
        }),
        (any::<bool>(), any::<u32>())
            .prop_map(|(down, keysym)| ClientMessage::KeyEvent { down, keysym }),
        (any::<u8>(), any::<u16>(), any::<u16>())
            .prop_map(|(buttons, x, y)| ClientMessage::PointerEvent { buttons, x, y }),
        prop::collection::vec(any::<u8>(), 0..256).prop_map(ClientMessage::ClientCutText),
        (any::<u32>(), prop::collection::vec(any::<u8>(), 0..256))
            .prop_map(|(flags, payload)| ClientMessage::ExtendedClipboard { flags, payload }),
        (
            0..=crate::consts::limits::MAX_FB_DIMENSION,
            0..=crate::consts::limits::MAX_FB_DIMENSION,
            prop::collection::vec(any::<(u32, [u16; 4], u32)>(), 0..8),
        )
            .prop_map(|(width, height, screens)| ClientMessage::SetDesktopSize {
                width,
                height,
                screens: screens
                    .into_iter()
                    .map(|(id, r, flags)| Screen {
                        id,
                        x: r[0],
                        y: r[1],
                        width: r[2],
                        height: r[3],
                        flags,
                    })
                    .collect(),
            }),
    ]
}

fn server_message() -> impl Strategy<Value = ServerMessage> {
    prop_oneof![
        any::<u16>().prop_map(|rects| ServerMessage::UpdateBegin { rects }),
        (
            any::<u16>(),
            prop::collection::vec(any::<[u16; 3]>(), 0..64)
        )
            .prop_map(|(first, colours)| ServerMessage::SetColourMapEntries { first, colours }),
        Just(ServerMessage::Bell),
        prop::collection::vec(any::<u8>(), 0..256).prop_map(ServerMessage::ServerCutText),
        (any::<u32>(), prop::collection::vec(any::<u8>(), 0..256))
            .prop_map(|(flags, payload)| ServerMessage::ExtendedClipboard { flags, payload }),
    ]
}

proptest! {
    #[test]
    fn client_round_trips(msg in client_message()) {
        let mut bytes = Vec::new();
        msg.write(&mut bytes).unwrap();
        prop_assert_eq!(ClientMessage::parse(&bytes), Ok((msg, bytes.len())));
    }

    #[test]
    fn server_round_trips(msg in server_message()) {
        let mut bytes = Vec::new();
        msg.write(&mut bytes).unwrap();
        prop_assert_eq!(ServerMessage::parse(&bytes), Ok((msg, bytes.len())));
    }

    #[test]
    fn client_prefixes_need_more(msg in client_message()) {
        let mut bytes = Vec::new();
        msg.write(&mut bytes).unwrap();
        for cut in 0..bytes.len() {
            prop_assert!(matches!(
                ClientMessage::parse(&bytes[..cut]),
                Err(ProtoError::NeedMore(n)) if n > 0
            ));
        }
    }

    #[test]
    fn server_prefixes_need_more(msg in server_message()) {
        let mut bytes = Vec::new();
        msg.write(&mut bytes).unwrap();
        for cut in 0..bytes.len() {
            prop_assert!(matches!(
                ServerMessage::parse(&bytes[..cut]),
                Err(ProtoError::NeedMore(n)) if n > 0
            ));
        }
    }

    /// Whatever the bytes, parsing returns and never claims more than it got.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Ok((_, used)) = ClientMessage::parse(&bytes) {
            prop_assert!(used <= bytes.len());
        }
        if let Ok((_, used)) = ServerMessage::parse(&bytes) {
            prop_assert!(used <= bytes.len());
        }
    }

    /// A stream of messages split at a random point parses to the same list.
    #[test]
    fn client_stream_split_anywhere(
        msgs in prop::collection::vec(client_message(), 1..6),
        split in any::<prop::sample::Index>(),
    ) {
        let mut stream = Vec::new();
        for m in &msgs {
            m.write(&mut stream).unwrap();
        }
        let at = split.index(stream.len() + 1);
        let mut buf = Vec::new();
        let mut got = vec![];
        for part in [&stream[..at], &stream[at..]] {
            buf.extend_from_slice(part);
            while let Ok((m, used)) = ClientMessage::parse(&buf) {
                got.push(m);
                buf.drain(..used);
            }
        }
        prop_assert_eq!(got, msgs);
        prop_assert_eq!(buf.len(), 0);
    }
}
