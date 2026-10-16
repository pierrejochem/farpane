//! Farpane Server: VNC-compatible remote desktop server.
//!
//! This first version has no dashboard window; it runs headless
//! (`--headless` is what `packaging/linux/farpane-server.service` passes). The
//! screen sources are the built-in test pattern and an X11 display.

use std::{net::SocketAddr, process::ExitCode, sync::Arc};

use rfb_server::{Input, NoInput, Server, Settings, Source, TestPattern};
use rfb_tokio::{ServerAuth, ServerOptions};
use tokio::net::TcpListener;

const USAGE: &str = "\
Usage: farpane-server [--headless] [--listen ADDR] [--x11 [DISPLAY]] [--size WxH]
                      [--fps N] [--password-file FILE] [--view-only]
                      [--allow-no-auth]

  --x11 [DISPLAY]        share an X11 display (default $DISPLAY) instead of
                         the test pattern; under XWayland only X11 windows
                         are visible, not the Wayland desktop

  --listen ADDR          address to listen on (default 127.0.0.1:5900)
  --size WxH             test pattern size (ignored with --x11) (default 1024x768)
  --password-file FILE   VNC password (first line); $FARPANE_PASSWORD also works
  --allow-no-auth        accept connections without a password on a
                         non-loopback address (a bad idea without a tunnel)
  --view-only            ignore keyboard, pointer and clipboard input

The password is never taken from the command line.";

struct Args {
    listen: SocketAddr,
    size: (u16, u16),
    fps: u32,
    password: Option<Vec<u8>>,
    view_only: bool,
    allow_no_auth: bool,
    /// Share an X11 display instead of the test pattern.
    x11: bool,
    /// Which one; `None` is `$DISPLAY`.
    x11_display: Option<String>,
}

type Words = std::iter::Peekable<std::iter::Skip<std::env::Args>>;

fn value(it: &mut Words, name: &str) -> Result<String, String> {
    it.next().ok_or(format!("{name} needs a value"))
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        listen: "127.0.0.1:5900"
            .parse()
            .map_err(|_| "bad default address")?,
        size: (1024, 768),
        fps: 30,
        password: std::env::var("FARPANE_PASSWORD")
            .ok()
            .filter(|p| !p.is_empty())
            .map(String::into_bytes),
        view_only: false,
        allow_no_auth: false,
        x11: false,
        x11_display: None,
    };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--headless" => {}
            "--listen" => {
                args.listen = value(&mut it, "--listen")?
                    .parse()
                    .map_err(|e| format!("--listen: {e}"))?;
            }
            "--size" => {
                let v = value(&mut it, "--size")?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH")?;
                args.size = (
                    w.parse().map_err(|_| "--size: bad width")?,
                    h.parse().map_err(|_| "--size: bad height")?,
                );
            }
            "--fps" => {
                args.fps = value(&mut it, "--fps")?
                    .parse()
                    .map_err(|_| "--fps: not a number")?;
            }
            "--password-file" => {
                let path = value(&mut it, "--password-file")?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                let line = text.lines().next().unwrap_or("").trim_end_matches('\r');
                args.password = Some(line.as_bytes().to_vec()).filter(|p| !p.is_empty());
            }
            "--x11" => {
                // The display is optional: take the next word only if it
                // looks like one (":0", "host:1").
                let display = it
                    .peek()
                    .filter(|d| d.contains(':') && !d.starts_with('-'))
                    .cloned();
                if display.is_some() {
                    it.next();
                }
                args.x11 = true;
                args.x11_display = display;
            }
            "--view-only" => args.view_only = true,
            "--allow-no-auth" => args.allow_no_auth = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            if e.is_empty() {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            eprintln!("farpane-server: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.password.is_none() && !args.listen.ip().is_loopback() && !args.allow_no_auth {
        eprintln!(
            "farpane-server: refusing to listen on {} without a password. Set one with \
             --password-file or $FARPANE_PASSWORD, or pass --allow-no-auth.",
            args.listen
        );
        return ExitCode::from(2);
    }
    let auth = match &args.password {
        // VNC Auth uses only the first 8 characters; say so rather than
        // silently accept a longer secret that is not checked.
        Some(p) => {
            if p.len() > 8 {
                eprintln!(
                    "farpane-server: VNC password authentication checks only the first 8 \
                     characters; the rest is ignored."
                );
            }
            ServerAuth::VncPassword(p.clone())
        }
        None => ServerAuth::None,
    };
    let template = ServerOptions::new(auth, 1, 1, b"Farpane");
    let settings = Settings {
        fps: args.fps,
        view_only: args.view_only,
    };
    let (source, input, what): (Box<dyn Source>, Box<dyn Input>, String) =
        match args.x11.then_some(&args.x11_display) {
            Some(display) => match farpane_x11::open(display.as_deref()) {
                Ok((source, input)) => {
                    let (w, h) = source.size();
                    (
                        Box::new(source),
                        Box::new(input),
                        format!("X11 {}, {w}x{h}", display.as_deref().unwrap_or("$DISPLAY")),
                    )
                }
                Err(e) => {
                    eprintln!("farpane-server: cannot open the X11 display: {e}");
                    return ExitCode::FAILURE;
                }
            },
            None => (
                Box::new(TestPattern::new(args.size.0, args.size.1)),
                Box::new(NoInput),
                format!("test pattern, {}x{}", args.size.0, args.size.1),
            ),
        };
    let server = match Server::new(source, input, template, settings) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("farpane-server: cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(args.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("farpane-server: cannot listen on {}: {e}", args.listen);
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "Farpane Server {} listening on {} ({what}, {})",
        env!("CARGO_PKG_VERSION"),
        args.listen,
        if args.password.is_some() {
            "password"
        } else {
            "no authentication"
        },
    );
    tokio::select! {
        result = Arc::clone(&server).listen(listener, |peer, e| {
            eprintln!("farpane-server: {peer}: {e}");
        }) => {
            if let Err(e) = result {
                eprintln!("farpane-server: {e}");
                return ExitCode::FAILURE;
            }
        }
        _ = tokio::signal::ctrl_c() => {}
    }
    ExitCode::SUCCESS
}
