//! Continuous updates: the server pushes changes without a request each.

use std::{sync::Arc, time::Duration};

use farpane_viewer::{ConnectOptions, RemoteScreen};
use rfb_proto::consts::enc;
use rfb_server::{NoInput, Server, Settings, TestPattern};
use rfb_tokio::{ServerAuth, ServerOptions};
use tokio::{net::TcpListener, time::timeout};

/// A server whose picture never stops changing, and its address.
async fn animated() -> (String, Arc<Server>) {
    let server = Arc::new(
        Server::new(
            TestPattern::new(200, 100),
            NoInput,
            ServerOptions::new(ServerAuth::None, 1, 1, b"continuous"),
            Settings::default(),
        )
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let s = Arc::clone(&server);
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.serve_connection(sock).await });
        }
    });
    (addr, server)
}

async fn connect(addr: &str, continuous: bool) -> RemoteScreen {
    let options = ConnectOptions {
        continuous_updates: continuous,
        ..ConnectOptions::default()
    };
    RemoteScreen::connect_opts(addr, &options, &[enc::ZRLE, enc::RAW])
        .await
        .unwrap()
}

#[tokio::test]
async fn updates_keep_coming_without_further_requests() {
    let (addr, _server) = animated().await;
    let mut screen = connect(&addr, true).await;
    // The first update answers the opening request; everything after it is
    // pushed, because nobody asks again.
    for n in 0..6 {
        timeout(Duration::from_secs(5), screen.next_update())
            .await
            .unwrap_or_else(|_| panic!("update {n} never came"))
            .unwrap();
    }
}

#[tokio::test]
async fn without_the_option_the_server_waits_to_be_asked() {
    let (addr, _server) = animated().await;
    let mut screen = connect(&addr, false).await;
    timeout(Duration::from_secs(5), screen.next_update())
        .await
        .unwrap()
        .unwrap();
    // No request was sent: nothing more arrives.
    assert!(
        timeout(Duration::from_millis(600), screen.next_update())
            .await
            .is_err()
    );
    screen.request_update(true).await.unwrap();
    timeout(Duration::from_secs(5), screen.next_update())
        .await
        .unwrap()
        .unwrap();
}
