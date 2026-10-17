//! The viewer asks for H.264 when it has a codec, and draws what comes back.
//! The codec here is a stand-in: "H.264" is the picture itself, so the test
//! covers the viewer's side (the request, the contexts, the cropping) without
//! Cisco's library. `crates/farpane-h264/tests` runs the real one.

use std::sync::Arc;

use farpane_viewer::{ConnectOptions, DEFAULT_ENCODINGS, RemoteScreen};
use rfb_codec::video::{Picture, VideoCodec, VideoDecoder, VideoEncoder};
use rfb_proto::consts::enc;
use rfb_server::{NoInput, Server, Settings, TestPattern};
use rfb_tokio::{ServerAuth, ServerOptions};
use tokio::net::TcpListener;

#[derive(Debug)]
struct Stand;

#[derive(Debug)]
struct StandEncoder;

#[derive(Debug)]
struct StandDecoder;

impl VideoEncoder for StandEncoder {
    fn encode(
        &mut self,
        bgrx: &[u8],
        width: u16,
        height: u16,
        stride: usize,
        _key: bool,
    ) -> Result<Vec<u8>, String> {
        let mut out = width.to_be_bytes().to_vec();
        out.extend_from_slice(&height.to_be_bytes());
        for row in 0..usize::from(height) {
            out.extend_from_slice(&bgrx[row * stride..][..usize::from(width) * 4]);
        }
        Ok(out)
    }
}

impl VideoDecoder for StandDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
        Ok(Some(Picture {
            width: u16::from_be_bytes([data[0], data[1]]),
            height: u16::from_be_bytes([data[2], data[3]]),
            bgrx: data[4..].to_vec(),
        }))
    }
}

impl VideoCodec for Stand {
    fn decoder(&self) -> Result<Box<dyn VideoDecoder>, String> {
        Ok(Box::new(StandDecoder))
    }

    fn encoder(&self, _: u16, _: u16) -> Result<Box<dyn VideoEncoder>, String> {
        Ok(Box::new(StandEncoder))
    }
}

async fn serve(with_video: bool) -> String {
    let server = Arc::new(
        Server::new(
            TestPattern::new(160, 96),
            NoInput,
            ServerOptions::new(ServerAuth::None, 1, 1, b"video"),
            Settings {
                fps: 60,
                view_only: false,
            },
        )
        .unwrap(),
    );
    if with_video {
        server.set_video(Some(Arc::new(Stand)));
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(server.listen(listener));
    addr
}

async fn draw(addr: &str, video: Option<Arc<dyn VideoCodec>>) -> (RemoteScreen, String) {
    let options = ConnectOptions {
        video,
        ..ConnectOptions::default()
    };
    let mut screen = RemoteScreen::connect_opts(addr, &options, &DEFAULT_ENCODINGS)
        .await
        .unwrap();
    for _ in 0..3 {
        screen.next_update().await.unwrap();
        screen.request_update(true).await.unwrap();
    }
    let snapshot = screen.sample_stats(std::time::Instant::now());
    let used = farpane_viewer::stats::pixel_mix(&snapshot, 1)
        .first()
        .map(|(e, _)| farpane_viewer::stats::encoding_name(*e))
        .unwrap_or_default();
    (screen, used)
}

#[tokio::test]
async fn a_viewer_with_a_codec_gets_h264_from_a_server_that_has_one() {
    let addr = serve(true).await;
    let (screen, used) = draw(&addr, Some(Arc::new(Stand))).await;
    assert_eq!(used, "H.264");
    assert!(
        screen
            .framebuffer()
            .pixels()
            .chunks_exact(4)
            .any(|p| p[..3] != [0, 0, 0]),
        "the picture was drawn"
    );
    // Without a codec the same server sends something else, and the viewer
    // never asks for H.264.
    let (_screen, used) = draw(&addr, None).await;
    assert_ne!(used, "H.264");
    assert!(DEFAULT_ENCODINGS.iter().all(|e| *e != enc::OPEN_H264));
}

#[tokio::test]
async fn a_server_without_a_codec_is_still_usable_by_a_viewer_that_asks() {
    let addr = serve(false).await;
    let (screen, used) = draw(&addr, Some(Arc::new(Stand))).await;
    assert_ne!(used, "H.264");
    assert!(
        screen
            .framebuffer()
            .pixels()
            .chunks_exact(4)
            .any(|p| p[..3] != [0, 0, 0])
    );
}
