//! Message parsers: no panics, and anything accepted round-trips.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rfb_proto::msg::{client::ClientMessage, server::ServerMessage};

fuzz_target!(|data: &[u8]| {
    if let Ok((msg, used)) = ClientMessage::parse(data) {
        assert!(used <= data.len());
        let mut out = Vec::new();
        msg.write(&mut out).expect("accepted message serialises");
        assert_eq!(ClientMessage::parse(&out), Ok((msg, out.len())));
    }
    if let Ok((msg, used)) = ServerMessage::parse(data) {
        assert!(used <= data.len());
        let mut out = Vec::new();
        msg.write(&mut out).expect("accepted message serialises");
        assert_eq!(ServerMessage::parse(&out), Ok((msg, out.len())));
    }
});
