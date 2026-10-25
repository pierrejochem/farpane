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

#[allow(clippy::struct_excessive_bools)] // independent command-line switches
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
    /// Ask for H.264 (headless run; needs the `OpenH264` library).
    h264: bool,
    /// Download Cisco's `OpenH264` library and exit.
    install_h264: bool,
    /// zlib level 0 to 9 and picture quality 0 to 9 to ask the server for.
    compression: Option<u8>,
    quality: Option<u8>,
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
    let (mut h264, mut install_h264) = (false, false);
    let (mut compression, mut quality) = (None, None);
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
            "--h264" => h264 = true,
            "--install-h264" => install_h264 = true,
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
            "--compression" => compression = Some(level(args.next(), "--compression")?),
            "--quality" => quality = Some(level(args.next(), "--quality")?),
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
        h264,
        install_h264,
        compression,
        quality,
        settle,
    })
}

/// A 0 to 9 setting.
fn level(value: Option<String>, name: &str) -> Result<u8, String> {
    value
        .and_then(|v| v.parse().ok())
        .filter(|n| *n <= 9)
        .ok_or_else(|| format!("{name} needs a number from 0 to 9"))
}

fn usage() {
    eprintln!(
        "usage: farpane-viewer [--fullscreen] [--stats] [--password-file FILE] [HOST:PORT]\n       farpane-viewer --screenshot FILE.png|FILE.ppm [--encodings zrle,hextile,zlib,corre,copyrect,raw]\n                      [--settle-ms N] [--compression 0-9] [--quality 0-9] [--h264] [--username NAME] [--pin SHA256] [--password-file FILE] HOST:PORT\n\
         The password comes from $FARPANE_PASSWORD or --password-file, never from an argument."
    );
}

/// The folder Cisco's `OpenH264` library is kept in: beside the settings.
fn h264_dir() -> Option<PathBuf> {
    farpane_viewer::settings::default_path().and_then(|p| p.parent().map(farpane_h264::library_dir))
}

/// `--install-h264`: download the library from Cisco.
fn install_h264() -> ExitCode {
    let Some(dir) = h264_dir() else {
        eprintln!("farpane-viewer: there is no settings folder to keep the library in");
        return ExitCode::FAILURE;
    };
    println!("{}", farpane_h264::NOTICE);
    match farpane_h264::install(&dir) {
        Ok(path) => {
            println!("Installed {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("farpane-viewer: {e}");
            ExitCode::FAILURE
        }
    }
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
    if args.install_h264 {
        return install_h264();
    }
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
        compression: args.compression,
        quality: args.quality,
        video: if args.h264 {
            let dir = h264_dir().ok_or("no settings folder")?;
            let codec = farpane_h264::OpenH264::open(&dir)?.ok_or(
                "H.264 needs Cisco's OpenH264 library: run `farpane-viewer --install-h264`",
            )?;
            Some(std::sync::Arc::new(codec))
        } else {
            None
        },
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
