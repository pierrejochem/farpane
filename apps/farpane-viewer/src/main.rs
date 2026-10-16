//! Farpane Viewer: VNC-compatible remote desktop viewer.
//!
//! The egui window is not built yet. For now the binary can connect to a
//! server and save a screenshot, which exercises the whole stack end to end:
//!
//! ```text
//! farpane-viewer --screenshot out.png host:5900
//! ```
//!
//! The password is never taken from the command line (it would show up in the
//! process list): set `FARPANE_PASSWORD`, or pass `--password-file FILE`.

use std::{path::PathBuf, process::ExitCode, time::Duration};

use farpane_viewer::{DEFAULT_ENCODINGS, RemoteScreen, parse_encodings, write_image};

struct Args {
    addr: String,
    screenshot: Option<PathBuf>,
    password_file: Option<PathBuf>,
    encodings: Vec<i32>,
    /// Keep applying updates for this long before saving, so a slowly drawn
    /// screen is complete.
    settle: Duration,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let (mut addr, mut screenshot, mut password_file) = (None, None, None);
    let mut settle = Duration::from_millis(1500);
    let mut encodings = DEFAULT_ENCODINGS.to_vec();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => {
                screenshot = Some(args.next().ok_or("--screenshot needs a file")?.into());
            }
            "--password-file" => {
                password_file = Some(args.next().ok_or("--password-file needs a file")?.into());
            }
            "--encodings" => {
                encodings = parse_encodings(&args.next().ok_or("--encodings needs a list")?)?;
            }
            "--settle-ms" => {
                let ms = args.next().ok_or("--settle-ms needs a number")?;
                settle =
                    Duration::from_millis(ms.parse().map_err(|_| "--settle-ms: not a number")?);
            }
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => addr = Some(s.to_owned()),
        }
    }
    Ok(Args {
        addr: addr.ok_or("missing host:port")?,
        screenshot,
        password_file,
        encodings,
        settle,
    })
}

fn usage() {
    eprintln!(
        "usage: farpane-viewer [--screenshot FILE.png|FILE.ppm] [--password-file FILE]\n                       [--encodings zrle,hextile,zlib,corre,copyrect,raw] [--settle-ms N] HOST:PORT\n\
         The password comes from $FARPANE_PASSWORD or --password-file, never from an argument."
    );
}

fn password(file: Option<&PathBuf>) -> Result<Option<Vec<u8>>, String> {
    if let Some(file) = file {
        let text = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(Some(text.trim_ascii_end().to_vec()));
    }
    Ok(std::env::var_os("FARPANE_PASSWORD").map(|p| p.to_string_lossy().into_owned().into_bytes()))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("farpane-viewer: {msg}");
            }
            usage();
            return ExitCode::from(2);
        }
    };
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("farpane-viewer: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut screen = RemoteScreen::connect_with(
        &args.addr,
        password(args.password_file.as_ref())?,
        &args.encodings,
    )
    .await?;
    let fb = screen.framebuffer();
    eprintln!(
        "connected to {:?}: {}x{}",
        screen.name(),
        fb.width(),
        fb.height()
    );
    let Some(path) = &args.screenshot else {
        eprintln!("the window is not built yet; use --screenshot FILE.png");
        return Ok(());
    };
    // The first update answers our full request. Then keep applying
    // incremental updates until the settle time is up (a blinking cursor
    // means the screen is never quiet, so wait for a deadline, not silence).
    screen.next_update().await?;
    let deadline = tokio::time::Instant::now() + args.settle;
    loop {
        screen.request_update(true).await?;
        // Dropping `next_update` at the deadline is fine: whatever was
        // decoded so far is already in the picture.
        match tokio::time::timeout_at(deadline, screen.next_update()).await {
            Ok(update) => {
                update?;
            }
            Err(_) => break,
        }
    }
    write_image(screen.framebuffer(), path)?;
    eprintln!("saved {}", path.display());
    Ok(())
}
