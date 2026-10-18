//! The worker and the service's remote source, joined by real OS pipes and
//! driven with the server's test pattern (no Windows needed).

use std::{
    io::pipe,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use farpane_win::{remote::Hub, worker};
use rfb_codec::Framebuffer;
use rfb_server::{Input, Source, TestPattern};

#[derive(Debug, Default, Clone)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Input for Recorder {
    fn key(&self, keysym: u32, down: bool) {
        self.0
            .lock()
            .unwrap()
            .push(format!("key {keysym:x} {down}"));
    }
    fn pointer(&self, x: u16, y: u16, buttons: u8) {
        self.0
            .lock()
            .unwrap()
            .push(format!("ptr {x} {y} {buttons}"));
    }
    fn clipboard(&self, text: &str) {
        self.0.lock().unwrap().push(format!("clip {text}"));
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn picture_and_input_cross_the_pipes() {
    let (hub, mut source, input) = Hub::new();
    assert_eq!(source.size(), (1024, 768), "waiting size before a worker");
    let (to_service_r, to_service_w) = pipe().unwrap();
    let (to_worker_r, to_worker_w) = pipe().unwrap();
    hub.attach(to_service_r, to_worker_w);
    assert!(hub.attached());

    let recorder = Recorder::default();
    let seen = recorder.clone();
    let worker = thread::spawn(move || {
        worker::run(
            TestPattern::new(96, 64),
            recorder,
            to_worker_r,
            to_service_w,
            60,
        )
    });

    // The service side picks up the worker's size and the whole first picture.
    let mut fb = Framebuffer::new(1024, 768).unwrap();
    let mut local = Framebuffer::new(96, 64).unwrap();
    let mut got_whole = false;
    wait_for("the first frame", || {
        let c = source.capture(&mut fb).unwrap();
        got_whole |= c.damage.iter().any(|r| r.width == 96 && r.height == 64);
        (fb.width(), fb.height()) == (96, 64) && got_whole
    });
    assert_eq!(source.size(), (96, 64));
    assert_eq!(fb.pixels().len(), 96 * 64 * 4);
    // Later frames keep changing the picture (the test pattern animates).
    TestPattern::new(96, 64).capture(&mut local).unwrap();
    let mut changes = 0;
    wait_for("later damage", || {
        changes += usize::from(!source.capture(&mut fb).unwrap().damage.is_empty());
        changes >= 3
    });

    // Input goes the other way.
    input.key(0xFF0D, true);
    input.pointer(10, 20, 1);
    input.clipboard("grüße");
    wait_for("input", || seen.0.lock().unwrap().len() == 3);
    assert_eq!(
        *seen.0.lock().unwrap(),
        ["key ff0d true", "ptr 10 20 1", "clip grüße"]
    );

    // Detaching closes the service's end; the worker notices and returns.
    hub.detach();
    drop(input);
    worker.join().unwrap().unwrap();
    assert!(!hub.attached());
}

#[test]
fn a_new_worker_replaces_the_old_one() {
    let (hub, mut source, _input) = Hub::new();
    let mut fb = Framebuffer::new(1024, 768).unwrap();
    for (w, h) in [(64u16, 48u16), (80, 40)] {
        let (r1, w1) = pipe().unwrap();
        let (r2, w2) = pipe().unwrap();
        hub.attach(r1, w2);
        let handle = thread::spawn(move || {
            worker::run(TestPattern::new(w, h), Recorder::default(), r2, w1, 60)
        });
        wait_for("this worker's size", || {
            source.capture(&mut fb).unwrap();
            (fb.width(), fb.height()) == (w, h)
        });
        hub.detach();
        handle.join().unwrap().unwrap();
    }
}

#[test]
fn ctrl_alt_delete_goes_to_the_handler_not_the_worker() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (hub, _source, input) = Hub::new();
    let (r1, _w1) = pipe().unwrap();
    let (mut r2, w2) = pipe().unwrap();
    hub.attach(r1, w2);
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    input.on_secure_attention(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    // Delete alone, and with only one modifier, is an ordinary key.
    input.key(0xFFFF, true);
    input.key(0xFFE3, true);
    input.key(0xFFFF, true);
    assert_eq!(hits.load(Ordering::Relaxed), 0);
    // With both held it is the secure attention sequence, not forwarded.
    input.key(0xFFE9, true);
    input.key(0xFFFF, true);
    assert_eq!(hits.load(Ordering::Relaxed), 1);
    // Releasing a modifier ends it.
    input.key(0xFFE3, false);
    input.key(0xFFFF, true);
    assert_eq!(hits.load(Ordering::Relaxed), 1);
    drop(input);
    hub.detach();
    // The worker saw every key except the swallowed Delete.
    let mut seen = 0;
    while farpane_win::ipc::read_command(&mut r2)
        .ok()
        .flatten()
        .is_some()
    {
        seen += 1;
    }
    // Delete, Ctrl, Delete, Alt, Ctrl up, Delete
    assert_eq!(seen, 6);
}
