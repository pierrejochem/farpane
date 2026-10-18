//! Extended Clipboard: message layout and the exchange between two machines.

use std::io::{Read, Write};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use rfb_codec::clipboard::{
    Action, ClipboardError, ExtClipboard, Reaction, Role, caps, flags, notify, parse, provide_text,
    request_text, wire_size,
};

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

fn unzlib(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    ZlibDecoder::new(data).read_to_end(&mut out).unwrap();
    out
}

#[test]
fn capabilities_round_trip_and_the_sizes_must_match_the_formats() {
    let (f, p) = caps(0);
    assert_eq!(
        f,
        flags::CAPS | flags::TEXT | flags::REQUEST | flags::PEEK | flags::NOTIFY | flags::PROVIDE
    );
    assert_eq!(p, [0, 0, 0, 0]);
    assert_eq!(
        parse(f, &p),
        Ok(Action::Caps {
            formats: flags::TEXT,
            actions: flags::REQUEST | flags::PEEK | flags::NOTIFY | flags::PROVIDE,
            sizes: vec![0],
        })
    );
    // Two announced formats need two sizes, in bit order.
    let two = flags::CAPS | flags::TEXT | flags::HTML;
    let Ok(Action::Caps { sizes, .. }) = parse(two, &[0, 0, 1, 0, 0, 0, 0, 5]) else {
        panic!("expected caps");
    };
    assert_eq!(sizes, [256, 5]);
    assert!(matches!(
        parse(two, &[0, 0, 0, 0]),
        Err(ClipboardError::Malformed(_))
    ));
    assert!(matches!(parse(f, &[]), Err(ClipboardError::Malformed(_))));
}

#[test]
fn a_provide_has_the_layout_the_spec_describes() {
    let (f, payload) = provide_text("a\nb").unwrap();
    assert_eq!(f, flags::PROVIDE | flags::TEXT);
    // Independently inflated: U32 size, then CR LF text with a terminating null.
    let raw = unzlib(&payload);
    assert_eq!(&raw[..4], 5u32.to_be_bytes(), "size counts the null");
    assert_eq!(&raw[4..], b"a\r\nb\0");
    assert_eq!(
        parse(f, &payload),
        Ok(Action::Provide {
            formats: flags::TEXT,
            text: Some("a\r\nb".into())
        })
    );
}

#[test]
fn line_ends_on_the_wire_are_crlf_whatever_the_input() {
    for (input, want) in [
        ("a\nb", "a\r\nb"),
        ("a\r\nb", "a\r\nb"),
        ("a\rb", "a\r\nb"),
        ("", ""),
    ] {
        let (f, p) = provide_text(input).unwrap();
        let Ok(Action::Provide { text: Some(t), .. }) = parse(f, &p) else {
            panic!()
        };
        assert_eq!(t, want, "{input:?}");
    }
    assert_eq!(wire_size("a\nb"), 5);
}

#[test]
fn unicode_round_trips() {
    let text = "h\u{e9}llo \u{4f60}\u{597d} \u{1f600}";
    let (f, p) = provide_text(text).unwrap();
    assert_eq!(
        parse(f, &p),
        Ok(Action::Provide {
            formats: flags::TEXT,
            text: Some(text.into())
        })
    );
}

#[test]
fn other_formats_in_a_provide_are_skipped_and_text_is_found_whatever_its_position() {
    // Formats TEXT and HTML: data pairs come in bit order, TEXT first.
    let mut raw = Vec::new();
    raw.extend_from_slice(&3u32.to_be_bytes());
    raw.extend_from_slice(b"hi\0");
    raw.extend_from_slice(&6u32.to_be_bytes());
    raw.extend_from_slice(b"<b>x</");
    let f = flags::PROVIDE | flags::TEXT | flags::HTML;
    assert_eq!(
        parse(f, &zlib(&raw)),
        Ok(Action::Provide {
            formats: flags::TEXT | flags::HTML,
            text: Some("hi".into())
        })
    );
    // HTML only: no text.
    let mut html = Vec::new();
    html.extend_from_slice(&2u32.to_be_bytes());
    html.extend_from_slice(b"ok");
    assert_eq!(
        parse(flags::PROVIDE | flags::HTML, &zlib(&html)),
        Ok(Action::Provide {
            formats: flags::HTML,
            text: None
        })
    );
}

#[test]
fn invalid_utf8_is_replaced_not_fatal() {
    let mut raw = Vec::new();
    raw.extend_from_slice(&4u32.to_be_bytes());
    raw.extend_from_slice(&[b'a', 0xFF, b'b', 0]);
    let Ok(Action::Provide { text: Some(t), .. }) =
        parse(flags::PROVIDE | flags::TEXT, &zlib(&raw))
    else {
        panic!("expected text");
    };
    assert_eq!(t, "a\u{fffd}b");
}

#[test]
fn damaged_provide_payloads_are_errors() {
    let f = flags::PROVIDE | flags::TEXT;
    assert!(matches!(
        parse(f, b"not zlib"),
        Err(ClipboardError::Malformed(_))
    ));
    assert!(
        matches!(parse(f, &zlib(&[0, 0])), Err(ClipboardError::Malformed(_))),
        "no room for a size"
    );
    // A size that runs past the data.
    let mut raw = 100u32.to_be_bytes().to_vec();
    raw.extend_from_slice(b"short");
    assert!(matches!(
        parse(f, &zlib(&raw)),
        Err(ClipboardError::Malformed(_))
    ));
    // A format listed but its data missing.
    assert!(matches!(
        parse(f, &zlib(&[])),
        Err(ClipboardError::Malformed(_))
    ));
}

#[test]
fn a_zip_bomb_is_refused_without_inflating_it() {
    // 20 MiB of zeros compress to a few KiB; the limit is 16 MiB.
    let mut raw = (20u32 * 1024 * 1024).to_be_bytes().to_vec();
    raw.extend(std::iter::repeat_n(0u8, 20 * 1024 * 1024));
    let bomb = zlib(&raw);
    assert!(
        bomb.len() < 100_000,
        "the test data should be small: {}",
        bomb.len()
    );
    assert_eq!(
        parse(flags::PROVIDE | flags::TEXT, &bomb),
        Err(ClipboardError::TooLarge)
    );
}

#[test]
fn exactly_one_action_per_message_and_no_stray_data() {
    assert!(matches!(
        parse(flags::REQUEST | flags::NOTIFY | flags::TEXT, &[]),
        Err(ClipboardError::Malformed(_))
    ));
    assert!(
        matches!(parse(flags::TEXT, &[]), Err(ClipboardError::Malformed(_))),
        "no action at all"
    );
    assert!(matches!(
        parse(flags::REQUEST | flags::TEXT, &[1]),
        Err(ClipboardError::Malformed(_))
    ));
    assert_eq!(parse(flags::PEEK, &[]), Ok(Action::Peek));
    assert_eq!(
        parse(request_text().0, &[]),
        Ok(Action::Request {
            formats: flags::TEXT
        })
    );
    assert_eq!(
        parse(notify(true).0, &[]),
        Ok(Action::Notify {
            formats: flags::TEXT
        })
    );
    assert_eq!(
        parse(notify(false).0, &[]),
        Ok(Action::Notify { formats: 0 })
    );
}

// --- two machines against each other ---------------------------------------

/// Deliver `messages` to `to`, collecting what it sends back and any text.
fn deliver(to: &mut ExtClipboard, messages: &[(u32, Vec<u8>)]) -> Reaction {
    let mut total = Reaction::default();
    for (f, p) in messages {
        let r = to.on_message(*f, p).unwrap();
        total.send.extend(r.send);
        total.received = r.received.or(total.received);
    }
    total
}

/// A server and a client that have exchanged capabilities.
fn ready(server_text_max: u32) -> (ExtClipboard, ExtClipboard) {
    let mut server = ExtClipboard::new(Role::Server);
    let mut client = ExtClipboard::new(Role::Client);
    assert!(!server.is_active() && !client.is_active());
    // The server announces itself (with its own limit for this test).
    let _ = server.announce();
    let server_caps = caps(server_text_max);
    let reply = deliver(&mut client, &[server_caps]);
    assert_eq!(
        reply.send,
        [caps(0)],
        "the client answers once with its own capabilities"
    );
    assert_eq!(
        deliver(&mut server, &reply.send).send.len(),
        0,
        "the server does not answer an answer"
    );
    assert!(server.is_active() && client.is_active());
    (server, client)
}

#[test]
fn the_client_answers_capabilities_only_once() {
    let mut client = ExtClipboard::new(Role::Client);
    let first = client.on_message(caps(0).0, &caps(0).1).unwrap();
    assert_eq!(first.send.len(), 1);
    let second = client.on_message(caps(0).0, &caps(0).1).unwrap();
    assert_eq!(second.send.len(), 0);
}

#[test]
fn a_change_on_the_server_reaches_the_client_via_notify_request_provide() {
    let (mut server, mut client) = ready(0);
    // Server clipboard changes; the client's limit is 0, so only a notify goes out.
    let sent = server.local_changed("from the server\nline two");
    assert_eq!(sent, [notify(true)]);
    let ask = deliver(&mut client, &sent);
    assert_eq!(ask.send, [request_text()]);
    let data = deliver(&mut server, &ask.send);
    assert_eq!(data.send.len(), 1);
    let got = deliver(&mut client, &data.send);
    assert_eq!(got.received.as_deref(), Some("from the server\r\nline two"));
}

#[test]
fn a_small_change_can_be_provided_unsolicited_when_the_peer_allows_it() {
    // The server accepts up to 1 KiB unsolicited.
    let (_server, mut client) = ready(1024);
    let sent = client.local_changed("short");
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].0,
        flags::PROVIDE | flags::TEXT,
        "no round trip needed"
    );
    // Too large for that limit: announced instead.
    let big = "x".repeat(2000);
    assert_eq!(client.local_changed(&big), [notify(true)]);
}

#[test]
fn a_request_for_text_we_do_not_have_gets_no_answer() {
    let (_server, mut client) = ready(0);
    assert_eq!(deliver(&mut client, &[request_text()]).send.len(), 0);
    // After a change it does.
    client.local_changed("now there is some");
    assert_eq!(deliver(&mut client, &[request_text()]).send.len(), 1);
}

#[test]
fn peek_is_answered_with_what_is_available() {
    let (_server, mut client) = ready(0);
    assert_eq!(
        deliver(&mut client, &[(flags::PEEK, vec![])]).send,
        [notify(false)]
    );
    client.local_changed("x");
    assert_eq!(
        deliver(&mut client, &[(flags::PEEK, vec![])]).send,
        [notify(true)]
    );
}

#[test]
fn nothing_is_sent_before_the_peer_has_announced_text() {
    let mut client = ExtClipboard::new(Role::Client);
    assert_eq!(
        client.local_changed("hello").len(),
        0,
        "the caller falls back to Latin-1"
    );
    // A peer that announces only HTML never gets asked for text.
    let html_caps = (
        flags::CAPS | flags::HTML | flags::REQUEST | flags::NOTIFY | flags::PROVIDE,
        vec![0; 4],
    );
    deliver(&mut client, &[html_caps]);
    assert!(!client.is_active());
    assert_eq!(client.local_changed("hello").len(), 0);
    assert_eq!(deliver(&mut client, &[notify(true)]).send.len(), 0);
}

#[test]
fn a_notify_without_text_asks_for_nothing() {
    let (_server, mut client) = ready(0);
    assert_eq!(deliver(&mut client, &[notify(false)]).send.len(), 0);
    assert_eq!(
        deliver(&mut client, &[(flags::NOTIFY | flags::HTML, vec![])])
            .send
            .len(),
        0
    );
}

/// `TigerVNC` ends a provide with a sync flush and never finishes the zlib
/// stream (found against a real Xvnc: the first version of the decoder called
/// that "zlib data" damage). The text must still come out, and a message cut
/// short must still fail.
#[test]
fn a_provide_whose_zlib_stream_is_not_finished_is_accepted() {
    use flate2::{Compress, FlushCompress};
    let text = "zurück ☺ from X";
    let mut body = text.as_bytes().to_vec();
    body.push(0);
    let mut inflated = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    inflated.extend_from_slice(&body);

    let mut z = Compress::new(Compression::default(), true);
    let mut sync = Vec::with_capacity(inflated.len() + 64);
    z.compress_vec(&inflated, &mut sync, FlushCompress::Sync)
        .unwrap();
    assert_ne!(sync.len(), 0);
    match parse(flags::PROVIDE | flags::TEXT, &sync).unwrap() {
        Action::Provide { text: got, .. } => assert_eq!(got.as_deref(), Some(text)),
        other => panic!("{other:?}"),
    }
    // Cut inside the data (not just the empty sync marker): the size check catches it.
    assert!(parse(flags::PROVIDE | flags::TEXT, &sync[..sync.len() / 2]).is_err());
}
