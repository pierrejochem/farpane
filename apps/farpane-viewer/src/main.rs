//! Farpane Viewer: VNC-compatible remote desktop viewer.
//!
//! ```text
//! farpane-viewer [HOST:PORT]                         open the window
//! farpane-viewer --screenshot out.png HOST:PORT      headless: save the remote screen
//! ```
//!
//! The password is never taken from the command line (it would show up in the
//! process list): set `FARPANE_PASSWORD`, or pass `--password-file FILE`.

use std::{path::PathBuf, process::ExitCode, time::Duration};

use farpane_viewer::{
    DEFAULT_ENCODINGS, RemoteScreen,
    app::{Startup, ViewerApp},
    parse_encodings, write_image,
};

struct Args {
    addr: String,
    screenshot: Option<PathBuf>,
    ui_screenshot: Option<PathBuf>,
    fullscreen: bool,
    pin_toolbar: bool,
    show_stats: bool,
    password_file: Option<PathBuf>,
    username: Option<String>,
    pins: Vec<rfb_security::tls::Fingerprint>,
    encodings: Vec<i32>,
    /// Keep applying updates for this long before saving, so a slowly drawn
    /// screen is complete.
    settle: Duration,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let (mut addr, mut screenshot, mut ui_screenshot, mut password_file) = (None, None, None, None);
    let (mut fullscreen, mut pin_toolbar, mut show_stats) = (false, false, false);
    let (mut username, mut pins) = (None, Vec::new());
    let mut settle = Duration::from_millis(1500);
    let mut encodings = DEFAULT_ENCODINGS.to_vec();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => {
                screenshot = Some(args.next().ok_or("--screenshot needs a file")?.into());
            }
            "--username" => username = Some(args.next().ok_or("--username needs a name")?),
            "--pin" => {
                let text = args.next().ok_or("--pin needs a SHA-256 fingerprint")?;
                pins.push(
                    rfb_security::tls::parse_fingerprint(&text).ok_or(
                        "--pin: not a SHA-256 fingerprint (64 hex digits, colons optional)",
                    )?,
                );
            }
            "--fullscreen" => fullscreen = true,
            "--stats" => show_stats = true,
            "--ui-pin-toolbar" => pin_toolbar = true,
            "--ui-screenshot" => {
                ui_screenshot = Some(args.next().ok_or("--ui-screenshot needs a file")?.into());
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
    if screenshot.is_some() && addr.is_none() {
        return Err("--screenshot needs host:port".into());
    }
    Ok(Args {
        addr: addr.unwrap_or_default(),
        screenshot,
        ui_screenshot,
        fullscreen,
        pin_toolbar,
        show_stats,
        password_file,
        username,
        pins,
        encodings,
        settle,
    })
}

fn usage() {
    eprintln!(
        "usage: farpane-viewer [--fullscreen] [--stats] [--password-file FILE] [HOST:PORT]\n       farpane-viewer --screenshot FILE.png|FILE.ppm [--encodings zrle,hextile,zlib,corre,copyrect,raw]\n                      [--settle-ms N] [--username NAME] [--pin SHA256] [--password-file FILE] HOST:PORT\n\
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

fn main() -> ExitCode {
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
    let result = if args.screenshot.is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(Into::into)
            .and_then(|rt| rt.block_on(run(&args)))
    } else {
        gui(&args)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("farpane-viewer: {e}");
            ExitCode::FAILURE
        }
    }
}

fn gui(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let startup = Startup {
        addr: (!args.addr.is_empty()).then(|| args.addr.clone()),
        password: password(args.password_file.as_ref())?,
        ui_screenshot: args.ui_screenshot.clone(),
        fullscreen: args.fullscreen,
        pin_toolbar: args.pin_toolbar,
        show_stats: args.show_stats,
        settings_path: farpane_viewer::settings::default_path(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Farpane Viewer")
            .with_app_id("farpane-viewer")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 420.0])
            .with_fullscreen(args.fullscreen),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Farpane Viewer",
        options,
        Box::new(move |cc| Ok(Box::new(ViewerApp::new(cc, startup)))),
    )
    .map_err(|e| e.to_string().into())
}

async fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let options = farpane_viewer::ConnectOptions {
        password: password(args.password_file.as_ref())?,
        username: args.username.clone().map(String::into_bytes),
        pins: args.pins.clone(),
        extra_roots: Vec::new(),
    };
    let mut screen = RemoteScreen::connect_opts(&args.addr, &options, &args.encodings).await?;
    let fb = screen.framebuffer();
    eprintln!(
        "connected to {:?}: {}x{}",
        screen.name(),
        fb.width(),
        fb.height()
    );
    let Some(path) = &args.screenshot else {
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
    let stats = screen.sample_stats(std::time::Instant::now());
    let mix = farpane_viewer::stats::pixel_mix(&stats, 4)
        .into_iter()
        .map(|(e, pct)| format!("{} {pct}%", farpane_viewer::stats::encoding_name(e)))
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "encodings used: {}",
        if mix.is_empty() { "none" } else { &mix }
    );
    write_image(screen.framebuffer(), path)?;
    eprintln!("saved {}", path.display());
    Ok(())
}
