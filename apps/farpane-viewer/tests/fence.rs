//! Fence: round-trip time and answering the server's requests.

use std::time::Duration;

use farpane_viewer::RemoteScreen;
use rfb_proto::{
    consts::{fence, pseudo},
    msg::{client::ClientMessage, server::ServerMessage},
};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};

type Server = ServerSession<TcpStream>;

async fn connect() -> (RemoteScreen, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        ServerSession::accept(sock, ServerOptions::new(ServerAuth::None, 8, 4, b"fence"))
            .await
            .unwrap()
    });
    let screen = RemoteScreen::connect(&addr, None).await.unwrap();
    let mut server = task.await.unwrap();
    // SetPixelFormat, SetEncodings (with Fence), the first update request.
    assert!(matches!(
        server.next_message().await.unwrap(),
        ClientMessage::SetPixelFormat(_)
    ));
    let ClientMessage::SetEncodings(list) = server.next_message().await.unwrap() else {
        panic!("expected SetEncodings");
    };
    assert!(list.contains(&pseudo::FENCE));
    server.next_message().await.unwrap();
    (screen, server)
}

/// Let the viewer read what the server sent; it reports no event.
async fn pump(screen: &mut RemoteScreen) {
    assert!(
        timeout(Duration::from_millis(300), screen.next_event())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn no_ping_is_sent_until_the_server_has_announced_fence() {
    let (mut screen, _server) = connect().await;
    assert!(!screen.ping().await.unwrap());
}

#[tokio::test]
async fn a_ping_measures_the_round_trip_when_the_server_answers() {
    let (mut screen, mut server) = connect().await;
    // The server announces Fence with a request; the viewer answers it.
    server
        .send(&ServerMessage::Fence {
            flags: fence::REQUEST,
            payload: vec![],
        })
        .await
        .unwrap();
    pump(&mut screen).await;
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::Fence {
            flags: 0,
            payload: vec![]
        }
    );
    assert_eq!(screen.sample_stats(std::time::Instant::now()).rtt, None);

    assert!(screen.ping().await.unwrap());
    let ClientMessage::Fence { flags, payload } = server.next_message().await.unwrap() else {
        panic!("expected ClientFence");
    };
    assert_eq!(flags, fence::REQUEST | fence::BLOCK_BEFORE);
    assert_eq!(payload.len(), 8);
    tokio::time::sleep(Duration::from_millis(40)).await;
    server
        .send(&ServerMessage::Fence {
            flags: flags & !fence::REQUEST,
            payload,
        })
        .await
        .unwrap();
    pump(&mut screen).await;
    let rtt = screen
        .sample_stats(std::time::Instant::now())
        .rtt
        .expect("an answer was read");
    assert!(rtt >= Duration::from_millis(40), "{rtt:?}");
    assert!(rtt < Duration::from_secs(2), "{rtt:?}");
}

#[tokio::test]
async fn the_viewer_answers_requests_keeping_only_the_flags_it_knows() {
    let (mut screen, mut server) = connect().await;
    server
        .send(&ServerMessage::Fence {
            flags: fence::REQUEST | fence::BLOCK_BEFORE | fence::SYNC_NEXT | (1 << 9),
            payload: vec![5, 6, 7],
        })
        .await
        .unwrap();
    pump(&mut screen).await;
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::Fence {
            flags: fence::BLOCK_BEFORE | fence::SYNC_NEXT,
            payload: vec![5, 6, 7]
        },
        "the request bit and the unknown bit are cleared, the payload echoed"
    );
}

#[tokio::test]
async fn an_unmatched_answer_is_ignored_and_pings_do_not_pile_up() {
    let (mut screen, mut server) = connect().await;
    server
        .send(&ServerMessage::Fence {
            flags: fence::REQUEST,
            payload: vec![],
        })
        .await
        .unwrap();
    pump(&mut screen).await;
    server.next_message().await.unwrap();
    // An answer to nothing we asked, with an odd payload size.
    server
        .send(&ServerMessage::Fence {
            flags: 0,
            payload: vec![1, 2],
        })
        .await
        .unwrap();
    pump(&mut screen).await;
    assert_eq!(screen.sample_stats(std::time::Instant::now()).rtt, None);
    // Four unanswered requests are the most that stay in flight.
    for _ in 0..4 {
        assert!(screen.ping().await.unwrap());
    }
    assert!(!screen.ping().await.unwrap());
}
