//! A Farpane server and client talking H.264 through the real `OpenH264`
//! library. Skipped when the library is not installed (see `roundtrip.rs`).

use std::{path::PathBuf, sync::Arc, time::Duration};

use farpane_h264::OpenH264;
use rfb_codec::{Damage, Decoder, Framebuffer};
use rfb_proto::{
    PixelFormat, conn::ClientConnEvent, consts::enc, msg::client::ClientMessage, types::Rect,
};
use rfb_server::{NoInput, Server, Settings, TestPattern};
use rfb_tokio::{ClientOptions, ClientSession, ServerAuth, ServerOptions};
use tokio::io::duplex;

fn library() -> Option<OpenH264> {
    let dir = std::env::var_os("FARPANE_H264_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/h264-test"),
        PathBuf::from,
    );
    OpenH264::open(&dir).expect("a file that is there loads")
}

#[tokio::test]
async fn the_test_pattern_arrives_through_h264_and_follows_changes() {
    let Some(codec) = library() else {
        eprintln!("OpenH264 is not installed: skipped");
        return;
    };
    let codec = Arc::new(codec);
    let server = Arc::new(
        Server::new(
            TestPattern::new(160, 96),
            NoInput,
            ServerOptions::new(ServerAuth::None, 1, 1, b"h264"),
            Settings {
                fps: 30,
                view_only: false,
            },
        )
        .unwrap(),
    );
    server.set_video(Some(codec.clone()));
    let (near, far) = duplex(1 << 20);
    let s = Arc::clone(&server);
    tokio::spawn(async move { s.serve_connection(far).await });
    let (mut session, info) = ClientSession::connect(near, ClientOptions::default())
        .await
        .unwrap();
    let pf = PixelFormat::RGB888;
    session
        .send(&ClientMessage::SetPixelFormat(pf))
        .await
        .unwrap();
    session
        .send(&ClientMessage::SetEncodings(vec![enc::OPEN_H264, enc::RAW]))
        .await
        .unwrap();
    let mut fb = Framebuffer::new(info.width, info.height).unwrap();
    let mut decoder = Decoder::new(&pf).unwrap();
    decoder.set_video(codec);
    let whole = Rect {
        x: 0,
        y: 0,
        width: info.width,
        height: info.height,
    };
    let mut h264_rects = 0;
    for round in 0..4 {
        session
            .send(&ClientMessage::FramebufferUpdateRequest {
                incremental: round > 0,
                rect: whole,
            })
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), session.next_event())
                .await
                .expect("the server answered")
                .unwrap();
            match event {
                ClientConnEvent::Rect { header, payload } => {
                    if header.encoding == enc::OPEN_H264 {
                        h264_rects += 1;
                        assert_eq!(header.rect, whole, "one context over the whole screen");
                    }
                    let damage = decoder.decode(&mut fb, &header, &payload).unwrap();
                    assert!(
                        matches!(damage, Damage::Rect(_) | Damage::None),
                        "{damage:?}"
                    );
                }
                ClientConnEvent::UpdateEnd => break,
                _ => {}
            }
        }
    }
    assert!(h264_rects >= 3, "H.264 was used: {h264_rects} rectangles");
    // The picture on the client is close to the server's.
    let (a, b) = (fb.pixels(), server_pixels(&server));
    assert_eq!(a.len(), b.len());
    let total: u64 = a
        .iter()
        .zip(&b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    #[allow(clippy::cast_precision_loss)]
    let mean = total as f64 / a.len() as f64;
    assert!(mean < 25.0, "the pictures differ by {mean} per byte");
    assert!(a.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]), "not blank");
}

/// A copy of the server's current picture.
fn server_pixels(server: &Server) -> Vec<u8> {
    let preview = server.preview(4096);
    // `preview` is R, G, B, 0xFF rows; the client has B, G, R, X.
    preview
        .rgba
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect()
}
