//! Live check of the Windows backend on the interactive desktop (run it from
//! a scheduled task with `/IT`, or from a console of that session):
//! `probe OUTDIR` saves `before.ppm` / `after.ppm` and prints what it saw.
//! It starts Notepad, types into it and closes it again.

// A throw-away live check, not library code.
#![allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::undocumented_unsafe_blocks,
    clippy::wildcard_imports,
    clippy::chunks_exact_to_as_chunks,
    clippy::manual_midpoint,
    clippy::cast_sign_loss,
    clippy::default_trait_access
)]

#[cfg(not(windows))]
fn main() {
    eprintln!("this probe only runs on Windows");
}

#[cfg(windows)]
fn main() {
    use std::{path::PathBuf, process::Command, thread::sleep, time::Duration};

    use rfb_codec::Framebuffer;
    use rfb_server::{Input, Source};
    use windows::Win32::UI::{
        Input::KeyboardAndMouse::GetAsyncKeyState, WindowsAndMessaging::GetCursorPos,
    };

    fn save(fb: &Framebuffer, path: &PathBuf) {
        let mut out = format!("P6\n{} {}\n255\n", fb.width(), fb.height()).into_bytes();
        for px in fb.pixels().chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        std::fs::write(path, out).unwrap();
    }

    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: probe OUTDIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut source, input) = farpane_win::open().expect("open");
    let (w, h) = source.size();
    println!("screen {w}x{h}");
    let mut fb = Framebuffer::new(w, h).unwrap();

    // 1. first frame: everything is damaged, a cursor shape comes along.
    let first = source.capture(&mut fb).unwrap();
    println!("first: damage {:?}", first.damage);
    match &first.cursor {
        Some(c) => println!(
            "cursor {}x{} hotspot {:?} bytes {}",
            c.width,
            c.height,
            c.hotspot,
            c.rgba.len()
        ),
        None => println!("cursor: none reported (FAIL)"),
    }
    let painted = fb
        .pixels()
        .chunks_exact(4)
        .filter(|p| p[..3] != [0, 0, 0])
        .count();
    println!(
        "non-black pixels: {painted} of {}",
        usize::from(w) * usize::from(h)
    );
    save(&fb, &dir.join("before.ppm"));

    // 2. a quiet screen gives little or no damage.
    sleep(Duration::from_millis(300));
    let quiet = source.capture(&mut fb).unwrap();
    println!("quiet: {} tiles", quiet.damage.len());

    // 3. pointer: move, read back.
    for (x, y) in [(100u16, 120u16), (400, 300), (w / 2, h / 2)] {
        input.pointer(x, y, 0);
        sleep(Duration::from_millis(100));
        let mut p = windows::Win32::Foundation::POINT::default();
        unsafe { GetCursorPos(&raw mut p).unwrap() };
        println!("pointer asked ({x},{y}) got ({},{})", p.x, p.y);
    }

    // 4. keys: 'a' and Shift+'1', read back through the async key state.
    input.key(u32::from(b'a'), true);
    sleep(Duration::from_millis(50));
    let down = unsafe { GetAsyncKeyState(0x41) } < 0;
    input.key(u32::from(b'a'), false);
    sleep(Duration::from_millis(50));
    let up = unsafe { GetAsyncKeyState(0x41) } >= 0;
    println!("key a: down seen {down}, up seen {up}");

    // 5. clipboard out of the machine (set by another program), then in.
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Set-Clipboard -Value ('from outside ' + [char]0xE4 + [char]0x20AC)",
        ])
        .status()
        .unwrap();
    sleep(Duration::from_millis(300));
    let c = source.capture(&mut fb).unwrap();
    println!(
        "clipboard seen (want 'from outside' + e4 20ac): {:?}",
        c.clipboard.as_ref().map(|t| t
            .chars()
            .map(|c| format!("{:x}", c as u32))
            .collect::<Vec<_>>()
            .join(" "))
    );
    input.clipboard("from a client\nline two \u{f6}\u{2713}");
    let got = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-Clipboard -Raw).ToCharArray() | % { '{0:x}' -f [int]$_ }",
        ])
        .output()
        .unwrap();
    println!(
        "clipboard set (want ... a 0d 0a ... f6 2713): {}",
        String::from_utf8_lossy(&got.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    );
    let c = source.capture(&mut fb).unwrap();
    println!("own clipboard echoed back: {:?} (want None)", c.clipboard);

    // 6. a window of our own appears, gets clicked and typed into, and is read back.
    use windows::{
        Win32::{
            Foundation::{HWND, RECT},
            UI::WindowsAndMessaging::*,
        },
        core::w,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            w!("EDIT"),
            w!(""),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE | WINDOW_STYLE(ES_MULTILINE as u32),
            200,
            150,
            900,
            500,
            None,
            None,
            None,
            None,
        )
        .expect("window");
        tx.send(hwnd.0 as isize).unwrap();
        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    });
    let hwnd = HWND(rx.recv().unwrap() as *mut _);
    sleep(Duration::from_millis(800));
    let appeared = source.capture(&mut fb).unwrap();
    println!("window appeared: {} tiles", appeared.damage.len());
    let (cx, cy) = unsafe {
        let _ = SetForegroundWindow(hwnd);
        sleep(Duration::from_millis(300));
        let mut r = RECT::default();
        GetWindowRect(hwnd, &raw mut r).unwrap();
        (
            u16::try_from((r.left + r.right) / 2).unwrap(),
            u16::try_from((r.top + r.bottom) / 2).unwrap(),
        )
    };
    input.pointer(cx, cy, 1); // click to focus
    input.pointer(cx, cy, 0);
    sleep(Duration::from_millis(300));
    let mut want = String::from("Farpane Win Test! ");
    want.extend((0x20u8..0x7F).map(char::from));
    want.push_str(" \u{e4}\u{f6}\u{fc}\u{df}\u{20ac}\u{263a}");
    for ch in want.chars() {
        let sym = if (ch as u32) < 0x100 {
            ch as u32
        } else {
            0x0100_0000 + ch as u32
        };
        // a client holds Shift itself for shifted symbols
        let upper = ch.is_ascii_uppercase() || "!\"\u{a7}$%&/()=?*>:;_".contains(ch);
        let _ = upper;
        input.key(sym, true);
        input.key(sym, false);
        sleep(Duration::from_millis(15));
    }
    input.key(0xFF0D, true);
    input.key(0xFF0D, false);
    sleep(Duration::from_millis(400));
    let mut buf = [0u16; 1024];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    let got = String::from_utf16_lossy(&buf[..n]);
    let want_full = format!("{want}\r\n");
    println!(
        "window holds {} chars, want {}",
        got.chars().count(),
        want_full.chars().count()
    );
    println!("typing matches: {}", got == want_full);
    if got != want_full {
        for (i, (g, w)) in got.chars().zip(want_full.chars()).enumerate() {
            if g != w {
                println!(
                    "first difference at {i}: got {g:?} (U+{:04X}) want {w:?}",
                    g as u32
                );
                break;
            }
        }
        println!("got: {got:?}");
    }
    let typed = source.capture(&mut fb).unwrap();
    println!("typed: {} tiles", typed.damage.len());
    save(&fb, &dir.join("after.ppm"));
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
    }
    println!("done");
}
