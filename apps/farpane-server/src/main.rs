//! Farpane Server: VNC-compatible remote desktop server.
//!
//! Opens the dashboard window unless `--headless` is given (which is what
//! `packaging/linux/farpane-server.service` passes). Screen sources are the
//! built-in test pattern and an X11 display.

use std::{
    io::{BufRead, IsTerminal},
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use farpane_server::{
    backend::{EventLog, Level, LiveBackend},
    config::{self, TlsChoice},
    dashboard::{Dashboard, Page},
    usercmd,
};
use rfb_security::tls::format_fingerprint;
use rfb_server::{Event, Input, NoInput, Permission, Server, Settings, Source, TestPattern};
use tokio::net::TcpListener;

const USAGE: &str = "\
Usage: farpane-server [--headless] [--listen ADDR] [--x11 [DISPLAY]] [--size WxH]
                      [--fps N] [--config-dir DIR] [--view-only]
                      [--tls-self-signed [--tls-name NAME]... | --tls-cert FILE --tls-key FILE] [--ra2]
                      [--password-file FILE] [--allow-no-auth]
       farpane-server user COMMAND ...     (see `farpane-server user --help`)

  --listen ADDR          address to listen on (default 127.0.0.1:5900)
  --x11 [DISPLAY]        share an X11 display (default $DISPLAY) instead of the
                         test pattern; under XWayland only X11 windows are
                         visible, not the Wayland desktop
  --wayland              share the Wayland desktop through the portal (the desktop
                         asks for approval; remembered in the config folder)
  --size WxH             test pattern size (default 1024x768)
  --config-dir DIR       holds users.toml and tls/ (default /etc/farpane if it
                         exists, else ~/.config/farpane; $FARPANE_CONFIG_DIR)
  --tls-self-signed      encrypt with a self-signed certificate kept in the
                         config folder; its fingerprint is printed at start
  --tls-name NAME        extra name for that certificate (repeatable)
  --ra2                  also offer RSA-AES (RA2_256 and RA2) with a key kept in the
                         config folder; needs users; its fingerprint is printed
  --tls-cert/--tls-key   encrypt with your own PEM certificate and key
  --password-file FILE   shared VNC password (first line), when there are no
                         users; $FARPANE_PASSWORD also works
  --allow-no-auth        accept connections without credentials on a
                         non-loopback address (a bad idea without a tunnel)
  --view-only            ignore keyboard, pointer and clipboard input
  --ui-screenshot FILE   save a picture of the dashboard and quit
  --ui-delay SECONDS     wait this long before that picture (default 1.5)
  --page NAME            start on overview, clients, users or log

Users (farpane-server user add NAME) sign in with name and password inside
TLS and have a role: admin, control or view. Passwords are never taken from
the command line.";

#[allow(clippy::struct_excessive_bools)] // independent command-line switches
#[derive(Clone)]
struct Args {
    listen: SocketAddr,
    size: (u16, u16),
    fps: u32,
    password: Option<Vec<u8>>,
    view_only: bool,
    allow_no_auth: bool,
    x11: bool,
    wayland: bool,
    x11_display: Option<String>,
    config_dir: Option<PathBuf>,
    self_signed: bool,
    ra2: bool,
    headless: bool,
    ui_screenshot: Option<PathBuf>,
    page: Page,
    ui_delay: f64,
    tls_names: Vec<String>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
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
        wayland: false,
        x11_display: None,
        config_dir: None,
        self_signed: false,
        ra2: false,
        headless: false,
        ui_screenshot: None,
        page: Page::Overview,
        ui_delay: 1.5,
        tls_names: Vec::new(),
        tls_cert: None,
        tls_key: None,
    };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--headless" => args.headless = true,
            "--ui-screenshot" => {
                args.ui_screenshot = Some(value(&mut it, "--ui-screenshot")?.into());
            }
            "--ui-delay" => {
                args.ui_delay = value(&mut it, "--ui-delay")?
                    .parse()
                    .map_err(|_| "--ui-delay: seconds")?;
            }
            "--page" => {
                let name = value(&mut it, "--page")?;
                args.page = Page::parse(&name).ok_or("--page: overview, clients, users or log")?;
            }
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
                args.password = read_password_file(&path)?;
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
            "--wayland" => args.wayland = true,
            "--config-dir" => args.config_dir = Some(value(&mut it, "--config-dir")?.into()),
            "--tls-self-signed" => args.self_signed = true,
            "--ra2" => args.ra2 = true,
            "--tls-name" => args.tls_names.push(value(&mut it, "--tls-name")?),
            "--tls-cert" => args.tls_cert = Some(value(&mut it, "--tls-cert")?.into()),
            "--tls-key" => args.tls_key = Some(value(&mut it, "--tls-key")?.into()),
            "--view-only" => args.view_only = true,
            "--allow-no-auth" => args.allow_no_auth = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(args)
}

fn read_password_file(path: &str) -> Result<Option<Vec<u8>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let line = text.lines().next().unwrap_or("").trim_end_matches('\r');
    Ok(Some(line.as_bytes().to_vec()).filter(|p| !p.is_empty()))
}

/// The password for a `user` command: `$FARPANE_PASSWORD`, `--password-file`,
/// or one line from standard input.
fn user_password(file: Option<&str>) -> std::io::Result<String> {
    if let Some(path) = file {
        return read_password_file(path)
            .map_err(std::io::Error::other)?
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .ok_or_else(|| std::io::Error::other("the password file is empty"));
    }
    if let Ok(p) = std::env::var("FARPANE_PASSWORD") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("Password (typed text is visible; prefer --password-file or $FARPANE_PASSWORD):");
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn log(line: &str) {
    eprintln!("farpane-server: {line}");
}

fn describe(event: &Event) -> String {
    let who =
        |peer: &Option<SocketAddr>| peer.map_or_else(|| "local".to_owned(), |p| p.to_string());
    match event {
        Event::Connected {
            peer,
            user,
            permission,
        } => format!(
            "{} connected{}{}",
            who(peer),
            user.as_ref()
                .map_or_else(String::new, |u| format!(" as {u:?}")),
            if *permission == Permission::ViewOnly {
                " (view only)"
            } else {
                ""
            }
        ),
        Event::Closed { peer, user, error } => format!(
            "{} disconnected{}{}",
            who(peer),
            user.as_ref()
                .map_or_else(String::new, |u| format!(" ({u:?})")),
            error
                .as_ref()
                .map_or_else(String::new, |e| format!(": {e}"))
        ),
        Event::AuthFailed { peer } => format!("{} failed to sign in", who(peer)),
        Event::HandshakeFailed { peer, error } => {
            format!("{} handshake failed: {error}", who(peer))
        }
    }
}

fn tls_choice(args: &Args) -> Result<TlsChoice, String> {
    match (&args.tls_cert, &args.tls_key, args.self_signed) {
        (Some(cert), Some(key), false) => Ok(TlsChoice::Files {
            cert: cert.clone(),
            key: key.clone(),
        }),
        (None, None, true) => {
            let mut names = args.tls_names.clone();
            names.push("localhost".to_owned());
            if let Ok(host) = std::env::var("HOSTNAME") {
                names.push(host);
            }
            names.sort();
            names.dedup();
            Ok(TlsChoice::SelfSigned { names })
        }
        (None, None, false) => Ok(TlsChoice::Off),
        _ => Err("use either --tls-self-signed or --tls-cert with --tls-key".to_owned()),
    }
}

type SourceAndInput = (Box<dyn Source>, Box<dyn Input>, String);

#[cfg(target_os = "linux")]
fn open_wayland(config_dir: &std::path::Path) -> Result<SourceAndInput, String> {
    eprintln!(
        "farpane-server: asking the desktop for permission to share the screen; \
         answer the dialog that appears (up to 3 minutes)"
    );
    let (source, input) = farpane_wayland::open(&farpane_wayland::Config {
        restore_token_file: Some(config_dir.join("wayland-restore-token")),
    })
    .map_err(|e| format!("cannot share the Wayland screen: {e}"))?;
    let (w, h) = source.size();
    Ok((
        Box::new(source),
        Box::new(input),
        format!("Wayland screen, {w}x{h}"),
    ))
}

#[cfg(not(target_os = "linux"))]
fn open_wayland(_config_dir: &std::path::Path) -> Result<SourceAndInput, String> {
    Err("--wayland is only available on Linux".to_owned())
}

fn open_source(args: &Args, config_dir: &std::path::Path) -> Result<SourceAndInput, String> {
    if args.wayland {
        open_wayland(config_dir)
    } else if args.x11 {
        let (source, input) = farpane_x11::open(args.x11_display.as_deref())
            .map_err(|e| format!("cannot open the X11 display: {e}"))?;
        let (w, h) = source.size();
        Ok((
            Box::new(source),
            Box::new(input),
            format!(
                "X11 {}, {w}x{h}",
                args.x11_display.as_deref().unwrap_or("$DISPLAY")
            ),
        ))
    } else {
        Ok((
            Box::new(TestPattern::new(args.size.0, args.size.1)),
            Box::new(NoInput),
            format!("test pattern, {}x{}", args.size.0, args.size.1),
        ))
    }
}

/// Print a line and keep it for the dashboard's event log.
fn note(events: &EventLog, level: Level, text: &str) {
    log(text);
    events.push(level, text);
}

/// Everything that is running once start-up is done.
struct Running {
    server: Arc<Server>,
    listener: TcpListener,
    listen: String,
    source: String,
    access: String,
    fingerprints: Vec<(String, String)>,
    events: EventLog,
    config_dir: PathBuf,
}

/// Read the settings, open the screen and the port. The error is the exit
/// code to give.
#[allow(clippy::too_many_lines)] // straight-line start-up
async fn start(args: &Args) -> Result<Running, ExitCode> {
    let config_dir = args
        .config_dir
        .clone()
        .unwrap_or_else(config::default_config_dir);
    let tls = tls_choice(args).map_err(|e| {
        eprintln!("farpane-server: {e}");
        ExitCode::from(2)
    })?;
    let events = EventLog::new();
    let sink = events.clone();
    let access = config::choose(
        &config_dir,
        args.password.clone(),
        &tls,
        args.ra2,
        move |l| {
            note(&sink, Level::Warning, &l);
        },
    )
    .map_err(|e| {
        eprintln!("farpane-server: {e}");
        ExitCode::from(2)
    })?;
    let authenticated = access.authenticator.is_some() || args.password.is_some();
    if !authenticated && !args.listen.ip().is_loopback() && !args.allow_no_auth {
        eprintln!(
            "farpane-server: refusing to listen on {} without credentials. Add a user \
             (`farpane-server user add NAME`), set a password with --password-file or \
             $FARPANE_PASSWORD, or pass --allow-no-auth.",
            args.listen
        );
        return Err(ExitCode::from(2));
    }
    if access.identity.is_none() && authenticated && !args.listen.ip().is_loopback() {
        note(
            &events,
            Level::Warning,
            "warning: no TLS; the connection is not encrypted (use --tls-self-signed)",
        );
    }
    if let Some(p) = &args.password {
        if access.authenticator.is_none() && p.len() > 8 {
            note(
                &events,
                Level::Warning,
                "warning: VNC password authentication checks only the first 8 characters",
            );
        }
    }
    // Opening a screen may wait for the user (the Wayland approval dialog), so
    // keep it off the async threads.
    let (a, dir) = (args.clone(), config_dir.clone());
    let (source, input, what) = tokio::task::spawn_blocking(move || open_source(&a, &dir))
        .await
        .map_err(|e| {
            eprintln!("farpane-server: {e}");
            ExitCode::FAILURE
        })?
        .map_err(|e| {
            eprintln!("farpane-server: {e}");
            ExitCode::FAILURE
        })?;
    let settings = Settings {
        fps: args.fps,
        view_only: args.view_only,
    };
    let mut server =
        Server::new(source, input, access.template.clone(), settings).map_err(|e| {
            eprintln!("farpane-server: cannot start: {e}");
            ExitCode::FAILURE
        })?;
    if let Some(auth) = access.authenticator.clone() {
        server.set_policy(move |user| auth.permission(user));
    }
    let sink = events.clone();
    server.set_events(move |e| {
        let level = match e {
            Event::AuthFailed { .. } | Event::HandshakeFailed { .. } => Level::Warning,
            _ => Level::Info,
        };
        note(&sink, level, &describe(e));
    });
    let listener = TcpListener::bind(args.listen).await.map_err(|e| {
        eprintln!("farpane-server: cannot listen on {}: {e}", args.listen);
        ExitCode::FAILURE
    })?;
    note(
        &events,
        Level::Info,
        &format!(
            "Farpane Server {} listening on {} ({what}; {})",
            env!("CARGO_PKG_VERSION"),
            args.listen,
            access.description
        ),
    );
    let mut fingerprints = Vec::new();
    if let Some(fp) = &access.ra2_fingerprint {
        let text = format_fingerprint(fp);
        note(&events, Level::Info, &format!("RSA-AES key SHA-256 {text}"));
        fingerprints.push(("RSA-AES key".to_owned(), text));
    }
    if let Some(id) = &access.identity {
        let text = format_fingerprint(&id.fingerprint);
        note(&events, Level::Info, &format!("certificate SHA-256 {text}"));
        fingerprints.push(("Certificate".to_owned(), text));
    }
    Ok(Running {
        server: Arc::new(server),
        listener,
        listen: args.listen.to_string(),
        source: what,
        access: access.description.clone(),
        fingerprints,
        events,
        config_dir,
    })
}

fn main() -> ExitCode {
    let mut raw = std::env::args().skip(1);
    if raw.next().as_deref() == Some("user") {
        return user_command();
    }
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
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("farpane-server: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let running = match rt.block_on(start(&args)) {
        Ok(r) => r,
        Err(code) => return code,
    };
    if args.headless {
        return rt.block_on(async {
            tokio::select! {
                result = Arc::clone(&running.server).listen(running.listener) => {
                    if let Err(e) = result {
                        eprintln!("farpane-server: {e}");
                        return ExitCode::FAILURE;
                    }
                }
                _ = tokio::signal::ctrl_c() => {}
            }
            ExitCode::SUCCESS
        });
    }
    // The window on this thread, the server on the runtime's.
    let Running {
        server,
        listener,
        listen,
        source,
        access,
        fingerprints,
        events,
        config_dir,
    } = running;
    rt.spawn(Arc::clone(&server).listen(listener));
    let backend = Arc::new(LiveBackend {
        server,
        listen,
        source,
        access,
        fingerprints,
    });
    let mut dashboard = Dashboard::new(backend, events, config_dir.join("users.toml"), config_dir);
    dashboard.set_screenshot(args.ui_screenshot.clone(), args.page, args.ui_delay);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Farpane Server")
            .with_app_id("farpane-server")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([760.0, 480.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    match eframe::run_native(
        "Farpane Server",
        options,
        Box::new(move |_cc| Ok(Box::new(dashboard))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!(
                "farpane-server: cannot open the window: {e}. Run with --headless on a machine \
                 without a display."
            );
            ExitCode::FAILURE
        }
    }
}

fn user_command() -> ExitCode {
    let mut words: Vec<String> = std::env::args().skip(2).collect();
    let mut take = |flag: &str| -> Option<String> {
        let i = words.iter().position(|w| w == flag)?;
        words.remove(i);
        (i < words.len()).then(|| words.remove(i))
    };
    let config_dir = take("--config-dir").map_or_else(config::default_config_dir, PathBuf::from);
    let password_file = take("--password-file");
    let mut out = std::io::stdout();
    let result = usercmd::run(
        &words,
        &config_dir.join("users.toml"),
        &mut || user_password(password_file.as_deref()),
        &mut out,
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("farpane-server: {e}");
            ExitCode::from(2)
        }
    }
}
